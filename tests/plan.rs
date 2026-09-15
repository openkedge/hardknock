// SPDX-License-Identifier: Apache-2.0
use chrono::Utc;
use hardknock::{
    assurance::{BehavioralCondition, PredicateOperator},
    composition::*,
    core::*,
    curriculum::Severity,
    hierarchy::{FreshnessStatus, ScopeValue},
    plan::*,
    runtime::*,
    store::{RuntimeStore, Store},
};
use std::{collections::BTreeMap, time::Duration};
fn context() -> RuntimeDecisionContext {
    serde_json::from_str::<RuntimeScenario>(include_str!(
        "../fixtures/runtime-scenarios/known-safe.json"
    ))
    .unwrap()
    .decision_context()
    .unwrap()
}
fn predicate() -> BehavioralCondition {
    BehavioralCondition::StatePredicate {
        path: "ready".into(),
        operator: PredicateOperator::Equals,
        value: serde_json::json!(true),
    }
}
fn plan() -> ExecutionPlan {
    let step = PlanStepId::new();
    let assumption = PlanAssumptionId::new();
    ExecutionPlan {
        id: ExecutionPlanId::new(),
        goal: PlanGoal {
            description: "Bounded observation".into(),
            family: None,
        },
        revision: 1,
        steps: vec![PlanStep {
            id: step.clone(),
            kind: PlanStepKind::Observe(ObservationSpec {
                key: "ready".into(),
                condition: predicate(),
                freshness: StateFreshnessRequirement::None,
            }),
            dependencies: vec![],
            required_assumptions: vec![assumption.clone()],
            required_invariants: vec![],
            expected_observations: vec![],
            status: PlanStepStatus::Pending,
            severity: Severity::High,
            required_capabilities: vec![],
        }],
        assumptions: vec![PlanAssumption {
            id: assumption,
            statement: "Backend ready".into(),
            predicate: predicate(),
            source: AssumptionSource::InitialObservation,
            required_by: vec![step],
            validity: AssumptionValidity::Supported,
            freshness: AssumptionFreshness {
                observed_at: Utc::now(),
                requirement: StateFreshnessRequirement::None,
                status: FreshnessStatus::Fresh,
            },
            severity: Severity::High,
            evidence: vec![],
        }],
        invariants: vec![],
        checkpoints: vec![],
        commitment_points: vec![],
        commitment_gates: vec![],
        knowledge_dependencies: vec![],
        freshness_policy: Default::default(),
        component_revisions: BTreeMap::new(),
        status: PlanStatus::Proposed,
        created_at: Utc::now(),
    }
}
fn claim(value: bool) -> StateClaim {
    StateClaim {
        key: "ready".into(),
        value: ScopeValue::Boolean(value),
        source: StateClaimSource::RuntimeObservation,
        freshness: StateFreshness {
            observed_at: Utc::now(),
            expires_at: None,
            step: None,
            external_version: None,
        },
    }
}
fn evaluate(p: &ExecutionPlan, s: &PlanState) -> PlanValidityAssessment {
    DeterministicPlanValidityEvaluator {
        inputs: Default::default(),
    }
    .evaluate(p, s, &context())
    .unwrap()
}
#[test]
fn absent_and_agent_claims_cannot_refresh_assumptions() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
    let mut c = claim(true);
    c.source = StateClaimSource::AgentReported;
    s.observations.push(c);
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
}
#[test]
fn stale_truth_requires_verification_not_replan() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    let mut c = claim(true);
    c.freshness.observed_at = Utc::now() - chrono::Duration::hours(1);
    s.observations.push(c);
    let a = evaluate(&p, &s);
    assert_eq!(a.status, PlanValidityStatus::VerificationRequired);
    assert_eq!(a.assumptions[0].validity, AssumptionValidity::Stale);
}
#[test]
fn relevant_drift_replans_and_refresh_can_restore_validity() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(false));
    assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::ReplanRequired);
    s.observations = vec![claim(true)];
    assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::Valid);
}
#[test]
fn irrelevant_drift_does_not_interrupt() {
    let mut p = plan();
    let mut a = p.assumptions[0].clone();
    a.id = PlanAssumptionId::new();
    a.required_by.clear();
    a.predicate = BehavioralCondition::Custom {
        kind: "unrelated".into(),
        payload: serde_json::Value::Null,
    };
    p.assumptions.push(a);
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::Valid);
}
#[test]
fn repeated_mutations_require_new_observation() {
    let mut p = plan();
    p.assumptions[0].freshness.requirement = StateFreshnessRequirement::BeforeNextMutation;
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    s.observation_epochs.insert("ready".into(), 0);
    for epoch in 1..=5 {
        s.mutation_epoch = epoch;
        assert_eq!(
            evaluate(&p, &s).status,
            PlanValidityStatus::VerificationRequired
        );
    }
    s.observation_epochs.insert("ready".into(), 5);
    assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::Valid);
}
#[test]
fn external_version_drift_invalidates_truth() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    let mut c = claim(true);
    c.freshness.external_version = Some(ExternalStateVersion {
        resource: "api".into(),
        version: "v1".into(),
    });
    s.observations.push(c);
    s.external_versions.insert("api".into(), "v2".into());
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
}
#[test]
fn invariant_failure_is_not_overridden_by_true_assumption() {
    let mut p = plan();
    p.invariants.push(PlanInvariant {
        id: PlanInvariantId::new(),
        condition: BehavioralCondition::StatePredicate {
            path: "ready".into(),
            operator: PredicateOperator::Equals,
            value: serde_json::json!(false),
        },
        scope: PlanInvariantScope::EntirePlan,
        severity: Severity::High,
        evidence: vec![],
    });
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::ReplanRequired);
}
#[test]
fn stable_plans_scale_to_one_hundred_steps() {
    for size in [10, 25, 50, 100] {
        let mut p = plan();
        for _ in 1..size {
            let mut step = p.steps[0].clone();
            step.id = PlanStepId::new();
            step.dependencies = vec![p.steps.last().unwrap().id.clone()];
            p.steps.push(step);
        }
        let mut s = PlanState::initial(&p);
        s.observations.push(claim(true));
        let started = std::time::Instant::now();
        for step in &p.steps {
            s.current_step = Some(step.id.clone());
            assert_eq!(evaluate(&p, &s).status, PlanValidityStatus::Valid);
            s.completed_steps.push(step.id.clone());
        }
        eprintln!(
            "plan steps={size}, total_us={}",
            started.elapsed().as_micros()
        );
    }
}
#[test]
fn revisions_and_runtime_publication_are_bound_to_live_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let p = store
        .save_execution_plan(&plan(), PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    store
        .record_plan_observation(
            &run.id,
            &PlanObservation {
                id: PlanObservationId::new(),
                source: StateClaimSource::RuntimeObservation,
                claims: vec![claim(true)],
                captured_at: Utc::now(),
                attestation: None,
            },
        )
        .unwrap();
    let ctx = store.plan_runtime_context(&run.id, context()).unwrap();
    let record = store
        .record_runtime_decision(&ctx, Default::default())
        .unwrap();
    assert_eq!(
        record
            .context
            .plan
            .as_ref()
            .unwrap()
            .validity
            .as_ref()
            .unwrap()
            .status,
        PlanValidityStatus::Valid
    );
    let mut next = p.clone();
    next.revision = 2;
    next.goal.description = "Revised goal".into();
    store
        .save_execution_plan(&next, PlanRevisionReason::UserChange)
        .unwrap();
    assert!(
        store
            .persist_runtime_decision(&record, Default::default())
            .is_err()
    );
    assert_eq!(
        store.plan_revision(&p.id, 1).unwrap().goal.description,
        p.goal.description
    );
}
#[test]
fn hard_policy_wins_over_valid_plan() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    let mut ctx = context();
    ctx.plan = Some(PlanRuntimeContext {
        run: PlanRunId::new(),
        plan: p.id.clone(),
        revision: 1,
        next_step: p.steps[0].id.clone(),
        validity: Some(evaluate(&p, &s)),
        crossed_commitments: vec![],
    });
    ctx.capability_context.governance.hard_policy_blocked = true;
    assert_ne!(
        DeterministicRuntimeController::default()
            .evaluate(&ctx)
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Act
    );
}
#[test]
fn backwards_declared_dag_starts_at_root() {
    let mut p = plan();
    let mut first = p.steps[0].clone();
    first.id = PlanStepId::new();
    p.steps[0].dependencies = vec![first.id.clone()];
    p.steps.push(first.clone());
    validate_plan(&p).unwrap();
    assert_eq!(PlanState::initial(&p).current_step, Some(first.id));
}
#[test]
fn explicit_max_age_is_enforced() {
    let mut p = plan();
    p.assumptions[0].freshness.requirement =
        StateFreshnessRequirement::MaxAge(Duration::from_secs(1));
    let mut s = PlanState::initial(&p);
    let mut c = claim(true);
    c.freshness.observed_at = Utc::now() - chrono::Duration::seconds(2);
    s.observations.push(c);
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
}
fn commitment(p: &mut ExecutionPlan) {
    let id = PlanCommitmentPointId::new();
    p.commitment_points.push(PlanCommitmentPoint {
        id: id.clone(),
        after_step: p.steps[0].id.clone(),
        consequences: vec![],
        assumptions_invalidated: vec![],
        recoveries_lost: vec![],
        new_recovery_requirements: vec![],
    });
    p.commitment_gates.push(CommitmentGate {
        commitment_point: id,
        required_assumptions: vec![p.assumptions[0].id.clone()],
        required_invariants: vec![],
        required_approvals: vec![],
        required_recoveries: vec![],
        diversity_claim: None,
        minimum_diversity: None,
    });
}
#[test]
fn unknown_rollback_blocks_commitment() {
    let mut p = plan();
    commitment(&mut p);
    p.commitment_gates[0]
        .required_recoveries
        .push(RecoveryId::new());
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    assert!(
        evaluate(&p, &s)
            .blockers
            .contains(&PlanValidityBlocker::MissingRecovery)
    );
}
#[test]
fn commitment_requires_fresher_state() {
    let mut p = plan();
    commitment(&mut p);
    let mut s = PlanState::initial(&p);
    let mut c = claim(true);
    c.freshness.observed_at = Utc::now() - chrono::Duration::seconds(90);
    s.observations.push(c);
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
}
#[test]
fn unbound_approval_blocks_commitment() {
    let mut p = plan();
    commitment(&mut p);
    p.commitment_gates[0]
        .required_approvals
        .push(ApprovalRequirement {
            id: "approval".into(),
            effects: vec![EffectId::new()],
            max_age: Some(Duration::from_secs(60)),
        });
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    assert!(
        evaluate(&p, &s)
            .blockers
            .contains(&PlanValidityBlocker::ApprovalRequired)
    );
}
#[test]
fn common_mode_evidence_blocks_commitment() {
    let mut p = plan();
    commitment(&mut p);
    let mut s = PlanState::initial(&p);
    s.observations.push(claim(true));
    let mut inputs = PlanEvaluationInputs::default();
    inputs
        .low_diversity
        .insert(p.commitment_points[0].id.clone());
    assert!(
        DeterministicPlanValidityEvaluator { inputs }
            .evaluate(&p, &s, &context())
            .unwrap()
            .blockers
            .contains(&PlanValidityBlocker::InsufficientDiversity)
    );
}
#[test]
fn checkpoint_resume_reassesses_current_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut p = plan();
    let checkpoint = PlanCheckpointId::new();
    p.checkpoints.push(PlanCheckpoint {
        id: checkpoint.clone(),
        after_step: None,
        required_observations: vec![ObservationSpec {
            key: "ready".into(),
            condition: predicate(),
            freshness: StateFreshnessRequirement::None,
        }],
        assumptions_to_revalidate: vec![p.assumptions[0].id.clone()],
        invariants_to_verify: vec![],
        decision_required: true,
    });
    store
        .save_execution_plan(&p, PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    let observe = |value| {
        store
            .record_plan_observation(
                &run.id,
                &PlanObservation {
                    id: PlanObservationId::new(),
                    source: StateClaimSource::RuntimeObservation,
                    claims: vec![claim(value)],
                    captured_at: Utc::now(),
                    attestation: None,
                },
            )
            .unwrap()
    };
    observe(true);
    let saved = store
        .capture_plan_checkpoint(&run.id, &checkpoint, &context())
        .unwrap();
    assert_eq!(
        store
            .resume_plan_checkpoint(&run.id, &saved.id, &context())
            .unwrap()
            .status,
        PlanValidityStatus::Valid
    );
    observe(false);
    assert_eq!(
        store
            .resume_plan_checkpoint(&run.id, &saved.id, &context())
            .unwrap()
            .status,
        PlanValidityStatus::ReplanRequired
    );
    assert_eq!(
        store
            .plan_checkpoint_snapshot(&saved.id)
            .unwrap()
            .state_claims[0]
            .value,
        ScopeValue::Boolean(true)
    );
}
#[test]
fn equal_trust_contradictions_remain_unknown() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    s.observations = vec![claim(true), claim(false)];
    assert_eq!(
        evaluate(&p, &s).status,
        PlanValidityStatus::VerificationRequired
    );
}
#[test]
fn future_observation_is_not_evidence() {
    let p = plan();
    let mut s = PlanState::initial(&p);
    let mut c = claim(true);
    c.freshness.observed_at = Utc::now() + chrono::Duration::hours(1);
    s.observations.push(c);
    assert_ne!(evaluate(&p, &s).status, PlanValidityStatus::Valid);
}
#[test]
fn cyclic_plan_is_rejected() {
    let mut p = plan();
    p.steps[0].dependencies = vec![p.steps[0].id.clone()];
    assert!(validate_plan(&p).is_err());
}

mod support;
#[tokio::test]
async fn controlled_rollout_compares_static_checkpoint_and_adaptive() {
    use hardknock::experimentation::*;
    let fixture = support::Fixture::from_fixture("plan-dependency-drift");
    let store = Store::open(&fixture.home).unwrap();
    let request = ExperimentRequest {
        id: ExperimentRequestId::new(),
        session_id: "plan-drift-fixture".into(),
        question: "Does fresh dependency checking with bounded rerouting avoid rollout failure?"
            .into(),
        hypothesis: None,
        candidates: ["static", "checkpoint", "adaptive"]
            .into_iter()
            .map(|mode| ExperimentCandidate {
                id: CandidateId::new(),
                name: mode.into(),
                description: "Deterministic deployment fixture".into(),
                execution: CandidateExecution::Shell {
                    commands: vec![format!("sh rollout.sh {mode}")],
                },
                expected_outcome: None,
            })
            .collect(),
        starting_state: ExperimentStartingState {
            state_ref: hardknock::dojo::capture_state(&fixture.repo).unwrap(),
            expected_fingerprint: None,
            parent_reality: None,
            source: SnapshotSource::RepositoryCommit,
        },
        evaluator: hardknock::evaluation::EvaluationSpec {
            checks: vec!["test \"$(cat result)\" = passed".into()],
        },
        budget: Default::default(),
        requested_by: AgentIdentity {
            kind: "fixture".into(),
            executable: "sh".into(),
            version: None,
            model: None,
        },
        created_at: Utc::now(),
        criteria: Default::default(),
        origin: ExperimentOrigin::User,
        intent: ExperimentIntent::CompareStrategies,
        capabilities: Default::default(),
    };
    let config = hardknock::bridge::config::Config::default();
    let result = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(request, &hardknock::cancellation::Cancellation::default())
    .await
    .unwrap();
    assert_eq!(
        result.status,
        ExperimentStatus::Completed,
        "{:?}",
        result.failure
    );
    let evidence = result.result.unwrap();
    assert_eq!(evidence.quality, ExperimentQuality::Controlled);
    let results: Vec<_> = evidence
        .candidates
        .iter()
        .map(|c| c.evaluation.success)
        .collect();
    assert_eq!(results, vec![false, false, true]);
    assert_eq!(evidence.created_experience.len(), 3);
    assert!(
        evidence
            .candidates
            .windows(2)
            .all(|p| p[0].starting_fingerprint == p[1].starting_fingerprint)
    );
}

#[test]
fn recorded_failure_requires_recovery_and_replan_preserves_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let p = store
        .save_execution_plan(&plan(), PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    store
        .record_plan_observation(
            &run.id,
            &PlanObservation {
                id: PlanObservationId::new(),
                source: StateClaimSource::RuntimeObservation,
                claims: vec![claim(true)],
                captured_at: Utc::now(),
                attestation: None,
            },
        )
        .unwrap();
    store
        .reconcile_plan_failure(&run.id, "Observed task failure")
        .unwrap();
    assert_eq!(
        store
            .assess_plan_run(&run.id, &context(), false)
            .unwrap()
            .status,
        PlanValidityStatus::RecoveryRequired
    );
    let mut revision = p.clone();
    revision.revision = 2;
    store
        .save_execution_plan(&revision, PlanRevisionReason::FailedStep)
        .unwrap();
    let resumed = store
        .replan_run(&run.id, PlanRevisionReason::FailedStep)
        .unwrap();
    assert_eq!(resumed.plan.revision, 2);
    assert_eq!(resumed.outcome, None);
    assert_eq!(
        store.plan_run(&run.id).unwrap().outcome,
        Some(PlanRunOutcome::Failure)
    );
    assert_eq!(store.plan_revision(&p.id, 1).unwrap().revision, 1);
}
#[test]
fn duplicate_observation_keys_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let p = store
        .save_execution_plan(&plan(), PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    assert!(
        store
            .record_plan_observation(
                &run.id,
                &PlanObservation {
                    id: PlanObservationId::new(),
                    source: StateClaimSource::RuntimeObservation,
                    claims: vec![claim(false), claim(true)],
                    captured_at: Utc::now(),
                    attestation: None
                }
            )
            .is_err()
    );
    assert!(
        store
            .plan_run(&run.id)
            .unwrap()
            .state
            .observations
            .is_empty()
    );
}
#[test]
fn observation_step_records_completion_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let p = store
        .save_execution_plan(&plan(), PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    store
        .record_plan_observation(
            &run.id,
            &PlanObservation {
                id: PlanObservationId::new(),
                source: StateClaimSource::RuntimeObservation,
                claims: vec![claim(true)],
                captured_at: Utc::now(),
                attestation: None,
            },
        )
        .unwrap();
    let ctx = store.plan_runtime_context(&run.id, context()).unwrap();
    let decision = store
        .record_runtime_decision(&ctx, Default::default())
        .unwrap();
    assert_eq!(decision.decision.kind(), RuntimeDecisionKind::Act);
    let record = PlanStepRun {
        run: run.id,
        step: p.steps[0].id.clone(),
        decision: decision.id,
        attestation: None,
        receipts: vec![],
        completed_at: Utc::now(),
        composition_evidence: None,
        experiment: None,
    };
    assert_eq!(
        store.complete_plan_step(&record).unwrap().outcome,
        Some(PlanRunOutcome::Success)
    );
    assert!(store.complete_plan_step(&record).is_err());
}

#[test]
fn expired_plan_fact_is_removed_from_reused_runtime_context() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let p = store
        .save_execution_plan(&plan(), PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    let observe = |c| {
        store
            .record_plan_observation(
                &run.id,
                &PlanObservation {
                    id: PlanObservationId::new(),
                    source: StateClaimSource::RuntimeObservation,
                    claims: vec![c],
                    captured_at: Utc::now(),
                    attestation: None,
                },
            )
            .unwrap()
    };
    observe(claim(true));
    let mut ctx = store.plan_runtime_context(&run.id, context()).unwrap();
    assert!(ctx.context_observations.contains_key("ready"));
    let mut expired = claim(true);
    expired.freshness.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    observe(expired);
    store.attach_plan_identity(&mut ctx).unwrap();
    assert!(!ctx.context_observations.contains_key("ready"));
}

#[test]
fn commitment_cannot_ignore_unknown_low_severity_invariant() {
    let mut p = plan();
    commitment(&mut p);
    let id = PlanInvariantId::new();
    p.invariants.push(PlanInvariant {
        id: id.clone(),
        condition: BehavioralCondition::StatePredicate {
            path: "cleanup_verified".into(),
            operator: PredicateOperator::Equals,
            value: serde_json::json!(true),
        },
        scope: PlanInvariantScope::EntirePlan,
        severity: Severity::Low,
        evidence: vec![],
    });
    p.commitment_gates[0].required_invariants.push(id);
    let mut state = PlanState::initial(&p);
    state.observations.push(claim(true));
    assert_eq!(
        evaluate(&p, &state).status,
        PlanValidityStatus::VerificationRequired
    );
}

#[test]
fn partial_effect_survives_replan_that_replaces_unfinished_step() {
    use hardknock::effects::*;
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let external = MockExternalSystem::new(temp.path()).unwrap();
    let manager = EffectManager::new(&store).unwrap();
    let mut effects = vec![];
    for name in ["first", "second"] {
        let target = format!("mock://plan/{name}");
        external
            .seed("mock-http", &target, &serde_json::json!({"version":1}))
            .unwrap();
        let request = EffectRequest {
            session_id: "plan-fixture".into(),
            reality_id: None,
            source_action: ActionRef {
                id: name.into(),
                kind: "fixture".into(),
            },
            kind: EffectKind::HttpApi,
            target: EffectTarget { uri: target },
            operation: EffectOperation::Update,
            payload: serde_json::json!({"version":2}),
            adapter: None,
            evidence: vec!["plan-partial-fixture".into()],
            fault: EffectFault::None,
        };
        effects.push(
            manager
                .propose_and_prepare(request, &EffectManager::user_context())
                .unwrap()
                .0
                .id,
        );
    }
    let effect_plan = manager
        .create_plan(effects.clone(), vec![], EffectAtomicity::BestEffortGroup)
        .unwrap();
    let mut p = plan();
    p.steps[0].kind = PlanStepKind::Effect(effect_plan.id);
    let p = store
        .save_execution_plan(&p, PlanRevisionReason::UserChange)
        .unwrap();
    let run = store.start_plan_run(&p.id).unwrap();
    let authorization = manager
        .authorize(CommitAuthority::User, &effects[..1])
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &effects[0],
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Committed { .. }
    ));
    let recovery = store
        .reconcile_plan_failure(&run.id, "Second operation did not complete")
        .unwrap();
    assert_eq!(recovery.committed_effects, vec![effects[0].clone()]);
    let mut revised = p.clone();
    revised.revision = 2;
    revised.component_revisions.clear();
    revised.steps[0].kind = PlanStepKind::Observe(ObservationSpec {
        key: "ready".into(),
        condition: predicate(),
        freshness: StateFreshnessRequirement::None,
    });
    store
        .save_execution_plan(&revised, PlanRevisionReason::FailedStep)
        .unwrap();
    let next = store
        .replan_run(&run.id, PlanRevisionReason::FailedStep)
        .unwrap();
    assert_eq!(
        store
            .plan_recovery_context(&next.id)
            .unwrap()
            .committed_effects,
        vec![effects[0].clone()]
    );
    assert_eq!(
        external
            .resource("mock-http", "mock://plan/first")
            .unwrap()
            .mutation_count,
        1
    );
    assert_eq!(
        external
            .resource("mock-http", "mock://plan/second")
            .unwrap()
            .mutation_count,
        0
    );
}
