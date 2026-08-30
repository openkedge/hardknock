// SPDX-License-Identifier: Apache-2.0

use crate::application::{RunLearningOptions, observe_application, prepare_advice};
use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::{
        AgentIdentity, ArtifactKind, CommandSpec, ExecutionId, ExecutionRecord, ExperienceId,
        Reality, RealityStatus, StateRef,
    },
    dojo::{GitRealityProvider, RealityProvider},
    evaluation::{CommandEvaluator, EvaluationSpec, Evaluator},
    experience::{
        EvidenceBundle, Experience, ExperienceContext, Outcome, Perturbation, ReplaySpec,
        failure_signatures,
    },
    process::ProcessRunner,
    store::{ExperienceStore, Store, artifact},
};
use chrono::Utc;
use std::{fs, time::Duration};

/// A leased Reality, verified at the orchestration barrier before any trial runs.
pub struct PreparedTrial {
    pub reality: Reality,
    pub lease: fs::File,
    pub experiment: Option<crate::experimentation::ExperimentEvidence>,
    pub commands: Option<Vec<CommandSpec>>,
}

pub async fn run_prepared_trial(
    store: &Store,
    request: RunRequest,
    prepared: PreparedTrial,
    cancel: &Cancellation,
) -> Result<RunResult> {
    execute_prepared(
        store,
        request,
        &RunLearningOptions::default(),
        None,
        cancel,
        prepared,
    )
    .await
}

#[derive(Clone)]
pub struct RunRequest {
    pub state: StateRef,
    pub goal: String,
    pub agent: AgentIdentity,
    pub command: CommandSpec,
    pub evaluation: EvaluationSpec,
    pub timeout_secs: u64,
    pub keep: bool,
    pub replay: Option<ReplaySpec>,
    pub perturbations: Vec<Perturbation>,
    pub expected_fingerprint: Option<String>,
}

#[derive(serde::Serialize)]
pub struct RunResult {
    pub execution: ExecutionRecord,
    pub experience: Experience,
    pub reality: Reality,
}

/// Shared lifecycle for original runs and trials: persist before discarding state.
pub async fn run_once(
    store: &Store,
    request: RunRequest,
    cancel: &Cancellation,
) -> Result<RunResult> {
    run_with_learning(store, request, &RunLearningOptions::default(), cancel).await
}

pub async fn run_with_learning(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    cancel: &Cancellation,
) -> Result<RunResult> {
    run_configured(store, request, learning, None, cancel).await
}

/// Run the agent execution plane inside a capability-isolated container while
/// keeping Hardknock reasoning, evaluation, effect authority, and evidence on
/// the trusted host plane. Evaluator commands remain trusted host operations in
/// V0.9 and are explicitly not attributed to the agent container.
pub async fn run_in_container(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    mut manifest: crate::capability::CapabilityManifest,
    image: Option<&str>,
    cancel: &Cancellation,
) -> Result<RunResult> {
    request.evaluation.validate()?;
    if cancel.is_cancelled() {
        return Err(Error::Intervention(
            "Run cancelled before creating a Reality".into(),
        ));
    }
    let requested_timeout = request.timeout_secs.saturating_mul(1000);
    manifest.resources.timeout_ms = Some(
        manifest
            .resources
            .timeout_ms
            .unwrap_or(requested_timeout)
            .min(requested_timeout),
    );
    let runtime = crate::capability::ContainerRuntime::detect()?;
    let provider = crate::capability::ContainerRealityProvider::with_runtime(
        store,
        runtime,
        image.unwrap_or(crate::capability::DEFAULT_CONTAINER_IMAGE),
    )?;
    let mut reality = crate::capability::IsolatedRealityProvider::create_with_capabilities(
        &provider,
        &request.state,
        &manifest,
    )?;
    let _token =
        match crate::cli::capability::issue_reality_token(store, &reality).and_then(|token| {
            crate::cli::capability::publish_reality_token(store, &reality, &token)?;
            Ok(token)
        }) {
            Ok(token) => token,
            Err(primary) => {
                return match provider.discard(&mut reality) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::Cleanup {
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                };
            }
        };
    let lease = store.lock_reality(&reality.id)?;
    execute_prepared(
        store,
        request,
        learning,
        None,
        cancel,
        PreparedTrial {
            reality,
            lease,
            experiment: None,
            commands: None,
        },
    )
    .await
}

pub async fn run_with_resilience(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    resilience: &crate::resilience::runtime::RunResilienceOptions,
    cancel: &Cancellation,
) -> Result<RunResult> {
    store.register_perturbations(&resilience.perturbations)?;
    run_configured(store, request, learning, Some(resilience), cancel).await
}

async fn run_configured(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    resilience: Option<&crate::resilience::runtime::RunResilienceOptions>,
    cancel: &Cancellation,
) -> Result<RunResult> {
    request.evaluation.validate()?;
    if cancel.is_cancelled() {
        return Err(Error::Intervention(
            "Run cancelled before creating a Reality".into(),
        ));
    }
    let provider = GitRealityProvider::new(store);
    let (reality, lease) = provider.create_for_run(&request.state, request.keep)?;
    execute_prepared(
        store,
        request,
        learning,
        resilience,
        cancel,
        PreparedTrial {
            reality,
            lease,
            experiment: None,
            commands: None,
        },
    )
    .await
}

async fn execute_prepared(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    resilience: Option<&crate::resilience::runtime::RunResilienceOptions>,
    cancel: &Cancellation,
    prepared: PreparedTrial,
) -> Result<RunResult> {
    let provider = GitRealityProvider::new(store);
    let PreparedTrial {
        mut reality,
        lease: _lease,
        experiment,
        commands,
    } = prepared;
    let experimental = experiment.is_some();
    let mut started = false;
    let keep = request.keep;
    let mut perturbation_handles = crate::perturbation::AppliedPerturbations::default();
    let result = async {
        if !experimental {
            if resilience.is_some() { reality.fork_reason = Some(crate::core::ForkReason::Chaos); }
            for relation in &learning.relations {
                let reason = match relation {
                    crate::application::ExperienceRelation::CounterfactualOf(_) => Some(crate::core::ForkReason::Counterfactual),
                    crate::application::ExperienceRelation::RetryOf(_) => Some(crate::core::ForkReason::Retry),
                    crate::application::ExperienceRelation::ChaosVariantOf(_) => Some(crate::core::ForkReason::Chaos),
                    _ => None,
                };
                if let Some(reason) = reason {
                    reality.fork_reason = Some(reason);
                    let source = store.experience(relation.target())?;
                    if source.starting_state == request.state { reality.parent = Some(source.reality_id); }
                    break;
                }
            }
        }
        provider.verify_start(&reality)?;
        let context = ExperienceContext::capture(&request.state, &reality.root, request.command.environment)?;
        if request.expected_fingerprint.as_ref().is_some_and(|expected| *expected != context.environment.fingerprint) {
            return Err(Error::Intervention("Counterfactual experiment cannot guarantee equivalent starting state: environment fingerprint mismatch.".into()));
        }
        if cancel.is_cancelled() { return Err(Error::Intervention("Run cancelled before execution".into())); }
        reality.status = RealityStatus::Running;
        store.update_reality(&reality)?;
        let id = ExperienceId::new();
        let artifacts = store.home.join("artifacts").join(id.to_string());
        fs::create_dir(&artifacts)?;
        let advice=prepare_advice(store,&context,&request.goal,&reality.root,&artifacts,learning)?;
        if let Some(options) = resilience { perturbation_handles = crate::resilience::runtime::apply(&reality, options)?; }
        started = true;
        let mut runtime = if let Some(options) = resilience {
            Some(crate::resilience::runtime::execute(&reality, &context, &request, &artifacts, cancel, options, &perturbation_handles).await?)
        } else { None };
        let mut trial_actions = Vec::new();
        let (status, action) = if let Some(result) = &runtime {
            (result.status, result.actions.last().cloned().ok_or_else(|| Error::InvalidInput("Runtime produced no actions".into()))?)
        } else if let Some(commands) = &commands {
            let mut last_status = crate::core::ProcessStatus::Succeeded;
            for (i, command) in commands.iter().enumerate() {
                let (status, action) = run_reality_process(store, &reality, command, &artifacts.join(format!("agent-{i}")), request.timeout_secs, cancel).await?;
                trial_actions.push(action);
                last_status = status;
                if status != crate::core::ProcessStatus::Succeeded { break; }
            }
            (last_status, trial_actions.last().cloned().ok_or_else(|| Error::InvalidInput("Trial has no commands".into()))?)
        } else {
            run_reality_process(store, &reality, &request.command, &artifacts.join("agent"), request.timeout_secs, cancel).await?
        };
        let agent_diff = artifacts.join("agent.diff.patch");
        fs::write(&agent_diff, provider.diff(&reality)?)?;
        let execution = ExecutionRecord { id: ExecutionId::new(), reality_id: reality.id.clone(), starting_state: request.state.clone(), task: request.goal.clone(), agent: request.agent.clone(), status, action, diff: artifact(&agent_diff)?.with_kind(ArtifactKind::Diff) };
        let metadata = artifacts.join("execution.json");
        fs::write(&metadata, serde_json::to_vec_pretty(&execution)?)?;
        store.insert_execution(&execution)?;
        let observation=observe_application(advice,&execution,&id,&reality.root,&artifacts,learning)?;
        if let Some(link) = &experiment {
            store.append_experiment_progress(&crate::experimentation::ExperimentProgress { experiment_id: link.experiment_id.clone(), candidate: Some(link.candidate_id.clone()), phase: crate::experimentation::ExperimentPhase::Evaluating, message: "Candidate execution finished; running identical configured checks".into(), created_at: Utc::now() })?;
        }
        let evaluator = CommandEvaluator { spec: request.evaluation, timeout: Duration::from_secs(request.timeout_secs), environment: if experimental { crate::core::EnvironmentMode::Controlled } else { request.command.environment }, environment_overrides: if experimental { Default::default() } else { runtime.as_ref().map(|r| r.environment.clone()).unwrap_or_else(|| request.command.environment_overrides.clone()) } };
        let evaluation = evaluator.evaluate(&reality, &execution, &artifacts, cancel).await?;
        // Evaluators may modify files; retain their final diff separately from agent effects.
        let diff_path = artifacts.join("diff.patch");
        fs::write(&diff_path, provider.diff(&reality)?)?;
        let mut actions = runtime.as_ref().map(|r| r.actions.clone()).unwrap_or_else(|| if trial_actions.is_empty() { vec![execution.action.clone()] } else { trial_actions });
        actions.extend(evaluation.checks.iter().filter_map(|c| c.action.clone()));
        let mut evidence: Vec<_> = actions.iter().flat_map(|a| [a.stdout.clone(), a.stderr.clone()]).collect();
        evidence.extend([execution.diff.clone(), artifact(&diff_path)?.with_kind(ArtifactKind::Diff), artifact(&metadata)?.with_kind(ArtifactKind::Metadata)]);
        evidence.extend(observation.artifacts);
        let mut replay=request.replay;
        if learning.fixture
            && let Some(action)=observation.actions.first().and_then(|a|a.action.shell_script()) {
                replay=Some(ReplaySpec { script:action.into(),timeout_secs:request.timeout_secs });
            }
        let mut signatures = failure_signatures(&evaluation, &execution.action)?;
        if let Some(result) = &mut runtime {
            crate::resilience::runtime::finish(result, &evaluation);
            signatures.extend(result.signatures.clone());
        }
        let mut perturbations = request.perturbations;
        if let Some(options) = resilience { perturbations.extend(options.perturbations.iter().map(|p| Perturbation::Local { perturbation_id: p.id.clone() })); }
        let experience = Experience {
            experiment,
            id, created_at: Utc::now(), goal: request.goal, tags: context.tags.clone(), context,
            starting_state: request.state, reality_id: reality.id.clone(), execution_id: execution.id.clone(), agent: request.agent,
            actions, perturbations, outcome: Outcome::from_evaluation(&evaluation),
            failure_signatures: signatures, evaluation,
            evidence: EvidenceBundle {
                artifacts: evidence,
                execution_assurance: Some(crate::capability::ExecutionAssurance {
                    reality_provider: reality.execution_boundary.provider.clone(),
                    isolation: reality.execution_boundary.capabilities.clone(),
                    capability_manifest_hash: reality.execution_boundary.manifest_hash.clone(),
                    external_effect_gating: reality.execution_boundary.capabilities.external_effect_control == crate::capability::EffectControlLevel::Gated,
                }),
            }, replay,
            lesson_applications:observation.applications, relations:observation.relations, repeated_mistakes:observation.mistakes,
            observed_actions:observation.actions, application_report_errors:observation.errors,
            resilience: runtime.map(|r|r.observation),
        };
        fs::write(artifacts.join("metadata.json"), serde_json::to_vec_pretty(&experience)?)?;
        ExperienceStore::insert(store, &experience)?;
        reality.status = if experience.exit_code(status) == 0 { RealityStatus::Completed } else { RealityStatus::Failed };
        store.update_reality(&reality)?;
        Ok((execution, experience))
    }.await;
    let result = match (result, perturbation_handles.remove()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    let preserve = !experimental
        && (keep
            || (started && result.is_err() && !matches!(&result, Err(Error::ProcessStart { .. }))));
    let cleanup = if preserve {
        if result.is_err() {
            reality.status = RealityStatus::Failed;
            reality.ephemeral = false;
        }
        store.update_reality(&reality)
    } else {
        if reality.execution_boundary.provider == "container" {
            crate::effects::EffectManager::new(store)
                .and_then(|manager| manager.discard_reality(&reality.id))
                .and_then(|_| crate::capability::ContainerRealityProvider::new(store))
                .and_then(|container| container.discard(&mut reality))
        } else {
            provider.discard(&mut reality)
        }
    };
    match (result, cleanup) {
        (Ok((execution, experience)), Ok(())) => Ok(RunResult {
            execution,
            experience,
            reality,
        }),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
        (Err(error), Ok(())) if preserve => Err(Error::RealityPreserved {
            id: reality.id.to_string(),
            path: reality.root.display().to_string(),
            source: Box::new(error),
        }),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

async fn run_reality_process(
    store: &Store,
    reality: &Reality,
    command: &CommandSpec,
    artifacts: &std::path::Path,
    timeout_secs: u64,
    cancel: &Cancellation,
) -> Result<(crate::core::ProcessStatus, crate::core::ActionRecord)> {
    if reality.execution_boundary.provider != "container" {
        return ProcessRunner
            .run(
                command,
                &reality.root,
                artifacts,
                Duration::from_secs(timeout_secs),
                cancel.cancelled(),
            )
            .await;
    }
    if cancel.is_cancelled() {
        return Err(Error::Intervention(
            "Run cancelled before container action".into(),
        ));
    }
    let token = crate::cli::capability::issue_reality_token(store, reality)?;
    let proxy = crate::capability::CapabilityExecutionProxy::new(
        store,
        crate::capability::SecretRedactor::default(),
    )?;
    let result = crate::capability::ToolExecutionProxy::execute(
        &proxy,
        reality,
        &token,
        &crate::capability::NormalizedAction::Shell(command.clone()),
        artifacts,
    )
    .await?;
    match result {
        crate::capability::ActionResult::Process { status, action } => Ok((status, action)),
        _ => Err(Error::InvalidInput(
            "Container shell proxy returned a non-process result".into(),
        )),
    }
}
