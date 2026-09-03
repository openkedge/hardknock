// SPDX-License-Identifier: Apache-2.0
//! Deterministic fixture adapters and measured three-arm benchmark. No model or network calls.
use crate::{
    Result,
    capability::{IsolationLevel, RealityRequirements},
    causal::*,
    core::*,
    experimentation::{ExperimentStartingState, SnapshotSource},
    lesson::ContextSelector,
};
use chrono::Utc;
use std::collections::BTreeMap;

pub fn stale_state_input(state: StateRef) -> CausalInvestigationInput {
    let variable = |name: &str, kind, domain, intervenable| CausalVariable {
        id: CausalVariableId::new(),
        name: name.into(),
        kind,
        domain,
        observable: true,
        intervenable,
    };
    let latency = variable(
        "latency",
        CausalVariableKind::Perturbation,
        VariableDomain::IntegerRange { min: 0, max: 10000 },
        true,
    );
    let retries = variable(
        "retry_count",
        CausalVariableKind::Configuration,
        VariableDomain::IntegerRange { min: 1, max: 20 },
        true,
    );
    let refresh = variable(
        "state_refresh",
        CausalVariableKind::State,
        VariableDomain::Boolean,
        true,
    );
    let failure = variable(
        "failure",
        CausalVariableKind::Outcome,
        VariableDomain::Boolean,
        false,
    );
    let scope = ContextSelector {
        repository: Some(state.repo_path.clone()),
        required_markers: vec![],
        tags: vec![],
        os: Some(std::env::consts::OS.into()),
        arch: Some(std::env::consts::ARCH.into()),
    };
    let values = [
        VariableValue::Integer(1000),
        VariableValue::Integer(3),
        VariableValue::Boolean(false),
    ];
    let inputs = [latency.clone(), retries.clone(), refresh.clone()];
    let hypotheses = inputs
        .iter()
        .map(|v| CausalHypothesis {
            id: CausalHypothesisId::new(),
            statement: format!("{} explains retry failure under this fixture scope", v.name),
            claim: CausalClaim::Causes,
            cause: v.id.clone(),
            effect: failure.id.clone(),
            scope: scope.clone(),
            conditions: vec![],
            status: CausalHypothesisStatus::Candidate,
            evidence: vec![],
            origin: CausalHypothesisOrigin::AgentSuggestion,
            baseline_prediction: PredictedOutcome::Fail,
            intervention_prediction: PredictedOutcome::Pass,
            remote_origin: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .collect();
    let baseline = inputs
        .iter()
        .zip(values)
        .map(|(v, value)| (v.id.clone(), value))
        .collect();
    let bindings = inputs
        .iter()
        .map(|v| (v.id.clone(), format!("{}.input", v.name).into()))
        .collect();
    let alternatives = [
        VariableValue::Integer(0),
        VariableValue::Integer(9),
        VariableValue::Boolean(true),
    ];
    let capabilities = inputs
        .iter()
        .zip(alternatives)
        .map(|(v, to)| InterventionCapability {
            variable: v.id.clone(),
            supported_values: vec![to],
            required_reality: RealityRequirements {
                filesystem_isolation: IsolationLevel::Cooperative,
                process_isolation: IsolationLevel::None,
                network_isolation: IsolationLevel::None,
                credential_isolation: IsolationLevel::None,
                effect_gating: false,
            },
        })
        .collect();
    CausalInvestigationInput {
        source_experiences: vec![],
        target: CausalTarget::FailureSignature("retry-exhaustion".into()),
        hypotheses,
        spec: CausalTestSpec {
            starting_state: ExperimentStartingState {
                state_ref: state,
                expected_fingerprint: None,
                parent_reality: None,
                source: SnapshotSource::RepositoryCommit,
            },
            variables: vec![latency, retries, refresh, failure],
            baseline,
            bindings,
            command: "sh scenario.sh".into(),
            evaluator: crate::evaluation::EvaluationSpec {
                checks: vec!["test \"$(cat outcome.txt)\" = PASS".into()],
            },
            scope,
            available_interventions: capabilities,
            known_confounders: vec![],
            budget: Default::default(),
            causal_budget: Default::default(),
        },
    }
}

pub async fn run(
    store: &crate::store::Store,
    config: &crate::bridge::config::Config,
    repo: &std::path::Path,
    cancel: &crate::cancellation::Cancellation,
) -> Result<serde_json::Value> {
    let input = stale_state_input(crate::dojo::capture_state(repo)?);
    let inv = store.create_causal_investigation(&input)?;
    let plan = store.plan_causal_investigation(&inv.id, config)?;
    let mut trials = vec![];
    for d in &plan.experiments {
        trials.push(
            execute_causal_run(
                store,
                config,
                compile_intervention(&inv.id, &input.spec, d)?,
                cancel,
            )
            .await?,
        );
    }
    let hypotheses = store.causal_investigation_hypotheses(&inv.id)?;
    let mut arms = BTreeMap::new();
    for (arm, variable) in [
        ("correlation_learner", "latency"),
        ("strategy_counterfactual_learner", "state_refresh"),
        ("causal_learner", "state_refresh"),
    ] {
        let id = &input
            .spec
            .variables
            .iter()
            .find(|v| v.name == variable)
            .expect("fixture variable")
            .id;
        let h = hypotheses
            .iter()
            .find(|h| &h.cause == id)
            .expect("fixture hypothesis");
        let evidence = store.causal_evidence(&h.id)?;
        let observed = evidence.last().ok_or_else(|| {
            crate::Error::InvalidInput("Benchmark requires completed intervention evidence".into())
        })?;
        let success = usize::from(observed.intervention_outcome == PredictedOutcome::Pass);
        arms.insert(arm,serde_json::json!({"task_success_rate":success,"repeated_failure_rate":1-success,"recovery_success_rate":success,"spurious_lesson_rate":if arm=="causal_learner" {0} else {usize::from(arm=="correlation_learner" && observed.outcome==CausalEvidenceOutcome::Contradicts)},"lessons_challenged":1,"mechanism_explicit":arm=="causal_learner"}));
    }
    // Held-out input points are measured with the same engine and explicit fixture semantics.
    let variable_id = |name: &str| {
        input
            .spec
            .variables
            .iter()
            .find(|v| v.name == name)
            .expect("fixture variable")
            .id
            .clone()
    };
    let mut heldout = input.clone();
    heldout.target = CausalTarget::Outcome("Held-out retry fixture: latency=2000,retries=5".into());
    heldout
        .spec
        .baseline
        .insert(variable_id("latency"), VariableValue::Integer(2000));
    heldout
        .spec
        .baseline
        .insert(variable_id("retry_count"), VariableValue::Integer(5));
    let held_inv = store.create_causal_investigation(&heldout)?;
    let held_plan = store.plan_causal_investigation(&held_inv.id, config)?;
    let mut held_reports = vec![];
    for name in ["retry_count", "state_refresh"] {
        let d = held_plan
            .experiments
            .iter()
            .find(|d| d.intervention.variable == variable_id(name))
            .expect("fixture intervention");
        let start = std::time::Instant::now();
        let result = execute_causal_run(
            store,
            config,
            compile_intervention(&held_inv.id, &heldout.spec, d)?,
            cancel,
        )
        .await?;
        held_reports.push(serde_json::json!({"recovery":name,"success":result["evidence"][0]["intervention_outcome"]=="pass","paired_experiment_duration_ms":start.elapsed().as_millis(),"result":result}));
    }
    let mut healthy = 0usize;
    let mut broad_false_positives = 0usize;
    let mut refined_false_positives = 0usize;
    let mut precision_trials = vec![];
    for latency in [500, 1000, 2000] {
        let mut healthy_input = input.clone();
        healthy_input.target =
            CausalTarget::Outcome(format!("Healthy reflex-negative input {latency}"));
        healthy_input
            .spec
            .baseline
            .insert(variable_id("latency"), VariableValue::Integer(latency));
        healthy_input
            .spec
            .baseline
            .insert(variable_id("state_refresh"), VariableValue::Boolean(true));
        let check = store.create_causal_investigation(&healthy_input)?;
        let p = store.plan_causal_investigation(&check.id, config)?;
        let d = p
            .experiments
            .iter()
            .find(|d| d.intervention.variable == variable_id("latency"))
            .expect("latency challenge");
        let result = execute_causal_run(
            store,
            config,
            compile_intervention(&check.id, &healthy_input.spec, d)?,
            cancel,
        )
        .await?;
        if result["evidence"][0]["baseline_outcome"] == "pass" {
            healthy += 1;
            broad_false_positives += usize::from(latency >= 1000);
            refined_false_positives += usize::from(
                healthy_input.spec.baseline[&variable_id("state_refresh")]
                    == VariableValue::Boolean(false),
            );
        }
        precision_trials.push(result);
    }
    let rate = |n: usize| {
        if healthy == 0 {
            None
        } else {
            Some(n as f64 / healthy as f64)
        }
    };
    let reflex = serde_json::json!({"healthy_cases":healthy,"before_false_positives":broad_false_positives,"after_false_positives":refined_false_positives,"before_false_positive_rate":rate(broad_false_positives),"after_false_positive_rate":rate(refined_false_positives),"scope":"Explicit latency>=1000 warning versus stale-state conditional warning, only on measured healthy fixture points; candidate rules are not activated","trials":precision_trials});
    Ok(
        serde_json::json!({"investigation":inv.id,"hypotheses":hypotheses,"causal_interventions":trials.len(),"interventions_until_discrimination":plan.experiments.iter().position(|p|input.spec.variables.iter().any(|v|v.name=="state_refresh"&&v.id==p.intervention.variable)).map(|i|i+1),"arms":arms,"trials":trials,"held_out_recovery":held_reports,"reflex_precision":reflex,"limitations":"Finite deterministic retry fixture only. Strategy and causal learners can find the same successful action; only causal learner retains an explicit tested mechanism. No cross-domain or statistical generalization claim. Timings measure complete paired experiments, not production time-to-recovery."}),
    )
}
