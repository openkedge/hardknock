// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    agent::{AgentAdapter, GenericShellAdapter},
    bridge::config::Config,
    budget::{
        BudgetDecision, ExperienceBudget, ExperienceBudgetPolicy, ExperienceUsage,
        StrictBudgetPolicy,
    },
    cancellation::Cancellation,
    core::{AgentIdentity, CommandSpec, EnvironmentMode, ExperimentId, ForkReason, RealityStatus},
    dojo::{GitRealityProvider, RealityProvider},
    experience::{EnvironmentContext, ReplaySpec},
    store::{EffectStore, ExperimentStore, Store, artifact},
    workflow::{PreparedTrial, RunRequest, run_prepared_trial},
};
use chrono::Utc;
use fs2::FileExt;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::task::JoinSet;

pub struct ExperimentOrchestrator<'a> {
    pub store: &'a Store,
    pub config: &'a Config,
}

#[derive(Clone)]
struct ResolvedCandidate {
    candidate: ExperimentCandidate,
    agent: AgentIdentity,
    command: CommandSpec,
    commands: Option<Vec<CommandSpec>>,
    template: String,
}

struct PreparedBatch<'a> {
    store: &'a Store,
    trials: VecDeque<PreparedTrial>,
}
impl Drop for PreparedBatch<'_> {
    fn drop(&mut self) {
        for trial in &mut self.trials {
            if let Err(error) = GitRealityProvider::new(self.store).discard(&mut trial.reality) {
                tracing::error!(%error, reality = %trial.reality.id, "Could not discard unexecuted candidate");
            }
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}
fn hash(value: &impl serde::Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn lock(home: &Path, name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("locks").join(name))?;
    FileExt::try_lock_exclusive(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Error::Intervention("Experiment or provider capacity is already in use".into())
        } else {
            Error::Io(e)
        }
    })?;
    Ok(file)
}

impl ExperimentOrchestrator<'_> {
    pub fn submit(&self, request: ExperimentRequest) -> Result<StrategyExperiment> {
        self.config
            .experiments
            .validate(&self.config.experience_budget)?;
        if let Some(existing) = self.store.experiment_for_request(&request.id)? {
            if serde_json::to_value(&existing.request)? != serde_json::to_value(&request)? {
                return Err(invalid("Request ID reused with different contents"));
            }
            return Ok(existing);
        }
        self.validate_shape(&request)?;
        let effective_budget = self.effective_budget(&request);
        let mut limited = request.clone();
        limited.budget = effective_budget.clone();
        let mut experiment = StrategyExperiment { id: ExperimentId::new(), request, effective_budget, status: ExperimentStatus::Accepted, result: None, failure: None, notices: vec!["git-worktree: repository copies only; no host, process, credential, network, or external-effect isolation. Trusted local commands only. Recommendation never applies changes.".into()] };
        let approval = self.validate_scope(&experiment.request).and_then(|()| {
            match StrictBudgetPolicy.evaluate(&limited, &ExperienceUsage::default()) {
                BudgetDecision::Approved => Ok(()),
                BudgetDecision::Rejected(reason) => Err(invalid(reason)),
                BudgetDecision::Reduced(_) => Err(invalid(
                    "Strict comparison policy does not reduce candidate sets",
                )),
            }
        });
        if let Err(error) = approval {
            experiment.status = ExperimentStatus::Rejected;
            experiment.failure = Some(error.to_string());
        }
        if experiment.request.budget != experiment.effective_budget {
            experiment.notices.push("Budget ceilings were clamped to local configuration; the candidate set was not reduced.".into());
        }
        if experiment.request.starting_state.source == SnapshotSource::SessionCommitFallback {
            experiment.notices.push("Session snapshot unavailable: using the session's recorded repository commit. Uncommitted/ignored files and live agent/process state are not reproduced.".into());
        }
        ExperimentStore::insert(self.store, &experiment)?;
        Ok(experiment)
    }

    pub async fn run(
        &self,
        request: ExperimentRequest,
        cancel: &Cancellation,
    ) -> Result<StrategyExperiment> {
        let experiment = self.submit(request)?;
        self.execute(&experiment.id, cancel).await
    }

    /// Resumes only accepted requests. Running requests are never replayed after a crash.
    pub async fn execute(
        &self,
        id: &ExperimentId,
        external: &Cancellation,
    ) -> Result<StrategyExperiment> {
        let _lease = lock(&self.store.home, &format!("{id}.lock"))?;
        let mut experiment = self.store.strategy_experiment(id)?;
        if experiment.status.terminal() {
            return Ok(experiment);
        }
        if experiment.status == ExperimentStatus::Running {
            return Err(invalid(
                "Experiment was interrupted; use replay to create new evidence, not resume",
            ));
        }
        experiment.status = ExperimentStatus::Running;
        ExperimentStore::update_status(self.store, &experiment)?;
        let cancel = Cancellation::default();
        if external.is_cancelled() || self.store.experiment_cancel_requested(id)? {
            cancel.cancel();
        }
        let start = Instant::now();
        let duration = experiment
            .effective_budget
            .max_duration_ms
            .unwrap_or(300_000);
        let mut timer = tokio::time::interval(Duration::from_millis(25));
        let mut cancel_reason = cancel
            .is_cancelled()
            .then(|| "Cancelled before execution".to_owned());
        let outcome = {
            let work = self.execute_inner(&experiment, &cancel);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    _ = timer.tick() => {
                        if start.elapsed().as_millis() >= u128::from(duration) {
                            cancel_reason.get_or_insert("Experience duration budget exhausted".to_owned()); cancel.cancel();
                        }
                        match self.store.experiment_cancel_requested(id) {
                            Ok(true) => { cancel_reason.get_or_insert("Explicit experiment cancellation".into()); cancel.cancel(); }
                            Ok(false) => {},
                            Err(error) => { cancel_reason.get_or_insert(format!("Cancellation monitor failed: {error}")); cancel.cancel(); }
                        }
                        if external.is_cancelled() { cancel_reason.get_or_insert("Requesting session or process ended".into()); cancel.cancel(); }
                    }
                }
            }
        };
        let mut result = match outcome {
            Ok(result) => result,
            Err(error) => {
                experiment.failure = Some(error.to_string());
                let candidates = self.store.candidate_results(id)?;
                ExperimentResult {
                    experiment_id: id.clone(),
                    question: experiment.request.question.clone(),
                    created_experience: candidates
                        .iter()
                        .map(|c| c.experience_id.clone())
                        .collect(),
                    candidates,
                    comparison: ExperimentComparison {
                        policy: "evaluator-success-first".into(),
                        recommendation: None,
                        reasons: vec![error.to_string()],
                        evidence_weight: "invalid".into(),
                    },
                    recommendation: None,
                    confidence: None,
                    candidate_lessons: vec![],
                    quality: ExperimentQuality::Invalid,
                    changed_variables: vec![],
                    starting_state: None,
                    usage: ExperienceUsage::default(),
                }
            }
        };
        result.usage.duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        result.usage.realities = self
            .store
            .realities()?
            .iter()
            .filter(|r| r.experiment_id.as_ref() == Some(id))
            .count();
        let mut execution_was_scheduled = false;
        if experiment.failure.is_some() {
            let launched: BTreeSet<_> = self
                .store
                .experiment_progress(id, 0)?
                .into_iter()
                .filter_map(|(_, p)| {
                    (p.phase == ExperimentPhase::Executing)
                        .then_some(p.candidate)
                        .flatten()
                        .map(|id| id.to_string())
                })
                .collect();
            execution_was_scheduled = !launched.is_empty();
            result.usage.agent_runs = experiment
                .request
                .candidates
                .iter()
                .filter(|c| {
                    launched.contains(&c.id.to_string())
                        && matches!(c.execution, CandidateExecution::AgentTask { .. })
                })
                .count();
            result.usage.commands = result
                .candidates
                .iter()
                .map(|c| {
                    self.store
                        .experience(&c.experience_id)
                        .map(|e| e.actions.len())
                })
                .collect::<Result<Vec<_>>>()?
                .iter()
                .sum();
            result.usage.tool_calls = (result.usage.agent_runs == 0).then_some(0);
        }
        experiment.status =
            if cancel_reason.is_some() || (cancel.is_cancelled() && experiment.failure.is_none()) {
                result.recommendation = None;
                result.comparison.recommendation = None;
                result
                    .comparison
                    .reasons
                    .push("Comparison interrupted; partial evidence is not a winner".into());
                experiment.failure =
                    cancel_reason.or_else(|| Some("Cancelled before execution".into()));
                ExperimentStatus::Cancelled
            } else if experiment.failure.is_some() {
                if result.candidates.is_empty() && !execution_was_scheduled {
                    ExperimentStatus::Rejected
                } else {
                    ExperimentStatus::Failed
                }
            } else {
                ExperimentStatus::Completed
            };
        // No new causal hypothesis is generated from a partial or confounded comparison.
        if experiment.status == ExperimentStatus::Completed
            && result.quality == ExperimentQuality::Controlled
            && experiment.request.origin != ExperimentOrigin::CausalInvestigation
        {
            match self.reflect(&result) {
                Ok(lessons) => result.candidate_lessons = lessons,
                Err(error) => {
                    experiment.status = ExperimentStatus::Failed;
                    experiment.failure = Some(format!(
                        "Evidence recorded, but Candidate Lesson recording failed: {error}"
                    ));
                }
            }
        }
        let progress = ExperimentProgress {
            experiment_id: id.clone(),
            candidate: None,
            phase: if experiment.status == ExperimentStatus::Cancelled {
                ExperimentPhase::Cancelled
            } else {
                ExperimentPhase::Completed
            },
            message: format!(
                "Experiment {:?}; no changes applied to source",
                experiment.status
            ),
            created_at: Utc::now(),
        };
        experiment.result = Some(result);
        self.store
            .finish_strategy_experiment(&experiment, &progress)?;
        Ok(experiment)
    }

    fn validate_shape(&self, request: &ExperimentRequest) -> Result<()> {
        if request.question.trim().is_empty()
            || request.question.len() > 4096
            || request.session_id.is_empty()
            || request.session_id.len() > 256
            || request.candidates.is_empty()
            || request.candidates.len() > 32
            || serde_json::to_vec(request)?.len() > 128 * 1024
        {
            return Err(invalid(
                "Experiment needs a question, session, and 1–32 bounded candidates (128 KiB maximum)",
            ));
        }
        request.evaluator.validate()?;
        if request.evaluator.checks.len() > 16 || !request.criteria.custom_checks.is_empty() {
            return Err(invalid(
                "At most 16 evaluator checks; custom comparison checks are not supported",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for c in &request.candidates {
            if c.name.is_empty()
                || c.name.len() > 64
                || !c
                    .name
                    .chars()
                    .all(|x| x.is_alphanumeric() || "-_".contains(x))
                || !ids.insert(c.id.to_string())
                || !names.insert(c.name.clone())
            {
                return Err(invalid(
                    "Candidate IDs/names must be unique; names use letters, numbers, hyphens or underscores",
                ));
            }
            let values = match &c.execution {
                CandidateExecution::Shell { commands } => {
                    if commands.is_empty() || commands.len() > 64 {
                        return Err(invalid(
                            "Shell candidate needs 1–64 explicit command entries",
                        ));
                    }
                    commands.clone()
                }
                CandidateExecution::AgentTask { prompt, .. } => vec![prompt.clone()],
                CandidateExecution::EffectPlan {
                    effects,
                    simulation,
                } => {
                    if effects.is_empty() || effects.len() > 32 || simulation.len() > 64 {
                        return Err(invalid(
                            "Effect-plan candidate needs 1–32 effects and at most 64 simulation steps",
                        ));
                    }
                    for effect in effects {
                        effect.validate()?;
                    }
                    effects
                        .iter()
                        .map(|effect| effect.target.uri.clone())
                        .chain(simulation.iter().cloned())
                        .collect()
                }
            };
            if values
                .iter()
                .any(|s| s.trim().is_empty() || s.contains('\0') || s.len() > 32 * 1024)
            {
                return Err(invalid(
                    "Candidate commands/prompts must be nonempty, NUL-free, and bounded",
                ));
            }
        }
        Ok(())
    }

    fn validate_scope(&self, request: &ExperimentRequest) -> Result<()> {
        let capabilities = &request.capabilities;
        let has_effect_plan = request
            .candidates
            .iter()
            .any(|candidate| matches!(candidate.execution, CandidateExecution::EffectPlan { .. }));
        if !capabilities.filesystem_scope.is_empty()
            || capabilities.allow_external_mutations
            || (!capabilities.external_effects.is_empty() && !has_effect_plan)
        {
            return Err(invalid(
                "Experiment requires unsupported external side effects or filesystem scope; only repository-local work is supported",
            ));
        }
        if request.origin == ExperimentOrigin::Agent
            && (!self.config.experiments.agent_requests.enabled
                || (capabilities.allow_network
                    && !self.config.experiments.agent_requests.allow_network))
        {
            return Err(invalid(
                "Agent experiment requests or requested network capability are disabled",
            ));
        }
        if !matches!(
            request.intent,
            ExperimentIntent::ResolveUncertainty
                | ExperimentIntent::CompareStrategies
                | ExperimentIntent::ValidateHypothesis
                | ExperimentIntent::ReproduceFederatedExperience
        ) {
            return Err(invalid(
                "This experiment intent requires the existing lesson/chaos/recovery engine; strategy execution does not substitute for it",
            ));
        }
        // Obvious declared shell mutations are rejected, but this is not a shell sandbox.
        let forbidden = regex::Regex::new(r"(?i)(\bgit\s+push\b|\b(sendmail|aws|gcloud|az|terraform)\b|\bsend[ -]email\b|\bcharge[ -](card|api)\b)").map_err(|e| invalid(e.to_string()))?;
        for text in request
            .candidates
            .iter()
            .flat_map(|c| match &c.execution {
                CandidateExecution::Shell { commands } => {
                    commands.iter().map(String::as_str).collect::<Vec<_>>()
                }
                CandidateExecution::AgentTask { prompt, .. } => vec![prompt.as_str()],
                CandidateExecution::EffectPlan { .. } => Vec::new(),
            })
            .chain(request.evaluator.checks.iter().map(String::as_str))
        {
            if forbidden.is_match(text) {
                return Err(invalid(
                    "Experiment requires unsupported external side effects; external mutations cannot be isolated",
                ));
            }
            let policy = &self.config.bridge.policy;
            if policy
                .blocked_shell_commands
                .iter()
                .chain(&policy.approval_shell_commands)
                .any(|command| command.trim() == text.trim())
            {
                return Err(invalid(
                    "Experiment requires a command blocked by local policy or explicit approval; unattended execution cannot grant approval",
                ));
            }
        }
        Ok(())
    }

    fn effective_budget(&self, request: &ExperimentRequest) -> ExperienceBudget {
        let configured = self.config.experience_budget.budget();
        let mut b = request.budget.clone();
        b.max_realities = b.max_realities.min(configured.max_realities);
        if request.origin == ExperimentOrigin::Agent {
            b.max_realities = b
                .max_realities
                .min(self.config.experiments.agent_requests.max_realities);
        }
        b.max_agent_runs = b.max_agent_runs.min(configured.max_agent_runs);
        b.max_duration_ms = Some(
            b.max_duration_ms
                .unwrap_or(u64::MAX)
                .min(configured.max_duration_ms.unwrap_or(300_000)),
        );
        b.max_commands_per_reality = match (
            b.max_commands_per_reality,
            configured.max_commands_per_reality,
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        b
    }

    fn resolve(
        &self,
        request: &ExperimentRequest,
        candidate: &ExperimentCandidate,
    ) -> Result<ResolvedCandidate> {
        match &candidate.execution {
            CandidateExecution::Shell { commands } => Ok(ResolvedCandidate {
                candidate: candidate.clone(),
                command: CommandSpec::shell(&commands[0], EnvironmentMode::Controlled),
                commands: Some(
                    commands
                        .iter()
                        .map(|s| CommandSpec::shell(s, EnvironmentMode::Controlled))
                        .collect(),
                ),
                agent: AgentIdentity {
                    kind: "shell".into(),
                    executable: "/bin/sh".into(),
                    version: None,
                    model: None,
                },
                template: "/bin/sh -c {command}".into(),
            }),
            CandidateExecution::AgentTask { prompt, agent } => {
                let identity = agent.as_ref().unwrap_or(&request.requested_by);
                let fixture = matches!(
                    identity.kind.as_str(),
                    "test-agent" | "fake-agent-A" | "fake-agent-B"
                );
                if fixture && identity.model.is_some() {
                    return Err(invalid(
                        "The deterministic fixture agent does not accept model selection",
                    ));
                }
                let config = self
                    .config
                    .experiments
                    .agents
                    .get(&identity.kind)
                    .cloned()
                    .or_else(|| match identity.kind.as_str() {
                        "codex" => Some(ExperimentAgentConfig {
                            command: "codex exec -- {task}".into(),
                            environment: EnvironmentMode::Inherited,
                            version: None,
                            model: None,
                        }),
                        _ => None,
                    });
                let (mut command, resolved, template) = if fixture {
                    (
                        CommandSpec {
                            program: "/bin/sh".into(),
                            args: vec![
                                "./agent-script.sh".into(),
                                prompt.clone(),
                                identity.kind.clone(),
                            ],
                            environment: EnvironmentMode::Controlled,
                            environment_overrides: BTreeMap::new(),
                        },
                        AgentIdentity {
                            kind: identity.kind.clone(),
                            executable: "/bin/sh".into(),
                            version: Some(env!("CARGO_PKG_VERSION").into()),
                            model: None,
                        },
                        "fixture-script-v1".into(),
                    )
                } else {
                    let config = config.ok_or_else(|| {
                        invalid(format!(
                            "Agent {} has no locally configured executor",
                            identity.kind
                        ))
                    })?;
                    if identity.model.is_some() && identity.model != config.model {
                        return Err(invalid(
                            "Requested model differs from the configured executor; configure its command and model explicitly",
                        ));
                    }
                    let adapter = GenericShellAdapter::new(&config.command)?;
                    let mut command = adapter.build_command(prompt)?;
                    command.environment = config.environment;
                    command.program = resolve_program(&command.program)?.to_string_lossy().into();
                    let resolved = AgentIdentity {
                        kind: identity.kind.clone(),
                        executable: command.program.clone(),
                        version: config.version,
                        model: config.model,
                    };
                    (command, resolved, config.command)
                };
                command
                    .environment_overrides
                    .insert("HARDKNOCK_EXPERIMENT_CANDIDATE".into(), "1".into());
                Ok(ResolvedCandidate {
                    candidate: candidate.clone(),
                    command,
                    commands: None,
                    agent: resolved,
                    template,
                })
            }
            CandidateExecution::EffectPlan { simulation, .. } => Ok(ResolvedCandidate {
                candidate: candidate.clone(),
                command: simulation.first().map_or_else(
                    || CommandSpec {
                        program: "/bin/true".into(),
                        args: Vec::new(),
                        environment: EnvironmentMode::Controlled,
                        environment_overrides: BTreeMap::new(),
                    },
                    |step| CommandSpec::shell(step, EnvironmentMode::Controlled),
                ),
                commands: (!simulation.is_empty()).then(|| {
                    simulation
                        .iter()
                        .map(|step| CommandSpec::shell(step, EnvironmentMode::Controlled))
                        .collect()
                }),
                agent: AgentIdentity {
                    kind: "effect-plan".into(),
                    executable: if simulation.is_empty() {
                        "/bin/true"
                    } else {
                        "/bin/sh"
                    }
                    .into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    model: None,
                },
                template: "hardknock-effect-plan-v1".into(),
            }),
        }
    }

    fn proof(
        &self,
        request: &ExperimentRequest,
        resolved: &[ResolvedCandidate],
    ) -> Result<StartingStateProof> {
        let state = &request.starting_state.state_ref;
        let environment =
            EnvironmentContext::capture(&state.repo_path, EnvironmentMode::Controlled)?;
        let mut runtimes = BTreeMap::new();
        for c in resolved {
            runtimes.insert(
                c.agent.kind.clone(),
                hash(&(
                    artifact(Path::new(&c.command.program))?.blake3,
                    &c.template,
                    &c.agent,
                    c.command.environment,
                ))?,
            );
        }
        let fingerprint = hash(&(
            "experiment-start-v1",
            state,
            &environment.fingerprint,
            &runtimes,
            &request.evaluator,
        ))?;
        Ok(StartingStateProof { state_ref: state.clone(), fingerprint, environment_fingerprint: environment.fingerprint, runtime_fingerprints: runtimes, scope: "Tracked commit/tree and fixture inputs, controlled evaluator environment, shell and configured executable/template hashes. No live process snapshot; inherited agent settings, remote models, ignored files and host services are not frozen.".into() })
    }

    pub fn starting_proof(&self, request: &ExperimentRequest) -> Result<StartingStateProof> {
        let resolved = request
            .candidates
            .iter()
            .map(|c| self.resolve(request, c))
            .collect::<Result<Vec<_>>>()?;
        self.proof(request, &resolved)
    }

    /// Shared admission barrier: no candidate may run until every prepared Reality matches.
    pub fn verify_equivalent_realities(
        &self,
        request: &ExperimentRequest,
        proof: &StartingStateProof,
        realities: &[crate::core::Reality],
    ) -> Result<()> {
        for reality in realities {
            if reality.starting_state != proof.state_ref {
                return Err(invalid(
                    "Experiment refused: candidate starting references differ",
                ));
            }
            GitRealityProvider::new(self.store).verify_start(reality)?;
        }
        if self.starting_proof(request)? != *proof {
            return Err(invalid(
                "Experiment refused: starting environment drifted during Reality preparation",
            ));
        }
        Ok(())
    }

    async fn execute_inner(
        &self,
        experiment: &StrategyExperiment,
        cancel: &Cancellation,
    ) -> Result<ExperimentResult> {
        let request = &experiment.request;
        self.progress(
            &experiment.id,
            None,
            ExperimentPhase::Preparing,
            "Validating budget, capacity, executors and equivalent starting state".into(),
        )?;
        if cancel.is_cancelled() {
            return Err(invalid("Cancelled before creating a Reality"));
        }
        let resolved = request
            .candidates
            .iter()
            .map(|c| self.resolve(request, c))
            .collect::<Result<Vec<_>>>()?;
        let proof = self.proof(request, &resolved)?;
        if request
            .starting_state
            .expected_fingerprint
            .as_ref()
            .is_some_and(|expected| expected != &proof.fingerprint)
        {
            return Err(invalid(
                "Experiment refused: candidate realities cannot be created from equivalent state (starting fingerprint drift)",
            ));
        }
        let mut capacity = Vec::new();
        for n in 0..self.config.experiments.provider_capacity {
            match lock(&self.store.home, &format!("experiment-capacity-{n}.lock")) {
                Ok(lease) => capacity.push(lease),
                Err(Error::Intervention(_)) => {}
                Err(error) => return Err(error),
            }
            if capacity.len() == resolved.len() {
                break;
            }
        }
        if capacity.len() < resolved.len() {
            return Err(invalid(
                "Provider capacity exhausted; no candidate Reality was created",
            ));
        }
        if let Some(parent) = &request.starting_state.parent_reality
            && self.store.reality(parent)?.starting_state != proof.state_ref
        {
            return Err(invalid(
                "Parent Reality and experiment starting state differ",
            ));
        }
        let provider = GitRealityProvider::new(self.store);
        let mut batch = PreparedBatch {
            store: self.store,
            trials: VecDeque::new(),
        };
        let mut realities = Vec::new();
        for c in &resolved {
            if cancel.is_cancelled() {
                return Err(invalid("Cancelled during preparation"));
            }
            let (mut reality, lease) = provider.create_for_run(&proof.state_ref, false)?;
            reality.parent = request.starting_state.parent_reality.clone();
            reality.fork_reason = Some(ForkReason::AgentExperiment);
            reality.experiment_id = Some(experiment.id.clone());
            reality.candidate_id = Some(c.candidate.id.clone());
            realities.push(reality.id.clone());
            batch.trials.push_back(PreparedTrial {
                reality,
                lease,
                experiment: Some(ExperimentEvidence {
                    experiment_id: experiment.id.clone(),
                    candidate_id: c.candidate.id.clone(),
                    starting_fingerprint: proof.fingerprint.clone(),
                }),
                commands: c.commands.clone(),
            });
            let trial = batch.trials.back().expect("just inserted trial");
            self.store.update_reality(&trial.reality)?;
            provider.verify_start(&trial.reality)?;
            if matches!(
                c.agent.kind.as_str(),
                "test-agent" | "fake-agent-A" | "fake-agent-B"
            ) {
                let marker = crate::experience::fixture_metadata(&trial.reality.root)?;
                if !matches!(
                    marker["kind"].as_str(),
                    Some("strategy-choice" | "confounded-comparison")
                ) {
                    return Err(invalid(
                        "test-agent requires a strategy-choice or confounded-comparison fixture",
                    ));
                }
            }
        }
        // Barrier: validate every prepared tree and the environment again before any execution.
        self.verify_equivalent_realities(
            request,
            &proof,
            &batch
                .trials
                .iter()
                .map(|trial| trial.reality.clone())
                .collect::<Vec<_>>(),
        )?;
        let variables = changed_variables(&resolved)?;
        let quality = classify_quality(
            &variables,
            resolved.iter().any(|c| {
                c.command.environment == EnvironmentMode::Inherited
                    || (matches!(c.candidate.execution, CandidateExecution::AgentTask { .. })
                        && !matches!(
                            c.agent.kind.as_str(),
                            "test-agent" | "fake-agent-A" | "fake-agent-B"
                        ))
            }),
        );
        self.store
            .insert_experiment_variables(&experiment.id, &variables)?;
        let parallel = if request.origin == ExperimentOrigin::Agent {
            self.config
                .experiments
                .max_parallel_realities
                .min(self.config.experiments.agent_requests.max_parallel)
        } else {
            self.config.experiments.max_parallel_realities
        };
        let mut pending: VecDeque<_> = resolved.into();
        let mut tasks = JoinSet::new();
        let mut usage = ExperienceUsage {
            realities: realities.len(),
            tool_calls: Some(0),
            ..Default::default()
        };
        let mut failure = None;
        while !pending.is_empty() || !tasks.is_empty() {
            while tasks.len() < parallel
                && !pending.is_empty()
                && !cancel.is_cancelled()
                && failure.is_none()
            {
                let c = pending.pop_front().expect("pending candidate");
                let prepared = batch.trials.pop_front().expect("prepared candidate");
                let home = self.store.home.clone();
                let cancellation = cancel.clone();
                let id = experiment.id.clone();
                let question = request.question.clone();
                let evaluator = request.evaluator.clone();
                let proof = proof.clone();
                let timeout_secs = experiment
                    .effective_budget
                    .max_duration_ms
                    .unwrap_or(300_000)
                    .div_ceil(1000)
                    .max(1);
                if matches!(c.candidate.execution, CandidateExecution::AgentTask { .. }) {
                    usage.agent_runs += 1;
                    usage.tool_calls = None;
                }
                if let Err(error) = self.progress(
                    &id,
                    Some(c.candidate.id.clone()),
                    ExperimentPhase::Executing,
                    format!("{} started", c.candidate.name),
                ) {
                    batch.trials.push_front(prepared);
                    failure.get_or_insert(error);
                    cancel.cancel();
                    break;
                }
                // Each bounded worker owns a connection and a current-thread runtime. SQLite is
                // never shared across threads, and Bridge action handling remains independent.
                tasks.spawn_blocking(move || -> Result<CandidateResult> {
                    let store = Store::open(&home)?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let executor = hash(&(
                        artifact(Path::new(&c.command.program))?.blake3,
                        &c.template,
                        &c.agent,
                        c.command.environment,
                    ))?;
                    if proof.runtime_fingerprints.get(&c.agent.kind) != Some(&executor) {
                        return Err(invalid(
                            "Executor fingerprint changed before candidate execution",
                        ));
                    }
                    let fingerprint = if c.command.environment == EnvironmentMode::Controlled {
                        proof.environment_fingerprint.clone()
                    } else {
                        EnvironmentContext::capture(&prepared.reality.root, c.command.environment)?
                            .fingerprint
                    };
                    let start = Instant::now();
                    let mut prepared_effects = Vec::new();
                    if let CandidateExecution::EffectPlan { effects, .. } = &c.candidate.execution {
                        let manager = crate::effects::EffectManager::new(&store)?;
                        for request in effects {
                            let mut request = request.clone();
                            request.reality_id = Some(prepared.reality.id.clone());
                            match manager.propose_and_prepare(
                                request,
                                &crate::effects::EffectManager::agent_context("experiment-engine"),
                            ) {
                                Ok((effect, _)) => prepared_effects.push(effect.id),
                                Err(error) => {
                                    for id in &prepared_effects {
                                        let _ = manager.discard(
                                            id,
                                            &crate::effects::EffectManager::user_context(),
                                        );
                                    }
                                    return Err(error);
                                }
                            }
                        }
                    }
                    let replay = c.commands.as_ref().map(|commands| ReplaySpec {
                        script: commands
                            .iter()
                            .map(|s| s.args[1].clone())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        timeout_secs,
                    });
                    let run = match runtime.block_on(run_prepared_trial(
                        &store,
                        RunRequest {
                            state: proof.state_ref,
                            goal: question,
                            agent: c.agent.clone(),
                            command: c.command,
                            evaluation: evaluator,
                            timeout_secs,
                            keep: false,
                            replay,
                            perturbations: vec![],
                            expected_fingerprint: Some(fingerprint),
                        },
                        prepared,
                        &cancellation,
                    )) {
                        Ok(run) => run,
                        Err(error) => {
                            let manager = crate::effects::EffectManager::new(&store)?;
                            for id in &prepared_effects {
                                let _ = manager
                                    .discard(id, &crate::effects::EffectManager::user_context());
                            }
                            return Err(error);
                        }
                    };
                    if !run.experience.evaluation.success {
                        let manager = crate::effects::EffectManager::new(&store)?;
                        for id in &prepared_effects {
                            manager.discard(id, &crate::effects::EffectManager::user_context())?;
                        }
                    }
                    for id in &prepared_effects {
                        store.link_effect_experience(
                            id,
                            &run.experience.id,
                            "experimental_candidate",
                        )?;
                    }
                    let result = CandidateResult {
                        candidate_id: c.candidate.id,
                        name: c.candidate.name,
                        reality_id: run.reality.id,
                        experience_id: run.experience.id,
                        execution_status: run.execution.status,
                        evaluation: run.experience.evaluation,
                        diff_summary: Some(summarize_diff(&fs::read(&run.execution.diff.path)?)),
                        duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                        artifacts: run.experience.evidence.artifacts,
                        starting_fingerprint: proof.fingerprint,
                        agent: c.agent,
                        prepared_effects,
                    };
                    store.insert_candidate_result(&id, &result)?;
                    Ok(result)
                });
            }
            if tasks.is_empty() {
                break;
            }
            match tasks.join_next().await {
                Some(Ok(Ok(result))) => {
                    match self
                        .store
                        .experience(&result.experience_id)
                        .and_then(|experience| {
                            usage.commands += experience.actions.len();
                            self.progress(
                                &experiment.id,
                                Some(result.candidate_id),
                                ExperimentPhase::Evaluating,
                                format!("{} completed: {}", result.name, result.evaluation.summary),
                            )
                        }) {
                        Ok(()) => {}
                        Err(error) => {
                            failure.get_or_insert(error);
                            cancel.cancel();
                        }
                    }
                }
                Some(Ok(Err(error))) => {
                    failure.get_or_insert(error);
                    cancel.cancel();
                }
                Some(Err(error)) => {
                    failure.get_or_insert(invalid(format!("Candidate worker failed: {error}")));
                    cancel.cancel();
                }
                None => break,
            }
        }
        // Join every launched process before cleanup; unexecuted leased trials are discarded too.
        drop(batch);
        for id in realities {
            let mut reality = self.store.reality(&id)?;
            if reality.status != RealityStatus::Discarded {
                let _lease = self.store.lock_reality(&id)?;
                provider.discard(&mut reality)?;
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.progress(
            &experiment.id,
            None,
            ExperimentPhase::Comparing,
            "Comparing evaluator evidence".into(),
        )?;
        let candidates = self.store.candidate_results(&experiment.id)?;
        let mut comparison =
            EvaluatorSuccessFirst(request.criteria.clone()).compare(&candidates)?;
        if candidates.len() != request.candidates.len() || cancel.is_cancelled() {
            comparison.recommendation = None;
            comparison
                .reasons
                .push("Incomplete comparison; no recommendation".into());
        }
        comparison.evidence_weight = match quality {
            ExperimentQuality::Controlled => {
                "controlled single-state observation; replication required"
            }
            ExperimentQuality::PartiallyControlled => {
                "partial control; native settings/model/host state unverified"
            }
            ExperimentQuality::Confounded => {
                "confounded observation only; no causal strategy attribution"
            }
            ExperimentQuality::Invalid => "invalid",
        }
        .into();
        let effect_manager = crate::effects::EffectManager::new(self.store)?;
        for candidate in &candidates {
            let selected = candidate.evaluation.success
                && comparison.recommendation.as_ref() == Some(&candidate.candidate_id);
            for effect_id in &candidate.prepared_effects {
                let effect = self.store.effect(effect_id)?;
                if effect.lifecycle != crate::effects::EffectLifecycle::Prepared {
                    continue;
                }
                if selected {
                    self.store.detach_prepared_effect(effect_id)?;
                } else {
                    effect_manager
                        .discard(effect_id, &crate::effects::EffectManager::user_context())?;
                }
            }
        }
        self.progress(
            &experiment.id,
            None,
            ExperimentPhase::Learning,
            "Recording Experiences; any proposed Lesson remains Candidate".into(),
        )?;
        Ok(ExperimentResult {
            experiment_id: experiment.id.clone(),
            question: request.question.clone(),
            recommendation: comparison.recommendation.clone(),
            confidence: None,
            created_experience: candidates.iter().map(|c| c.experience_id.clone()).collect(),
            candidates,
            comparison,
            candidate_lessons: vec![],
            quality,
            changed_variables: variables,
            starting_state: Some(proof),
            usage,
        })
    }

    fn progress(
        &self,
        id: &ExperimentId,
        candidate: Option<crate::core::CandidateId>,
        phase: ExperimentPhase,
        message: String,
    ) -> Result<()> {
        self.store.append_experiment_progress(&ExperimentProgress {
            experiment_id: id.clone(),
            candidate,
            phase,
            message,
            created_at: Utc::now(),
        })
    }

    fn reflect(&self, result: &ExperimentResult) -> Result<Vec<crate::core::LessonId>> {
        use crate::{
            core::HypothesisId,
            experience::Outcome,
            lesson::{ActionPattern, ContextSelector, HeuristicConfidence, Lesson},
            reflection::CandidateHypothesis,
            store::LessonStore,
        };
        if result.candidates.len() != 2 {
            return Ok(vec![]);
        }
        let Some(winner) = result.candidates.iter().find(|c| {
            Some(&c.candidate_id) == result.recommendation.as_ref() && c.evaluation.success
        }) else {
            return Ok(vec![]);
        };
        let Some(loser) = result
            .candidates
            .iter()
            .find(|c| c.candidate_id != winner.candidate_id && !c.evaluation.success)
        else {
            return Ok(vec![]);
        };
        let source = self.store.experience(&loser.experience_id)?;
        if source.outcome != Outcome::Failure {
            return Ok(vec![]);
        }
        let h = CandidateHypothesis {
            id: HypothesisId::new(),
            source_experience: source.id.clone(),
            created_at: Utc::now(),
            claim: format!(
                "{} outperformed {} for this starting state and evaluator; replication is required",
                winner.name, loser.name
            ),
            rationale: format!(
                "Controlled experiment {} observed failing {} and passing {}. This proposes a scoped hypothesis, not established causality.",
                result.experiment_id, loser.experience_id, winner.experience_id
            ),
            context_match: ContextSelector::from_context(&source.context),
            avoid: ActionPattern::Custom {
                kind: "experiment_candidate".into(),
                value: loser.candidate_id.to_string(),
            },
            prefer: ActionPattern::Custom {
                kind: "experiment_candidate".into(),
                value: winner.candidate_id.to_string(),
            },
            generated_by: AgentIdentity {
                kind: "experiment-reflection".into(),
                executable: "hardknock".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                model: None,
            },
        };
        self.store.insert_hypothesis(&h)?;
        let lesson = Lesson::candidate(&h, &HeuristicConfidence);
        LessonStore::insert(self.store, &lesson)?;
        Ok(vec![lesson.id])
    }
}

fn resolve_program(program: &str) -> Result<PathBuf> {
    if Path::new(program).is_absolute() {
        return Ok(Path::new(program).canonicalize()?);
    }
    if program.contains('/') {
        return Err(invalid(
            "Configured executor must be absolute or a name on PATH; relative executors are ambiguous",
        ));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(program))
        .find(|p| p.is_file())
        .ok_or_else(|| invalid(format!("Executor {program} is not installed")))?
        .canonicalize()
        .map_err(Error::Io)
}

fn changed_variables(candidates: &[ResolvedCandidate]) -> Result<Vec<ExperimentVariable>> {
    let mut variables = Vec::new();
    for name in ["strategy", "agent", "model", "configuration", "environment"] {
        let values = candidates
            .iter()
            .map(|c| {
                let value = match name {
                    "strategy" => match &c.candidate.execution {
                        CandidateExecution::Shell { commands } => hash(commands)?,
                        CandidateExecution::AgentTask { prompt, .. } => hash(prompt)?,
                        CandidateExecution::EffectPlan {
                            effects,
                            simulation,
                        } => hash(&(effects, simulation))?,
                    },
                    "agent" => format!(
                        "{}@{}",
                        c.agent.kind,
                        c.agent.version.as_deref().unwrap_or("unknown")
                    ),
                    "model" => c
                        .agent
                        .model
                        .clone()
                        .unwrap_or_else(|| "unspecified".into()),
                    "configuration" => hash(&c.template)?,
                    _ => format!("{:?}", c.command.environment),
                };
                Ok((c.candidate.name.clone(), value))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        if values.values().collect::<BTreeSet<_>>().len() > 1 {
            variables.push(ExperimentVariable {
                name: name.into(),
                values,
            });
        }
    }
    Ok(variables)
}
