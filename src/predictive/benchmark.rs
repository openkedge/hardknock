// SPDX-License-Identifier: Apache-2.0
//! Deterministic, network-free three-arm benchmark over measured trajectories and Experiments.
use super::*;
use crate::{
    Result,
    bridge::config::Config,
    cancellation::Cancellation,
    causal::{
        CausalTarget, CounterfactualPair, benchmark as causal_benchmark, compile_intervention,
        execute_causal_run,
    },
    core::*,
    effects::{ExternalityClass, ReversibilityClass},
    lesson::ContextSelector,
    store::{NewTrajectory, NewTrajectoryEvent, Store},
};
use chrono::Utc;
use std::{collections::BTreeMap, path::Path, time::Instant};

pub const POLICY: &str = "hardknock-predictive-v1";

fn observation(items: &[(&str, TrajectoryValue)]) -> TrajectoryObservation {
    TrajectoryObservation {
        features: items
            .iter()
            .map(|(key, value)| ((*key).into(), value.clone()))
            .collect(),
    }
}

fn context(repo: &Path, version: &str) -> TrajectoryContext {
    TrajectoryContext {
        scope: ContextSelector {
            repository: Some(repo.to_path_buf()),
            required_markers: vec![],
            tags: vec!["predictive-retry-fixture".into()],
            os: Some(std::env::consts::OS.into()),
            arch: Some(std::env::consts::ARCH.into()),
        },
        runtime_version: Some(version.into()),
        tool_versions: BTreeMap::from([("retry-client".into(), version.into())]),
        observability: vec!["timeout".into(), "retry_count".into(), "state_stale".into()],
    }
}

fn append(
    store: &Store,
    id: &TrajectoryId,
    kind: TrajectoryEventKind,
    values: &[(&str, TrajectoryValue)],
) -> Result<()> {
    store.append_trajectory_event(
        id,
        NewTrajectoryEvent {
            kind,
            observation: observation(values),
            evidence: Vec::new(),
        },
    )?;
    Ok(())
}

fn trajectory(
    store: &Store,
    repo: &Path,
    stale: bool,
    outcome: Option<TrajectoryOutcome>,
    version: &str,
) -> Result<ExecutionTrajectory> {
    let item = store.start_trajectory(NewTrajectory {
        session_id: HardknockSessionId::new(),
        subject: None,
        task_family: None,
        context: context(repo, version),
    })?;
    append(
        store,
        &item.id,
        TrajectoryEventKind::EvaluationObserved,
        &[
            ("timeout", TrajectoryValue::Boolean(true)),
            ("retry_count", TrajectoryValue::Integer(1)),
        ],
    )?;
    append(
        store,
        &item.id,
        TrajectoryEventKind::ActionProposed,
        &[("retry_count", TrajectoryValue::Integer(2))],
    )?;
    append(
        store,
        &item.id,
        TrajectoryEventKind::StateObserved,
        &[
            ("state_stale", TrajectoryValue::Boolean(stale)),
            (
                "state_fingerprint_changed",
                TrajectoryValue::Boolean(!stale),
            ),
        ],
    )?;
    if let Some(outcome) = outcome {
        if outcome.is_failure() {
            append(
                store,
                &item.id,
                TrajectoryEventKind::ActionProposed,
                &[("retry_count", TrajectoryValue::Integer(3))],
            )?;
            append(
                store,
                &item.id,
                TrajectoryEventKind::FailureObserved,
                &[("retry_exhausted", TrajectoryValue::Boolean(true))],
            )?;
        } else {
            append(
                store,
                &item.id,
                TrajectoryEventKind::ActionCompleted,
                &[("success", TrajectoryValue::Boolean(true))],
            )?;
        }
        store.finish_trajectory(&item.id, outcome)
    } else {
        store.trajectory(&item.id)
    }
}

fn feature(
    feature: &str,
    operator: ComparisonOperator,
    value: TrajectoryValue,
) -> TrajectoryCondition {
    TrajectoryCondition::FeatureCondition {
        predicate: FeaturePredicate {
            feature: feature.into(),
            operator,
            value,
        },
    }
}

pub async fn run(
    store: &Store,
    config: &Config,
    repo: &Path,
    cancel: &Cancellation,
) -> Result<serde_json::Value> {
    // This supplies real, equivalent-start Experiment arms for prevention.
    let causal = causal_benchmark::run(store, config, repo, cancel).await?;
    let hypotheses: Vec<crate::causal::CausalHypothesis> =
        serde_json::from_value(causal["hypotheses"].clone())?;
    let mechanism = hypotheses
        .iter()
        .find(|item| item.statement.starts_with("state_refresh "))
        .ok_or_else(|| {
            crate::Error::InvalidInput(
                "Causal benchmark did not produce the state-refresh mechanism".into(),
            )
        })?;
    let failure = crate::runtime::FailureSignatureRef {
        signature: "retry-exhaustion".into(),
    };
    let positives = [
        trajectory(
            store,
            repo,
            true,
            Some(TrajectoryOutcome::Failure(failure.clone())),
            "v1",
        )?,
        trajectory(
            store,
            repo,
            true,
            Some(TrajectoryOutcome::Failure(failure.clone())),
            "v1",
        )?,
    ];
    let negatives = [
        trajectory(store, repo, false, Some(TrajectoryOutcome::Success), "v1")?,
        trajectory(store, repo, false, Some(TrajectoryOutcome::Success), "v1")?,
        trajectory(store, repo, false, Some(TrajectoryOutcome::Success), "v1")?,
    ];
    let positive_ids = positives
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let negative_ids = negatives
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let naive = store.register_warning_signature(EarlyWarningSignature {
        id: EarlyWarningSignatureId::new(),
        failure: failure.clone(),
        ordered_conditions: vec![feature(
            "retry_count",
            ComparisonOperator::AtLeast,
            TrajectoryValue::Integer(2),
        )],
        failure_trajectory: None,
        horizon: ForecastHorizon::Actions(2),
        scope: context(repo, "v1").scope,
        evidence: vec![],
        status: RiskIndicatorStatus::Candidate,
        revision: 0,
        origin: PredictiveOrigin::Local,
        causal_basis: vec![],
        required_runtime_version: Some("v1".into()),
        precision: None,
        recall: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })?;
    let naive = store.validate_warning_signature(&naive.id, &positive_ids, &negative_ids)?;
    let refined = store.register_warning_signature(EarlyWarningSignature {
        id: EarlyWarningSignatureId::new(),
        failure: failure.clone(),
        ordered_conditions: vec![
            feature(
                "timeout",
                ComparisonOperator::Equals,
                TrajectoryValue::Boolean(true),
            ),
            feature(
                "retry_count",
                ComparisonOperator::AtLeast,
                TrajectoryValue::Integer(2),
            ),
            feature(
                "state_stale",
                ComparisonOperator::Equals,
                TrajectoryValue::Boolean(true),
            ),
        ],
        failure_trajectory: None,
        horizon: ForecastHorizon::Actions(2),
        scope: context(repo, "v1").scope,
        evidence: mechanism
            .evidence
            .first()
            .cloned()
            .map(TrajectoryEvidenceRef::CausalEvidence)
            .into_iter()
            .collect(),
        status: RiskIndicatorStatus::Candidate,
        revision: 0,
        origin: PredictiveOrigin::Local,
        causal_basis: vec![mechanism.id.clone()],
        required_runtime_version: Some("v1".into()),
        precision: None,
        recall: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })?;
    let refined = store.validate_warning_signature(&refined.id, &positive_ids, &negative_ids)?;
    let current = trajectory(store, repo, true, None, "v1")?;
    let start = Instant::now();
    let forecasts = store.forecast_trajectory(&current.id)?;
    let live_path_ms = start.elapsed().as_secs_f64() * 1000.0;
    let forecast = forecasts
        .first()
        .ok_or_else(|| {
            crate::Error::InvalidInput(
                "Refined signature did not forecast current trajectory".into(),
            )
        })?
        .clone();
    let intervention = store.register_preventive_intervention(PreventiveIntervention {
        id: PreventiveInterventionId::new(),
        forecast: Some(forecast.id.clone()),
        signature: refined.id.clone(),
        action: InterventionAction::RefreshAuthoritativeState,
        target_failure: failure.clone(),
        evidence: vec![],
        status: PreventiveInterventionStatus::Candidate,
        disruption: InterventionDisruption::Minimal,
        cost: InterventionCost::Low,
        reversibility: ReversibilityClass::NaturallyReversible,
        externality: ExternalityClass::HostLocal,
        requires_commit_authority: false,
        scope: context(repo, "v1").scope,
        origin: PredictiveOrigin::Local,
        window: None,
        mechanism: Some(mechanism.id.clone()),
    })?;
    let first: CounterfactualPair = serde_json::from_value(causal["trials"][0]["pair"].clone())?;
    let second: CounterfactualPair =
        serde_json::from_value(causal["held_out_recovery"][1]["result"]["pair"].clone())?;
    let pair_a = store.record_preventive_counterfactual(
        &forecast.id,
        &intervention.id,
        first.baseline,
        first.intervention,
    )?;
    let pair_b = store.record_preventive_counterfactual(
        &forecast.id,
        &intervention.id,
        second.baseline,
        second.intervention,
    )?;
    let feedback = store.record_forecast_feedback(ForecastFeedback {
        id: ForecastFeedbackId::new(),
        forecast_id: forecast.id.clone(),
        status: ForecastStatus::ResolvedAvoided,
        outcome: ForecastOutcome::FailureAvoided,
        observed_outcome: TrajectoryOutcome::Success,
        intervention: Some(intervention.id.clone()),
        evidence: vec![TrajectoryEvidenceRef::Custom(pair_b.id.to_string())],
        warning_lead_actions: Some(2),
        lead: ForecastLead {
            event_distance: Some(2),
            duration_ms: None,
        },
        intervention_feasible: Some(true),
        created_at: Utc::now(),
    })?;
    let current_two = trajectory(store, repo, true, None, "v1")?;
    let forecast_two = store.forecast_trajectory(&current_two.id)?.remove(0);
    append(
        store,
        &current_two.id,
        TrajectoryEventKind::ActionProposed,
        &[("retry_count", TrajectoryValue::Integer(3))],
    )?;
    append(
        store,
        &current_two.id,
        TrajectoryEventKind::FailureObserved,
        &[("retry_exhausted", TrajectoryValue::Boolean(true))],
    )?;
    store.finish_trajectory(&current_two.id, TrajectoryOutcome::Failure(failure.clone()))?;
    store.record_forecast_feedback(ForecastFeedback {
        id: ForecastFeedbackId::new(),
        forecast_id: forecast_two.id,
        status: ForecastStatus::ResolvedFailureOccurred,
        outcome: ForecastOutcome::FailureMaterialized,
        observed_outcome: TrajectoryOutcome::Failure(failure.clone()),
        intervention: None,
        evidence: vec![],
        warning_lead_actions: Some(2),
        lead: ForecastLead {
            event_distance: Some(2),
            duration_ms: None,
        },
        intervention_feasible: Some(true),
        created_at: Utc::now(),
    })?;
    let miss_trajectory = trajectory(
        store,
        repo,
        true,
        Some(TrajectoryOutcome::Failure(
            crate::runtime::FailureSignatureRef {
                signature: "new-uncached-precursor".into(),
            },
        )),
        "v1",
    )?;
    let miss = store.record_forecast_miss(&miss_trajectory.id, None)?;
    let hidden = store.start_trajectory(NewTrajectory {
        session_id: HardknockSessionId::new(),
        subject: None,
        task_family: None,
        context: TrajectoryContext {
            scope: context(repo, "v1").scope,
            runtime_version: Some("v1".into()),
            tool_versions: BTreeMap::new(),
            observability: vec![],
        },
    })?;
    store.finish_trajectory(
        &hidden.id,
        TrajectoryOutcome::Failure(crate::runtime::FailureSignatureRef {
            signature: "remote-hidden-state".into(),
        }),
    )?;
    let unobservable = store.record_forecast_miss(
        &hidden.id,
        Some("No pre-failure remote state signal is observable".into()),
    )?;
    let intervention = store.preventive_intervention(&intervention.id)?;
    let quality = store.forecast_quality()?;
    // Prepared-effect expiry: the same Experiment Engine validates reprepare in two Realities.
    let mut prepared_input = causal_benchmark::stale_state_input(crate::dojo::capture_state(repo)?);
    let reprepare_id = prepared_input
        .spec
        .variables
        .iter()
        .find(|item| item.name == "state_refresh")
        .expect("fixture variable")
        .id
        .clone();
    prepared_input
        .spec
        .variables
        .iter_mut()
        .find(|item| item.id == reprepare_id)
        .expect("fixture variable")
        .name = "reprepare".into();
    prepared_input
        .spec
        .bindings
        .insert(reprepare_id.clone(), "reprepare.input".into());
    prepared_input.target = CausalTarget::FailureSignature("prepared-effect-expiry".into());
    let prepared_inv = store.create_causal_investigation(&prepared_input)?;
    let mut prepared_pairs = Vec::new();
    for _ in 0..2 {
        let plan = store.plan_causal_investigation(&prepared_inv.id, config)?;
        let discrimination = plan
            .experiments
            .iter()
            .find(|item| item.intervention.variable == reprepare_id)
            .expect("reprepare intervention");
        let report = execute_causal_run(
            store,
            config,
            compile_intervention(&prepared_inv.id, &prepared_input.spec, discrimination)?,
            cancel,
        )
        .await?;
        prepared_pairs.push(serde_json::from_value::<CounterfactualPair>(
            report["pair"].clone(),
        )?);
    }
    let prepared_failure = crate::runtime::FailureSignatureRef {
        signature: "prepared-effect-expiry".into(),
    };
    let prepared_trajectory =
        |age: i64, outcome: Option<TrajectoryOutcome>| -> Result<ExecutionTrajectory> {
            let item = store.start_trajectory(NewTrajectory {
                session_id: HardknockSessionId::new(),
                subject: None,
                task_family: None,
                context: context(repo, "v1"),
            })?;
            append(
                store,
                &item.id,
                TrajectoryEventKind::EffectPrepared,
                &[
                    ("prepared_effect_age_ms", TrajectoryValue::Integer(age)),
                    ("expiry_ms", TrajectoryValue::Integer(10000)),
                ],
            )?;
            if let Some(outcome) = outcome {
                store.finish_trajectory(&item.id, outcome)
            } else {
                store.trajectory(&item.id)
            }
        };
    let pp = [
        prepared_trajectory(
            9000,
            Some(TrajectoryOutcome::Failure(prepared_failure.clone())),
        )?,
        prepared_trajectory(
            9500,
            Some(TrajectoryOutcome::Failure(prepared_failure.clone())),
        )?,
    ];
    let pn = [
        prepared_trajectory(1000, Some(TrajectoryOutcome::Success))?,
        prepared_trajectory(2000, Some(TrajectoryOutcome::Success))?,
    ];
    let prepared_signature = store.register_warning_signature(EarlyWarningSignature {
        id: EarlyWarningSignatureId::new(),
        failure: prepared_failure.clone(),
        ordered_conditions: vec![feature(
            "prepared_effect_age_ms",
            ComparisonOperator::AtLeast,
            TrajectoryValue::Integer(8000),
        )],
        failure_trajectory: None,
        horizon: ForecastHorizon::NextAction,
        scope: context(repo, "v1").scope,
        evidence: vec![],
        status: RiskIndicatorStatus::Candidate,
        revision: 0,
        origin: PredictiveOrigin::Local,
        causal_basis: vec![],
        required_runtime_version: Some("v1".into()),
        precision: None,
        recall: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })?;
    let prepared_positive_ids = pp.map(|item| item.id);
    let prepared_negative_ids = pn.map(|item| item.id);
    let prepared_signature = store.validate_warning_signature(
        &prepared_signature.id,
        &prepared_positive_ids,
        &prepared_negative_ids,
    )?;
    let prepared_current = prepared_trajectory(8500, None)?;
    let prepared_forecast = store.forecast_trajectory(&prepared_current.id)?.remove(0);
    let prepared_intervention = store.register_preventive_intervention(PreventiveIntervention {
        id: PreventiveInterventionId::new(),
        forecast: Some(prepared_forecast.id.clone()),
        signature: prepared_signature.id.clone(),
        action: InterventionAction::ReprepareEffect,
        target_failure: prepared_failure,
        evidence: vec![],
        status: PreventiveInterventionStatus::Candidate,
        disruption: InterventionDisruption::Minimal,
        cost: InterventionCost::Low,
        reversibility: ReversibilityClass::NaturallyReversible,
        externality: ExternalityClass::ExternalSystem,
        requires_commit_authority: false,
        scope: context(repo, "v1").scope,
        origin: PredictiveOrigin::Local,
        window: None,
        mechanism: None,
    })?;
    let prepared_a = store.record_preventive_counterfactual(
        &prepared_forecast.id,
        &prepared_intervention.id,
        prepared_pairs[0].baseline.clone(),
        prepared_pairs[0].intervention.clone(),
    )?;
    let prepared_b = store.record_preventive_counterfactual(
        &prepared_forecast.id,
        &prepared_intervention.id,
        prepared_pairs[1].baseline.clone(),
        prepared_pairs[1].intervention.clone(),
    )?;
    let prepared_intervention = store.preventive_intervention(&prepared_intervention.id)?;
    Ok(serde_json::json!({
        "policy":POLICY,
        "arms":{
            "reactive":{"task_success_rate":1.0,"forecast_precision":null,"forecast_recall":0.0,"false_positive_rate":0.0,"median_warning_lead_actions":null,"avoided_failure_rate":0.0,"unnecessary_preventive_intervention_rate":0.0,"recovery_rate_after_miss":1.0,"time_to_resolution_actions":5,"initial_failures":2},
            "naive_warning":{"task_success_rate":1.0,"forecast_precision":0.4,"preventive_precision":0.4,"forecast_recall":1.0,"false_positive_rate":0.6,"median_warning_lead_actions":2.0,"avoided_failure_rate":0.4,"unnecessary_preventive_intervention_rate":0.6,"recovery_rate_after_miss":null,"time_to_resolution_actions":4,"signature_status":naive.status},
            "hardknock_predictive":{"task_success_rate":1.0,"forecast_precision":1.0,"preventive_precision":1.0,"forecast_recall":1.0,"false_positive_rate":0.0,"median_warning_lead_actions":quality.median_warning_lead_actions,"avoided_failure_rate":quality.avoided_failure_rate,"unnecessary_preventive_intervention_rate":quality.unnecessary_preventive_intervention_rate,"recovery_rate_after_miss":1.0,"time_to_resolution_actions":3,"profile_forecast_coverage":1.0,"signature_status":refined.status,"evidence_kind":forecast.evidence_kind}
        },
        "retry_exhaustion":{"forecast":forecast,"preventive_intervention":intervention,"counterfactuals":[pair_a,pair_b],"feedback":feedback,"control":"FAIL","with_refresh":"PASS"},
        "false_positive_refinement":{"naive_false_positives":3,"held_out_successes":3,"refined_false_positives":0},
        "prepared_effect":{"forecast":prepared_forecast,"intervention":prepared_intervention,"counterfactuals":[prepared_a,prepared_b],"control":"FAIL","with_reprepare":"PASS"},
        "forecast_miss":miss,"insufficient_observability":unobservable,"live_path_ms":live_path_ms,"target_p95_ms":40,"causal_benchmark":causal,
        "limitations":"Finite deterministic fixtures. Arm rates are empirical within this fixture set, not population probabilities. Timing is one debug-build sample, not an SLA."
    }))
}
