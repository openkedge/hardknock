// SPDX-License-Identifier: Apache-2.0
mod support;

use chrono::Utc;
use hardknock::{
    bridge::config::Config,
    cancellation::Cancellation,
    core::*,
    curriculum::Severity,
    effects::{ExternalityClass, ReversibilityClass},
    lesson::ContextSelector,
    predictive::*,
    runtime::*,
    store::{NewTrajectory, NewTrajectoryEvent, Store},
};
use std::{collections::BTreeMap, time::Instant};
use support::Fixture;

fn scope(f: &Fixture) -> ContextSelector {
    ContextSelector {
        repository: Some(f.repo.clone()),
        required_markers: vec![],
        tags: vec![],
        os: Some(std::env::consts::OS.into()),
        arch: Some(std::env::consts::ARCH.into()),
    }
}
fn context(f: &Fixture, version: &str, observable: bool) -> TrajectoryContext {
    TrajectoryContext {
        scope: scope(f),
        runtime_version: Some(version.into()),
        tool_versions: BTreeMap::from([("client".into(), version.into())]),
        observability: if observable {
            vec!["timeout".into(), "retry_count".into(), "state_stale".into()]
        } else {
            vec![]
        },
    }
}
fn obs(items: &[(&str, TrajectoryValue)]) -> TrajectoryObservation {
    TrajectoryObservation {
        features: items
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    }
}
fn add(
    store: &Store,
    id: &TrajectoryId,
    kind: TrajectoryEventKind,
    items: &[(&str, TrajectoryValue)],
) {
    store
        .append_trajectory_event(
            id,
            NewTrajectoryEvent {
                kind,
                observation: obs(items),
                evidence: vec![],
            },
        )
        .unwrap();
}
fn start(store: &Store, f: &Fixture, version: &str, observable: bool) -> ExecutionTrajectory {
    store
        .start_trajectory(NewTrajectory {
            session_id: HardknockSessionId::new(),
            subject: None,
            task_family: None,
            context: context(f, version, observable),
        })
        .unwrap()
}
fn retry_trajectory(
    store: &Store,
    f: &Fixture,
    stale: bool,
    outcome: Option<TrajectoryOutcome>,
) -> ExecutionTrajectory {
    let t = start(store, f, "v1", true);
    add(
        store,
        &t.id,
        TrajectoryEventKind::EvaluationObserved,
        &[
            ("timeout", TrajectoryValue::Boolean(true)),
            ("retry_count", TrajectoryValue::Integer(1)),
        ],
    );
    add(
        store,
        &t.id,
        TrajectoryEventKind::ActionProposed,
        &[("retry_count", TrajectoryValue::Integer(2))],
    );
    add(
        store,
        &t.id,
        TrajectoryEventKind::StateObserved,
        &[("state_stale", TrajectoryValue::Boolean(stale))],
    );
    if let Some(outcome) = outcome {
        if outcome.is_failure() {
            add(
                store,
                &t.id,
                TrajectoryEventKind::FailureObserved,
                &[("retry_exhausted", TrajectoryValue::Boolean(true))],
            );
        } else {
            add(
                store,
                &t.id,
                TrajectoryEventKind::ActionCompleted,
                &[("success", TrajectoryValue::Boolean(true))],
            );
        }
        store.finish_trajectory(&t.id, outcome).unwrap()
    } else {
        store.trajectory(&t.id).unwrap()
    }
}
fn feature(
    name: &str,
    operator: ComparisonOperator,
    value: TrajectoryValue,
) -> TrajectoryCondition {
    TrajectoryCondition::FeatureCondition {
        predicate: FeaturePredicate {
            feature: name.into(),
            operator,
            value,
        },
    }
}
fn signature(
    f: &Fixture,
    failure: &str,
    conditions: Vec<TrajectoryCondition>,
    origin: PredictiveOrigin,
) -> EarlyWarningSignature {
    EarlyWarningSignature {
        id: EarlyWarningSignatureId::new(),
        failure: FailureSignatureRef {
            signature: failure.into(),
        },
        ordered_conditions: conditions,
        failure_trajectory: None,
        horizon: ForecastHorizon::Actions(2),
        scope: scope(f),
        evidence: vec![],
        status: RiskIndicatorStatus::Candidate,
        revision: 0,
        origin,
        causal_basis: vec![],
        required_runtime_version: Some("v1".into()),
        precision: None,
        recall: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
fn retry_conditions() -> Vec<TrajectoryCondition> {
    vec![
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
    ]
}
fn failure() -> TrajectoryOutcome {
    TrajectoryOutcome::Failure(FailureSignatureRef {
        signature: "retry-exhaustion".into(),
    })
}

#[test]
fn trajectories_are_ordered_normalized_windowed_and_structurally_fingerprinted() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let t = retry_trajectory(&store, &f, true, Some(failure()));
    let events = store.trajectory_events(&t.id).unwrap();
    assert_eq!(
        events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(t.fingerprint.event_kinds.len(), 4);
    assert!(
        !t.fingerprint
            .hash
            .contains(&events[0].timestamp.to_rfc3339())
    );
    let recent = windowed_events(
        &events,
        &TrajectoryWindow {
            max_events: 2,
            max_duration: None,
        },
    );
    assert_eq!(recent[0].sequence, 2);
    let bad = store.append_trajectory_event(
        &t.id,
        NewTrajectoryEvent {
            kind: TrajectoryEventKind::Custom("raw".into()),
            observation: obs(&[("secret value", TrajectoryValue::Text("x".into()))]),
            evidence: vec![],
        },
    );
    assert!(bad.is_err());
}

#[test]
fn temporal_ordering_and_full_match_are_required() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let t = start(&store, &f, "v1", true);
    add(
        &store,
        &t.id,
        TrajectoryEventKind::StateObserved,
        &[("state_stale", TrajectoryValue::Boolean(true))],
    );
    add(
        &store,
        &t.id,
        TrajectoryEventKind::EvaluationObserved,
        &[("timeout", TrajectoryValue::Boolean(true))],
    );
    add(
        &store,
        &t.id,
        TrajectoryEventKind::ActionProposed,
        &[("retry_count", TrajectoryValue::Integer(2))],
    );
    let sig = signature(
        &f,
        "retry-exhaustion",
        retry_conditions(),
        PredictiveOrigin::Local,
    );
    let matched = signature_matches(&sig, &store.trajectory_events(&t.id).unwrap());
    assert_eq!(
        matched.unmatched_conditions.len(),
        1,
        "stale state before timeout must not satisfy the ordered signature"
    );
}

#[test]
fn candidate_warnings_stay_inactive_and_remote_warnings_are_advisory_only() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let current = retry_trajectory(&store, &f, true, None);
    store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap();
    store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::FederatedAdvisory,
        ))
        .unwrap();
    let forecasts = store.forecast_trajectory(&current.id).unwrap();
    assert_eq!(forecasts.len(), 1);
    assert!(forecasts[0].advisory);
    assert!(matches!(
        forecasts[0].status,
        ForecastStatus::Watch | ForecastStatus::Elevated
    ));
    assert!(!forecasts[0].status.is_actionable());
}

#[test]
fn negative_controls_reject_naive_warning_and_validate_refined_warning() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let p = positives.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
    let n = negatives.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
    let naive = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            vec![feature(
                "retry_count",
                ComparisonOperator::AtLeast,
                TrajectoryValue::Integer(2),
            )],
            PredictiveOrigin::Local,
        ))
        .unwrap();
    assert_eq!(
        store
            .validate_warning_signature(&naive.id, &p, &n)
            .unwrap()
            .status,
        RiskIndicatorStatus::Contradicted
    );
    let refined = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap();
    assert_eq!(
        store
            .validate_warning_signature(&refined.id, &p, &n)
            .unwrap()
            .status,
        RiskIndicatorStatus::Validated
    );
    let current = retry_trajectory(&store, &f, true, None);
    let forecast = store.forecast_trajectory(&current.id).unwrap().remove(0);
    assert_eq!(forecast.horizon, ForecastHorizon::Actions(2));
    assert_eq!(forecast.evidence_kind, ForecastEvidenceKind::Correlational);
}

#[test]
fn precursor_discovery_creates_inactive_candidates_from_failure_control_differences() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    retry_trajectory(&store, &f, true, Some(failure()));
    retry_trajectory(&store, &f, true, Some(failure()));
    retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success));
    retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success));

    let candidates = store
        .discover_candidate_risk_indicators("retry-exhaustion")
        .unwrap();
    assert!(!candidates.is_empty());
    assert!(candidates.iter().all(|indicator| {
        indicator.status == RiskIndicatorStatus::Candidate
            && indicator.origin == PredictiveOrigin::Local
            && indicator.evidence.len() == 2
    }));
    let current = retry_trajectory(&store, &f, true, None);
    assert!(store.forecast_trajectory(&current.id).unwrap().is_empty());
}

#[test]
fn validation_rejects_duplicate_samples_and_never_uses_post_failure_conditions() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let signature = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            vec![feature(
                "retry_exhausted",
                ComparisonOperator::Equals,
                TrajectoryValue::Boolean(true),
            )],
            PredictiveOrigin::Local,
        ))
        .unwrap();
    assert!(
        store
            .validate_warning_signature(
                &signature.id,
                &[positives[0].id.clone(), positives[0].id.clone()],
                &negatives
                    .iter()
                    .map(|item| item.id.clone())
                    .collect::<Vec<_>>(),
            )
            .is_err()
    );
    let result = store
        .validate_warning_signature(
            &signature.id,
            &positives
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            &negatives
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(result.status, RiskIndicatorStatus::Candidate);
}

#[test]
fn causal_basis_strengthens_but_does_not_replace_observed_history() {
    let f = Fixture::new();
    let t = ExecutionTrajectory {
        id: TrajectoryId::new(),
        session_id: HardknockSessionId::new(),
        subject: TrajectorySubject::Task(TaskId::new()),
        task_family: None,
        started_at: Utc::now(),
        ended_at: None,
        completed_at: None,
        events: vec![],
        points: vec![],
        outcome: None,
        context: context(&f, "v1", true),
        fingerprint: Default::default(),
    };
    let id = CausalHypothesisId::new();
    let mut sig = signature(
        &f,
        "retry-exhaustion",
        vec![feature(
            "state_stale",
            ComparisonOperator::Equals,
            TrajectoryValue::Boolean(true),
        )],
        PredictiveOrigin::Local,
    );
    sig.status = RiskIndicatorStatus::Validated;
    sig.causal_basis = vec![id.clone()];
    let event = TrajectoryEvent {
        id: TrajectoryEventId::new(),
        trajectory_id: t.id.clone(),
        sequence: 0,
        timestamp: Utc::now(),
        kind: TrajectoryEventKind::StateObserved,
        observation: obs(&[("state_stale", TrajectoryValue::Boolean(true))]),
        evidence: vec![],
    };
    let forecasts = DeterministicForecastEngine
        .forecast(
            &t,
            &ForecastContext {
                events: vec![event],
                signatures: vec![sig],
                indicators: vec![],
                history: vec![],
                supported_causal_hypotheses: vec![id],
                causal_precursors: vec![],
                envelope_proximity: EnvelopeProximity::Unknown,
                risk: Severity::Medium,
                failure_trajectories: vec![],
                interventions: vec![],
                policy: ForecastPolicyConfig::default(),
                window: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(forecasts[0].evidence_kind, ForecastEvidenceKind::Causal);
    assert_eq!(forecasts[0].strength, ForecastStrength::Moderate);
}

#[test]
fn misses_and_insufficient_observability_are_explicit_curriculum_inputs() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let visible = retry_trajectory(
        &store,
        &f,
        true,
        Some(TrajectoryOutcome::Failure(FailureSignatureRef {
            signature: "new-failure".into(),
        })),
    );
    let miss = store.record_forecast_miss(&visible.id, None).unwrap();
    assert_eq!(miss.forecastability, Forecastability::NotYetPredictable);
    let hidden = start(&store, &f, "v1", false);
    store
        .finish_trajectory(
            &hidden.id,
            TrajectoryOutcome::Failure(FailureSignatureRef {
                signature: "hidden".into(),
            }),
        )
        .unwrap();
    let hidden = store
        .record_forecast_miss(&hidden.id, Some("remote state unavailable".into()))
        .unwrap();
    assert_eq!(
        hidden.forecastability,
        Forecastability::InsufficientObservability
    );
    let goals = store.predictive_curriculum_goals().unwrap();
    assert_eq!(goals.len(), 1);
    assert_eq!(
        goals[0].kind,
        hardknock::curriculum::CurriculumGoalKind::DiscoverEarlyWarning
    );
}

#[test]
fn false_alarm_feedback_degrades_and_quarantines_predictor_without_rewriting_history() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let id = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap()
        .id;
    store
        .validate_warning_signature(&id, &positives.map(|t| t.id), &negatives.map(|t| t.id))
        .unwrap();
    for _ in 0..2 {
        let t = retry_trajectory(&store, &f, true, None);
        let fc = store.forecast_trajectory(&t.id).unwrap().remove(0);
        store
            .finish_trajectory(&t.id, TrajectoryOutcome::Success)
            .unwrap();
        store
            .record_forecast_feedback(ForecastFeedback {
                id: ForecastFeedbackId::new(),
                forecast_id: fc.id,
                status: ForecastStatus::ResolvedFalseAlarm,
                outcome: ForecastOutcome::FalsePositive,
                observed_outcome: TrajectoryOutcome::Success,
                intervention: None,
                evidence: vec![],
                warning_lead_actions: None,
                lead: ForecastLead::default(),
                intervention_feasible: Some(true),
                created_at: Utc::now(),
            })
            .unwrap();
    }
    let health = store.forecast_health(&id).unwrap();
    assert_eq!(health.health, ForecastHealth::Degrading);
    assert!(health.revalidation_recommended);
    assert_eq!(
        store.warning_signature(&id).unwrap().status,
        RiskIndicatorStatus::Validated
    );
}

#[test]
fn federated_warning_is_advisory_until_distinct_local_reproduction() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let remote = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::FederatedAdvisory,
        ))
        .unwrap();
    let local = store.localize_federated_signature(&remote.id).unwrap();
    assert_ne!(local.id, remote.id);
    assert_eq!(local.origin, PredictiveOrigin::Local);
    assert_eq!(local.status, RiskIndicatorStatus::Candidate);
    assert!(
        store
            .validate_warning_signature(&remote.id, &[TrajectoryId::new()], &[TrajectoryId::new()])
            .is_err()
    );
}

#[test]
fn preventive_policy_respects_authority_and_avoids_high_cost_overreaction() {
    let f = Fixture::new();
    let mut runtime = RuntimeScenario::default().decision_context().unwrap();
    runtime.risk.severity = Severity::High;
    runtime.capability_context.commit_authority = false;
    let sig = EarlyWarningSignatureId::new();
    let forecast = FailureForecast {
        id: FailureForecastId::new(),
        trajectory_id: TrajectoryId::new(),
        failure: FailureSignatureRef {
            signature: "external-commit".into(),
        },
        matched_trajectory: None,
        horizon: ForecastHorizon::NextAction,
        evidence: vec![],
        indicators: vec![],
        matched_signals: vec![],
        missing_signals: vec![],
        recommended_interventions: vec![],
        signature: sig.clone(),
        causal_basis: vec![],
        evidence_kind: ForecastEvidenceKind::Correlational,
        strength: ForecastStrength::Moderate,
        status: ForecastStatus::Actionable,
        historical_matches: vec![],
        warning_sequence: 2,
        lead: ForecastLead::default(),
        matcher_version: "test-v1".into(),
        policy_version: "test-v1".into(),
        warning_revision: 1,
        causal_model_revision: None,
        advisory: false,
        created_at: Utc::now(),
    };
    let mut intervention = PreventiveIntervention {
        id: PreventiveInterventionId::new(),
        forecast: Some(forecast.id.clone()),
        signature: sig,
        action: InterventionAction::ReconcileEffect,
        target_failure: forecast.failure.clone(),
        evidence: vec![],
        status: PreventiveInterventionStatus::Validated,
        disruption: InterventionDisruption::Minimal,
        cost: InterventionCost::Low,
        reversibility: ReversibilityClass::Compensatable,
        externality: ExternalityClass::ExternalSystem,
        requires_commit_authority: true,
        scope: scope(&f),
        origin: PredictiveOrigin::Local,
        window: None,
        mechanism: None,
    };
    let decision = DeterministicPreventivePolicy.decide(
        &forecast,
        &PreventivePolicyContext {
            runtime: runtime.clone(),
            failure_severity: Severity::High,
            available_interventions: vec![intervention.clone()],
            false_positive_rate: Some(0.0),
            adequate_evidence_diversity: true,
        },
    );
    assert!(matches!(decision, PreventiveDecision::RequireApproval(_)));
    intervention.requires_commit_authority = false;
    intervention.cost = InterventionCost::High;
    runtime.risk.severity = Severity::Low;
    let decision = DeterministicPreventivePolicy.decide(
        &forecast,
        &PreventivePolicyContext {
            runtime,
            failure_severity: Severity::Low,
            available_interventions: vec![intervention],
            false_positive_rate: Some(0.0),
            adequate_evidence_diversity: true,
        },
    );
    assert!(matches!(decision, PreventiveDecision::Warn(_)));
}

#[test]
fn prepared_effect_expiry_is_forecast_from_observable_lifecycle_state() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let build = |age, outcome| {
        let t = start(&store, &f, "v1", true);
        add(
            &store,
            &t.id,
            TrajectoryEventKind::EffectPrepared,
            &[
                ("prepared_effect_age_ms", TrajectoryValue::Integer(age)),
                ("expiry_ms", TrajectoryValue::Integer(10000)),
            ],
        );
        if let Some(o) = outcome {
            store.finish_trajectory(&t.id, o).unwrap()
        } else {
            store.trajectory(&t.id).unwrap()
        }
    };
    let failure = TrajectoryOutcome::Failure(FailureSignatureRef {
        signature: "prepared-effect-expiry".into(),
    });
    let positives = [
        build(9000, Some(failure.clone())),
        build(9500, Some(failure)),
    ];
    let negatives = [
        build(1000, Some(TrajectoryOutcome::Success)),
        build(2000, Some(TrajectoryOutcome::Success)),
    ];
    let sig = store
        .register_warning_signature(signature(
            &f,
            "prepared-effect-expiry",
            vec![feature(
                "prepared_effect_age_ms",
                ComparisonOperator::AtLeast,
                TrajectoryValue::Integer(8000),
            )],
            PredictiveOrigin::Local,
        ))
        .unwrap();
    store
        .validate_warning_signature(&sig.id, &positives.map(|t| t.id), &negatives.map(|t| t.id))
        .unwrap();
    assert_eq!(
        store.forecast_trajectory(&build(8500, None).id).unwrap()[0]
            .failure
            .signature,
        "prepared-effect-expiry"
    );
}

#[tokio::test]
async fn controlled_prevention_benchmark_uses_real_experiments_and_meets_fast_path_target() {
    let f = Fixture::from_fixture("predictive/retry-exhaustion");
    let store = Store::open(&f.home).unwrap();
    let start = Instant::now();
    let report = hardknock::predictive::benchmark::run(
        &store,
        &Config::default(),
        &f.repo,
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(report["retry_exhaustion"]["control"], "FAIL");
    assert_eq!(report["retry_exhaustion"]["with_refresh"], "PASS");
    assert_eq!(
        report["retry_exhaustion"]["preventive_intervention"]["status"],
        "validated"
    );
    assert_eq!(report["prepared_effect"]["control"], "FAIL");
    assert_eq!(report["prepared_effect"]["with_reprepare"], "PASS");
    assert_eq!(
        report["prepared_effect"]["intervention"]["status"],
        "validated"
    );
    assert_eq!(
        report["false_positive_refinement"]["refined_false_positives"],
        0
    );
    assert_eq!(
        report["insufficient_observability"]["forecastability"],
        "insufficient_observability"
    );
    assert!(report["live_path_ms"].as_f64().unwrap() < 40.0);
    assert!(start.elapsed().as_secs() < 90);
}

#[test]
fn late_warning_has_zero_lead_and_is_not_counted_as_useful_prevention() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let sig = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap();
    store
        .validate_warning_signature(&sig.id, &positives.map(|t| t.id), &negatives.map(|t| t.id))
        .unwrap();
    let current = retry_trajectory(&store, &f, true, None);
    let fc = store.forecast_trajectory(&current.id).unwrap().remove(0);
    store.finish_trajectory(&current.id, failure()).unwrap();
    store
        .record_forecast_feedback(ForecastFeedback {
            id: ForecastFeedbackId::new(),
            forecast_id: fc.id,
            status: ForecastStatus::ResolvedFailureOccurred,
            outcome: ForecastOutcome::FailureMaterialized,
            observed_outcome: failure(),
            intervention: None,
            evidence: vec![],
            warning_lead_actions: Some(0),
            lead: ForecastLead {
                event_distance: Some(0),
                duration_ms: None,
            },
            intervention_feasible: Some(false),
            created_at: Utc::now(),
        })
        .unwrap();
    assert_eq!(
        store
            .forecast_quality()
            .unwrap()
            .median_warning_lead_actions,
        None,
        "one late warning is below the minimum calibration sample size"
    );
}

#[test]
fn version_drift_is_stale_in_the_new_context_without_mutating_validated_history() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let sig = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap();
    store
        .validate_warning_signature(&sig.id, &positives.map(|t| t.id), &negatives.map(|t| t.id))
        .unwrap();
    let assessment = store
        .forecast_health_for_context(&sig.id, &context(&f, "v2", true))
        .unwrap();
    assert_eq!(assessment.health, ForecastHealth::Stale);
    assert!(assessment.revalidation_recommended);
    assert_eq!(
        store.warning_signature(&sig.id).unwrap().status,
        RiskIndicatorStatus::Validated
    );
}

#[test]
fn runtime_forecast_changes_adaptive_decision_but_never_overrides_hard_policy() {
    let f = Fixture::new();
    let mut context = RuntimeScenario::default().decision_context().unwrap();
    let signature = EarlyWarningSignatureId::new();
    let forecast = FailureForecast {
        id: FailureForecastId::new(),
        trajectory_id: TrajectoryId::new(),
        failure: FailureSignatureRef {
            signature: "retry-exhaustion".into(),
        },
        matched_trajectory: None,
        horizon: ForecastHorizon::Actions(2),
        evidence: vec![],
        indicators: vec![],
        matched_signals: vec![],
        missing_signals: vec![],
        recommended_interventions: vec![],
        signature: signature.clone(),
        causal_basis: vec![],
        evidence_kind: ForecastEvidenceKind::Correlational,
        strength: ForecastStrength::Strong,
        status: ForecastStatus::Actionable,
        historical_matches: vec![],
        warning_sequence: 2,
        lead: ForecastLead::default(),
        matcher_version: "test-v1".into(),
        policy_version: "test-v1".into(),
        warning_revision: 1,
        causal_model_revision: None,
        advisory: false,
        created_at: Utc::now(),
    };
    let intervention = PreventiveIntervention {
        id: PreventiveInterventionId::new(),
        forecast: Some(forecast.id.clone()),
        signature,
        action: InterventionAction::RefreshAuthoritativeState,
        target_failure: forecast.failure.clone(),
        evidence: vec![],
        status: PreventiveInterventionStatus::Validated,
        disruption: InterventionDisruption::Minimal,
        cost: InterventionCost::Low,
        reversibility: ReversibilityClass::NaturallyReversible,
        externality: ExternalityClass::HostLocal,
        requires_commit_authority: false,
        scope: scope(&f),
        origin: PredictiveOrigin::Local,
        window: None,
        mechanism: None,
    };
    context.active_forecasts = vec![forecast];
    context.preventive_interventions = vec![intervention];
    let evaluation = DeterministicRuntimeController::with_config(RuntimePolicyConfig {
        autonomy: RuntimeAutonomy::Adaptive,
        forecast: RuntimeForecastConfig {
            mode: ForecastRuntimeMode::Prevent,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap()
    .evaluate(&context)
    .unwrap();
    assert_eq!(evaluation.decision.kind(), RuntimeDecisionKind::Replan);
    assert!(matches!(
        evaluation.reasons[0],
        DecisionReason::ForecastedFailure { .. }
    ));
    context.capability_context.governance.hard_policy_blocked = true;
    context.capability_context.governance.block_reason = Some("blocked".into());
    let blocked = DeterministicRuntimeController::with_config(RuntimePolicyConfig {
        autonomy: RuntimeAutonomy::Adaptive,
        forecast: RuntimeForecastConfig {
            mode: ForecastRuntimeMode::Prevent,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap()
    .evaluate(&context)
    .unwrap();
    assert_eq!(blocked.decision.kind(), RuntimeDecisionKind::Abstain);
    assert!(
        blocked
            .reasons
            .contains(&DecisionReason::HardPolicyPrecedence)
    );
}

#[test]
fn revisions_are_append_only_candidates_until_revalidated() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let sig = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            vec![feature(
                "retry_count",
                ComparisonOperator::AtLeast,
                TrajectoryValue::Integer(2),
            )],
            PredictiveOrigin::Local,
        ))
        .unwrap();
    let rev = store
        .propose_warning_revision(&sig.id, retry_conditions(), vec![])
        .unwrap();
    assert_eq!(rev.revision, 2);
    assert_eq!(store.warning_signature_history(&sig.id).unwrap().len(), 2);
    assert_eq!(store.warning_signature(&sig.id).unwrap().revision, 1);
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute(
            "UPDATE early_warning_revisions SET revision=9 WHERE id=?1",
            [rev.id.to_string()]
        )
        .is_err()
    );
}

#[test]
fn forecasting_fast_path_has_no_network_or_model_dependency() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let positives = [
        retry_trajectory(&store, &f, true, Some(failure())),
        retry_trajectory(&store, &f, true, Some(failure())),
    ];
    let negatives = [
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
        retry_trajectory(&store, &f, false, Some(TrajectoryOutcome::Success)),
    ];
    let sig = store
        .register_warning_signature(signature(
            &f,
            "retry-exhaustion",
            retry_conditions(),
            PredictiveOrigin::Local,
        ))
        .unwrap();
    store
        .validate_warning_signature(&sig.id, &positives.map(|t| t.id), &negatives.map(|t| t.id))
        .unwrap();
    let current = retry_trajectory(&store, &f, true, None);
    let start = Instant::now();
    for _ in 0..100 {
        assert_eq!(store.forecast_trajectory(&current.id).unwrap().len(), 1);
    }
    assert!(start.elapsed().as_millis() < 4000);
}

#[test]
fn predictive_cli_exposes_required_inspection_and_rejects_unacknowledged_execution() {
    let f = Fixture::from_fixture("predictive/retry-exhaustion");
    let rejected = f
        .command()
        .arg("--json")
        .args(["forecast", "benchmark"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("trusted-local"));
    let report = f.cli(&["forecast", "benchmark", "--trusted-local"], 0);
    assert_eq!(report["event"], "predictive");
    assert_eq!(report["result"]["retry_exhaustion"]["with_refresh"], "PASS");
    let listed = f.cli(&["trajectory", "list"], 0);
    let id = listed["result"]["trajectories"][0]["id"].as_str().unwrap();
    let shown = f.cli(&["trajectory", "show", id], 0);
    assert!(shown["result"]["events"].as_array().unwrap().len() >= 3);
    let quality = f.cli(&["forecast", "quality"], 0);
    assert!(
        quality["result"]["quality"]["resolved_forecasts"]
            .as_u64()
            .unwrap()
            >= 2
    );
}
