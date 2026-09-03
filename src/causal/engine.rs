// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result, bridge::config::Config, cancellation::Cancellation, core::*,
    evaluation::EvaluationStatus, experimentation::*, store::Store,
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Component,
};

fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
pub fn validate_spec(spec: &CausalTestSpec) -> Result<()> {
    spec.evaluator.validate()?;
    if spec.evaluator.checks.is_empty() {
        return Err(invalid("Causal evidence requires an explicit evaluator"));
    }
    if spec.command.trim().is_empty()
        || spec.command.contains('\0')
        || spec.variables.len() > 64
        || spec.variables.is_empty()
    {
        return Err(invalid(
            "A causal fixture requires a trusted local command and 1..64 explicit variables",
        ));
    }
    let ids: BTreeSet<_> = spec.variables.iter().map(|v| &v.id).collect();
    let names: BTreeSet<_> = spec.variables.iter().map(|v| &v.name).collect();
    if ids.len() != spec.variables.len() || names.len() != ids.len() {
        return Err(invalid("Variable names and IDs must be unique"));
    }
    for (id, value) in &spec.baseline {
        if !spec
            .variables
            .iter()
            .any(|v| v.id == *id && v.domain.contains(value))
        {
            return Err(invalid("Baseline value is outside its declared domain"));
        }
    }
    if spec.bindings.keys().collect::<BTreeSet<_>>() != spec.baseline.keys().collect()
        || spec.bindings.values().collect::<BTreeSet<_>>().len() != spec.bindings.len()
    {
        return Err(invalid("Each baseline input needs a distinct file binding"));
    }
    for path in spec.bindings.values() {
        if path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
            || path.to_str().is_none_or(|s| {
                s.starts_with('.') || !s.ends_with(".input") || s.contains(['\0', '\n'])
            })
        {
            return Err(invalid(
                "Input bindings must be non-hidden root-level *.input files, not arbitrary filesystem targets",
            ));
        }
    }
    for c in &spec.available_interventions {
        let v = spec
            .variables
            .iter()
            .find(|v| v.id == c.variable)
            .ok_or_else(|| invalid("Unknown intervention variable"))?;
        if !spec.baseline.contains_key(&v.id)
            || !v.intervenable
            || c.supported_values
                .iter()
                .any(|value| !v.domain.contains(value))
        {
            return Err(invalid(
                "Intervention needs an intervenable input and domain-valid values",
            ));
        }
    }
    if spec
        .known_confounders
        .iter()
        .any(|c| !ids.contains(&c.variable))
    {
        return Err(invalid("Unknown confounder variable"));
    }
    Ok(())
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn script(spec: &CausalTestSpec, values: &BTreeMap<CausalVariableId, VariableValue>) -> String {
    let mut s = "set -eu\n".to_string();
    for (id, path) in &spec.bindings {
        let path = quote(&path.to_string_lossy());
        // Prevent tracked symlink bindings from escaping the Reality. The command remains trusted.
        s.push_str(&format!(
            "test ! -L {path}\nprintf '%s\\n' {} > {path}\n",
            quote(&values[id].literal())
        ));
    }
    s.push_str(&spec.command);
    s
}
fn request(
    spec: &CausalTestSpec,
    changed: &BTreeMap<CausalVariableId, VariableValue>,
) -> ExperimentRequest {
    let mut budget = spec.budget.clone();
    if let Some(duration) = spec.causal_budget.max_duration {
        let allocation = (duration.as_millis()
            / spec.causal_budget.max_interventions.max(1) as u128)
            .min(u64::MAX as u128) as u64;
        budget.max_duration_ms = Some(budget.max_duration_ms.unwrap_or(u64::MAX).min(allocation));
    }
    ExperimentRequest {
        id: ExperimentRequestId::new(),
        session_id: "causal-local".into(),
        question: "Controlled causal intervention under explicit fixture scope".into(),
        hypothesis: None,
        candidates: [("baseline", &spec.baseline), ("intervention", changed)]
            .into_iter()
            .map(|(name, values)| ExperimentCandidate {
                id: CandidateId::new(),
                name: name.into(),
                description: "Same adapter/evaluator; only declared input literals differ".into(),
                execution: CandidateExecution::Shell {
                    commands: vec![script(spec, values)],
                },
                expected_outcome: None,
            })
            .collect(),
        starting_state: spec.starting_state.clone(),
        evaluator: spec.evaluator.clone(),
        budget,
        requested_by: AgentIdentity {
            kind: "causal-planner".into(),
            executable: "hardknock".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            model: None,
        },
        created_at: Utc::now(),
        criteria: ComparisonCriteria::default(),
        origin: ExperimentOrigin::CausalInvestigation,
        intent: ExperimentIntent::ValidateHypothesis,
        capabilities: ExperimentCapabilities::default(),
    }
}
pub fn planning_context(
    spec: &CausalTestSpec,
    store: &Store,
    config: &Config,
) -> Result<CausalPlanningContext> {
    validate_spec(spec)?;
    let proof =
        ExperimentOrchestrator { store, config }.starting_proof(&request(spec, &spec.baseline))?;
    Ok(CausalPlanningContext {
        starting_state: proof,
        variables: spec.variables.clone(),
        values: spec.baseline.clone(),
        available_interventions: spec.available_interventions.clone(),
        evaluator: spec.evaluator.clone(),
        known_confounders: spec.known_confounders.clone(),
        reality_capabilities: RealityCapabilities::git_worktree(),
    })
}
pub fn compile_intervention(
    investigation: &CausalInvestigationId,
    spec: &CausalTestSpec,
    discrimination: &HypothesisDiscrimination,
) -> Result<CausalRun> {
    validate_spec(spec)?;
    if spec.causal_budget.max_interventions == 0
        || spec.causal_budget.max_variables_per_intervention == 0
    {
        return Err(invalid("Causal budget does not permit an intervention"));
    }
    let i = &discrimination.intervention;
    let cap = spec
        .available_interventions
        .iter()
        .find(|c| c.variable == i.variable && c.supported_values.contains(&i.to))
        .ok_or_else(|| invalid("Intervention value is not available"))?;
    if !requirements_met(&cap.required_reality, &RealityCapabilities::git_worktree()) {
        return Err(invalid(
            "INTERVENTION UNSUPPORTED: actual worktree provider cannot satisfy required isolation",
        ));
    }
    if i.from.as_ref() != spec.baseline.get(&i.variable) || i.from.as_ref() == Some(&i.to) {
        return Err(invalid(
            "Intervention must change the declared baseline value",
        ));
    }
    let held: BTreeSet<_> = spec
        .baseline
        .keys()
        .filter(|id| **id != i.variable)
        .collect();
    if i.held_constant.iter().collect::<BTreeSet<_>>() != held {
        return Err(invalid(
            "Held constants must enumerate every other baseline input",
        ));
    }
    let mut changed = spec.baseline.clone();
    changed.insert(i.variable.clone(), i.to.clone());
    Ok(CausalRun {
        investigation: investigation.clone(),
        discrimination: discrimination.clone(),
        request: request(spec, &changed),
        baseline: spec.baseline.clone(),
        changed,
        known_confounders: spec.known_confounders.clone(),
        scope: spec.scope.clone(),
    })
}
pub fn trial_outcome(c: &CandidateResult) -> PredictedOutcome {
    if c.evaluation.status != EvaluationStatus::Completed
        || c.execution_status != ProcessStatus::Succeeded
    {
        PredictedOutcome::Unknown
    } else if c.execution_status == ProcessStatus::Succeeded && c.evaluation.success {
        PredictedOutcome::Pass
    } else {
        PredictedOutcome::Fail
    }
}
pub fn derive_pair(run: &CausalRun, e: &StrategyExperiment) -> Result<CounterfactualPair> {
    if run.request.candidates.len() != 2
        || serde_json::to_value(&run.request)? != serde_json::to_value(&e.request)?
        || e.status != ExperimentStatus::Completed
    {
        return Err(invalid(
            "Only a completed matching ExperimentRequest can provide causal evidence",
        ));
    }
    let result = e
        .result
        .as_ref()
        .ok_or_else(|| invalid("Missing experiment result"))?;
    let baseline = result
        .candidates
        .iter()
        .find(|c| c.candidate_id == run.request.candidates[0].id)
        .ok_or_else(|| invalid("Missing baseline"))?;
    let intervention = result
        .candidates
        .iter()
        .find(|c| c.candidate_id == run.request.candidates[1].id)
        .ok_or_else(|| invalid("Missing intervention"))?;
    let proof = result
        .starting_state
        .clone()
        .ok_or_else(|| invalid("Missing starting state proof"))?;
    let changed: Vec<_> = run
        .changed
        .iter()
        .filter(|(id, v)| run.baseline.get(id) != Some(v))
        .map(|(id, _)| id.clone())
        .collect();
    let reference = |c: &CandidateResult| TrialRef {
        experiment: e.id.clone(),
        candidate: c.candidate_id.clone(),
        experience: c.experience_id.clone(),
    };
    let pair = CounterfactualPair {
        id: CounterfactualPairId::new(),
        starting_state: proof.clone(),
        baseline: reference(baseline),
        intervention: reference(intervention),
        changed_variables: changed.clone(),
        held_constant: run.discrimination.intervention.held_constant.clone(),
        quality: assess_quality(
            baseline.starting_fingerprint == intervention.starting_fingerprint
                && baseline.starting_fingerprint == proof.fingerprint,
            baseline.evaluation.spec == intervention.evaluation.spec,
            &changed,
            &run.discrimination.intervention.held_constant,
            &run.known_confounders,
            result.quality,
        ),
    };
    // Evidence is filled by the store against registered hypotheses, never caller-submitted status.
    Ok(pair)
}
pub async fn execute_causal_run(
    store: &Store,
    config: &Config,
    run: CausalRun,
    cancel: &Cancellation,
) -> Result<serde_json::Value> {
    store.start_causal_run(&run)?;
    let e = ExperimentOrchestrator { store, config }
        .run(run.request.clone(), cancel)
        .await?;
    if e.status != ExperimentStatus::Completed {
        return Ok(
            serde_json::json!({"experiment":e,"evidence":[],"notice":"No causal conclusion from incomplete or rejected execution"}),
        );
    }
    let pair = derive_pair(&run, &e)?;
    let evidence = store.finish_causal_run(&run, &e, &pair)?;
    Ok(
        serde_json::json!({"experiment":e.id,"pair":pair,"evidence":evidence,"hypotheses":store.causal_investigation_hypotheses(&run.investigation)?}),
    )
}
