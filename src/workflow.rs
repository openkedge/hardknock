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

pub async fn run_with_resilience(
    store: &Store,
    request: RunRequest,
    learning: &RunLearningOptions,
    resilience: &crate::resilience::runtime::RunResilienceOptions,
    cancel: &Cancellation,
) -> Result<RunResult> {
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
    let (mut reality, _lease) = provider.create_for_run(&request.state, request.keep)?;
    let mut started = false;
    let keep = request.keep;
    let mut perturbation_handles = crate::perturbation::AppliedPerturbations::default();
    let result = async {
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
        let (status, action) = if let Some(result) = &runtime {
            (result.status, result.actions.last().cloned().ok_or_else(|| Error::InvalidInput("Runtime produced no actions".into()))?)
        } else {
            ProcessRunner.run(&request.command, &reality.root, &artifacts.join("agent"), Duration::from_secs(request.timeout_secs), cancel.cancelled()).await?
        };
        let agent_diff = artifacts.join("agent.diff.patch");
        fs::write(&agent_diff, provider.diff(&reality)?)?;
        let execution = ExecutionRecord { id: ExecutionId::new(), reality_id: reality.id.clone(), starting_state: request.state.clone(), task: request.goal.clone(), agent: request.agent.clone(), status, action, diff: artifact(&agent_diff)?.with_kind(ArtifactKind::Diff) };
        let metadata = artifacts.join("execution.json");
        fs::write(&metadata, serde_json::to_vec_pretty(&execution)?)?;
        store.insert_execution(&execution)?;
        let observation=observe_application(advice,&execution,&id,&reality.root,&artifacts,learning)?;
        let evaluator = CommandEvaluator { spec: request.evaluation, timeout: Duration::from_secs(request.timeout_secs), environment: request.command.environment, environment_overrides: runtime.as_ref().map(|r| r.environment.clone()).unwrap_or_else(|| request.command.environment_overrides.clone()) };
        let evaluation = evaluator.evaluate(&reality, &execution, &artifacts, cancel).await?;
        // Evaluators may modify files; retain their final diff separately from agent effects.
        let diff_path = artifacts.join("diff.patch");
        fs::write(&diff_path, provider.diff(&reality)?)?;
        let mut actions = runtime.as_ref().map(|r| r.actions.clone()).unwrap_or_else(|| vec![execution.action.clone()]);
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
            id, created_at: Utc::now(), goal: request.goal, tags: context.tags.clone(), context,
            starting_state: request.state, reality_id: reality.id.clone(), execution_id: execution.id.clone(), agent: request.agent,
            actions, perturbations, outcome: Outcome::from_evaluation(&evaluation),
            failure_signatures: signatures, evaluation,
            evidence: EvidenceBundle { artifacts: evidence }, replay,
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
    let preserve =
        keep || (started && result.is_err() && !matches!(&result, Err(Error::ProcessStart { .. })));
    let cleanup = if preserve {
        if result.is_err() {
            reality.status = RealityStatus::Failed;
            reality.ephemeral = false;
        }
        store.update_reality(&reality)
    } else {
        provider.discard(&mut reality)
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
