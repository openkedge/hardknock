// SPDX-License-Identifier: Apache-2.0
mod support;
use chrono::Utc;
use hardknock::{
    assurance::{BehavioralCondition, PredicateOperator},
    capability::*,
    composition::*,
    core::*,
    curriculum::Severity,
    hierarchy::*,
    store::{Store, ToolStore},
    tool::*,
    tool_runtime::HostMicroSandboxProvider,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
fn condition(key: &str, value: serde_json::Value) -> BehavioralCondition {
    BehavioralCondition::StatePredicate {
        path: key.into(),
        operator: PredicateOperator::Equals,
        value,
    }
}
fn fact(key: &str, value: ScopeValue) -> StateClaim {
    StateClaim {
        key: key.into(),
        value,
        source: StateClaimSource::RuntimeObservation,
        freshness: StateFreshness {
            observed_at: Utc::now(),
            expires_at: None,
            step: None,
            external_version: None,
        },
    }
}
fn minimal() -> CapabilityManifest {
    let mut p = builtin_profile("coding-offline").unwrap();
    p.filesystem.writable.clear();
    p.process.allow_exec = false;
    p.process.allowed_executables.clear();
    p.environment.values.clear();
    p.environment.readable.clear();
    p
}
fn definition(f: &support::Fixture, mode: &str) -> ToolDefinition {
    let script = f.repo.join("operation.py");
    if !script.exists() {
        fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/composition/credential-rollout/operation.py"),
            &script,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        std::process::Command::new("git")
            .args(["add", "operation.py"])
            .current_dir(&f.repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "composition fixture"])
            .current_dir(&f.repo)
            .output()
            .unwrap();
    }
    let mut t = builtin_tools()
        .into_iter()
        .find(|t| t.name == "run-tests")
        .unwrap();
    t.id = ToolId::new();
    t.name = mode.into();
    t.invocation = ToolInvocation::NativeBinary {
        executable: script.display().to_string(),
        args_template: vec![mode.into(), "{input}".into()],
    };
    t.capabilities.process.allowed_executables =
        vec![ExecutablePattern(script.display().to_string())];
    t.capabilities.filesystem = ToolFilesystemCapabilities {
        read: vec!["$WORKSPACE/**".into()],
        write: vec!["$WORKSPACE/**".into()],
    };
    t.inputs = ToolInputSchema::default();
    t.integrity.artifact_hash = Some(
        blake3::hash(&fs::read(script).unwrap())
            .to_hex()
            .to_string(),
    );
    t.integrity.manifest_hash = t.manifest_hash().unwrap();
    t
}
fn setup(modes: &[&str]) -> (support::Fixture, Store, Composition) {
    let f = support::Fixture::new();
    let s = Store::open(&f.home).unwrap();
    let mut steps = vec![];
    for mode in modes {
        let t = definition(&f, mode);
        s.insert_tool_definition(&t).unwrap();
        let mut capabilities = builtin_profile("coding-offline").unwrap();
        if let ToolInvocation::NativeBinary { executable, .. } = &t.invocation {
            capabilities
                .process
                .allowed_executables
                .push(ExecutablePattern(executable.clone()));
        }
        steps.push(CompositionStep {
            id: CompositionStepId::new(),
            component: s
                .pin_composition_component(&ComposableArtifactRef::Tool(t.id))
                .unwrap(),
            input_bindings: vec![],
            expected_outputs: vec![],
            preconditions: vec![],
            invariants: vec![],
            local_recovery: None,
            capabilities,
        });
    }
    let relations = steps
        .windows(2)
        .map(|p| CompositionRelation {
            from: p[0].id.clone(),
            to: p[1].id.clone(),
            kind: CompositionRelationKind::Before,
        })
        .collect();
    let now = Utc::now();
    let c = Composition {
        id: CompositionId::new(),
        revision: 1,
        name: "fixture-composition".into(),
        scope: KnowledgeScope::default(),
        steps,
        relations,
        contract: CompositionContract {
            preconditions: vec![],
            postconditions: vec![],
            sequence_invariants: vec![],
            forbidden_outcomes: vec![],
            capability_policy: CompositionCapabilityPolicy {
                persistent_capabilities: minimal(),
                allow_sensitive_handoffs: false,
            },
            effect_policy: CompositionEffectPlan {
                effects: vec![],
                commit_points: vec![],
                compensation_edges: vec![],
                atomicity: CompositionAtomicity::PerStep,
            },
        },
        maturity: CompositionMaturity::Candidate,
        evidence: vec![],
        assumptions: vec![],
        cross_step_preconditions: vec![],
        recoveries: vec![],
        created_at: now,
        updated_at: now,
    };
    (f, s, c)
}
fn request(f: &support::Fixture, c: &Composition) -> CompositionExperimentRequest {
    let state = hardknock::dojo::capture_state(&f.repo).unwrap();
    CompositionExperimentRequest {
        composition: c.id.clone(),
        revision: c.revision,
        starting_state: composition_starting_proof(c, &state).unwrap(),
        initial_state: vec![],
        step_inputs: BTreeMap::new(),
        failure_injections: vec![],
        evaluation: CompositionEvaluationSpec {
            evaluator: "state-v1".into(),
            checks: vec![condition("ok", json!(true))],
            required_failure_points: vec![],
        },
        budget: hardknock::budget::ExperienceBudget {
            max_realities: c.steps.len(),
            ..Default::default()
        },
    }
}
fn binding(key: &str, from: Option<CompositionStepId>) -> StateBinding {
    StateBinding {
        key: key.into(),
        source_key: key.into(),
        from,
        resource: CapabilityFlowResource::ExternalStateHandle,
        allowed: true,
    }
}
#[test]
fn bounded_dag_and_step_identity_are_checked() {
    let (_f, _s, mut c) = setup(&["rotate", "deploy", "rollback"]);
    assert_eq!(ordered_steps(&c).unwrap().len(), 3);
    c.relations.push(CompositionRelation {
        from: c.steps[2].id.clone(),
        to: c.steps[0].id.clone(),
        kind: CompositionRelationKind::Before,
    });
    assert!(ordered_steps(&c).is_err());
    c.relations.pop();
    c.steps[1].id = c.steps[0].id.clone();
    assert!(ordered_steps(&c).is_err());
}
#[test]
fn static_compatibility_does_not_promote_components() {
    let (_f, s, c) = setup(&["safe"]);
    let report = DefaultCompositionPreflightAnalyzer
        .analyze(&c, &KnowledgeContext::default())
        .unwrap();
    assert_eq!(report.status, CompositionCompatibilityStatus::Compatible);
    s.save_composition(&c).unwrap();
    assert_eq!(
        s.composition_maturity(&c).unwrap(),
        CompositionMaturity::Candidate
    );
}
#[test]
fn explicit_assumption_invalidation_is_reported() {
    let (_f, _s, mut c) = setup(&["rotate", "deploy", "rollback"]);
    c.steps[0]
        .expected_outputs
        .push(fact("old_credential_valid", ScopeValue::Boolean(false)));
    c.assumptions.push(OperationalAssumption {
        id: OperationalAssumptionId::new(),
        owner: c.steps[2].component.component.clone(),
        condition: condition("old_credential_valid", json!(true)),
        scope: Default::default(),
        evidence: vec![],
    });
    let r = DefaultCompositionPreflightAnalyzer
        .analyze(&c, &Default::default())
        .unwrap();
    assert_eq!(
        r.status,
        CompositionCompatibilityStatus::CompatibleWithConditions
    );
    assert!(
        r.findings
            .iter()
            .any(|f| f.kind == CompositionCompatibilityFindingKind::InvalidatedAssumption)
    );
}
#[tokio::test]
async fn credential_rollback_control_and_counterfactual() {
    let (f, s, mut c) = setup(&["rotate", "deploy", "rollback"]);
    c.steps[2].input_bindings = vec![
        binding("old_credential_valid", Some(c.steps[0].id.clone())),
        binding("healthy", Some(c.steps[1].id.clone())),
    ];
    s.save_composition(&c).unwrap();
    let engine =
        CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
            .unwrap();
    let mut r = request(&f, &c);
    r.evaluation.checks = vec![condition("service_consistent", json!(true))];
    let control = engine.run(&r, &Default::default()).await.unwrap();
    assert_eq!(control.outcome, CompositionOutcome::Pass);
    r.step_inputs
        .insert(c.steps[1].id.clone(), json!({"fail":true}));
    let fail = engine.run(&r, &Default::default()).await.unwrap();
    assert_eq!(fail.outcome, CompositionOutcome::Fail);
    assert_eq!(fail.step_results[2].outcome, CompositionOutcome::Fail);
    r.step_inputs
        .insert(c.steps[0].id.clone(), json!({"preserve":true}));
    let fixed = engine.run(&r, &Default::default()).await.unwrap();
    assert_eq!(fixed.outcome, CompositionOutcome::Pass);
    assert!(fixed.step_results.iter().all(|r| r.attestation.is_some()));
    assert_eq!(
        s.composition_maturity(&c).unwrap(),
        CompositionMaturity::Degraded
    );
}
#[tokio::test]
async fn cumulative_capacity_fails_despite_successful_steps() {
    let (f, s, mut c) = setup(&["capacity", "safe"]);
    let first = c.steps[0].component.clone();
    c.steps[1].component = first;
    c.steps[1].input_bindings = vec![binding("capacity", Some(c.steps[0].id.clone()))];
    c.contract.sequence_invariants.push(SequenceInvariant {
        id: SequenceInvariantId::new(),
        scope: SequenceScope::EntireComposition,
        condition: BehavioralCondition::StatePredicate {
            path: "capacity".into(),
            operator: PredicateOperator::GreaterThanOrEqual,
            value: json!(3),
        },
        severity: Severity::Critical,
        evidence: vec![],
    });
    s.save_composition(&c).unwrap();
    let mut r = request(&f, &c);
    r.initial_state = vec![fact("capacity", ScopeValue::Integer(4))];
    r.step_inputs
        .insert(c.steps[0].id.clone(), json!({"capacity":4}));
    r.evaluation.checks = vec![condition("capacity", json!(2))];
    let e = CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
        .unwrap()
        .run(&r, &Default::default())
        .await
        .unwrap();
    assert!(
        e.step_results
            .iter()
            .all(|s| s.outcome == CompositionOutcome::Pass)
    );
    assert_eq!(e.outcome, CompositionOutcome::Fail);
}
#[tokio::test]
async fn separate_realities_destroy_ephemeral_state() {
    let (f, s, c) = setup(&["ephemeral-a", "ephemeral-b"]);
    s.save_composition(&c).unwrap();
    let mut r = request(&f, &c);
    r.evaluation.checks = vec![condition("isolated", json!(true))];
    let e = CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
        .unwrap()
        .run(&r, &Default::default())
        .await
        .unwrap();
    assert_eq!(e.outcome, CompositionOutcome::Pass);
    assert!(
        s.micro_sandboxes()
            .unwrap()
            .iter()
            .all(|m| m.destroyed_at.is_some())
    );
}
#[test]
fn secret_network_flow_and_ambient_union_are_rejected() {
    let (_f, _s, mut c) = setup(&["rotate", "deploy"]);
    let source = c.steps[0].id.clone();
    c.steps[1].input_bindings.push(StateBinding {
        resource: CapabilityFlowResource::Secret,
        ..binding("secret", Some(source))
    });
    c.steps[1].capabilities.network.mode = NetworkMode::Unrestricted;
    let r = DefaultCompositionPreflightAnalyzer
        .analyze(&c, &Default::default())
        .unwrap();
    assert_eq!(
        r.capability_plan.findings[0].classification,
        CapabilityFlowClassification::Forbidden
    );
    assert!(
        r.capability_plan
            .persistent_capabilities
            .credentials
            .is_empty()
    );
    c.contract
        .capability_policy
        .persistent_capabilities
        .process
        .allow_exec = true;
    assert_eq!(
        DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap()
            .status,
        CompositionCompatibilityStatus::Conflict
    );
}
#[test]
fn stale_external_handoff_is_unknown() {
    let (_f, _s, c) = setup(&["rotate", "deploy"]);
    let mut fact = fact("resource_version", ScopeValue::Integer(10));
    fact.freshness.external_version = Some(ExternalStateVersion {
        resource: "resource".into(),
        version: "10".into(),
    });
    let h = StateHandoff {
        id: StateHandoffId::new(),
        from: c.steps[0].id.clone(),
        to: c.steps[1].id.clone(),
        facts: vec![fact],
        artifacts: vec![],
        external_state_versions: vec![ExternalStateVersion {
            resource: "resource".into(),
            version: "10".into(),
        }],
        provenance: CompositionEvidenceProvenance {
            evidence: vec![],
            evaluator: "fixture".into(),
            environment: "fixture".into(),
            intervention: None,
        },
    };
    assert!(!handoff_fresh(
        &h,
        &StateFreshnessRequirement::None,
        &c.steps[1].id,
        &BTreeMap::from([("resource".into(), "11".into())]),
        Utc::now()
    ));
}
#[test]
fn no_fake_atomicity_or_self_promoted_composition() {
    let (_f, s, mut c) = setup(&["safe"]);
    c.contract.effect_policy.atomicity = CompositionAtomicity::FullyAtomic;
    assert_eq!(
        DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap()
            .status,
        CompositionCompatibilityStatus::Conflict
    );
    c.maturity = CompositionMaturity::Validated;
    assert!(s.save_composition(&c).is_err());
}

fn runtime_context(c: &Composition, index: usize) -> hardknock::runtime::RuntimeDecisionContext {
    let scenario: hardknock::runtime::RuntimeScenario = serde_json::from_str(include_str!(
        "../fixtures/runtime-scenarios/known-safe.json"
    ))
    .unwrap();
    let mut r = scenario.decision_context().unwrap();
    r.composition = Some(CompositionRuntimeContext {
        composition: c.id.clone(),
        revision: c.revision,
        step: c.steps[index].id.clone(),
        completed_steps: c.steps[..index].iter().map(|s| s.id.clone()).collect(),
        current_handoffs: vec![],
        active_sequence_invariants: vec![],
        commit_state: Default::default(),
        state: vec![],
        external_versions: BTreeMap::new(),
        assessed_at: Utc::now(),
    });
    r
}
#[test]
fn current_step_rechecks_invariant_and_authority() {
    use hardknock::{runtime::*, store::RuntimeStore};
    let (_f, s, mut c) = setup(&["safe"]);
    c.contract.sequence_invariants.push(SequenceInvariant {
        id: SequenceInvariantId::new(),
        scope: SequenceScope::Before {
            step: c.steps[0].id.clone(),
        },
        condition: condition("credential_valid", json!(true)),
        severity: Severity::Critical,
        evidence: vec![],
    });
    s.save_composition(&c).unwrap();
    let mut r = runtime_context(&c, 0);
    r.composition.as_mut().unwrap().state =
        vec![fact("credential_valid", ScopeValue::Boolean(true))];
    let original = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(original.decision.kind(), RuntimeDecisionKind::Act);
    r.composition.as_mut().unwrap().state[0].value = ScopeValue::Boolean(false);
    assert_eq!(
        s.record_runtime_decision(&r, Default::default())
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Replan
    );
    assert_eq!(
        s.runtime_decision(&original.id).unwrap().decision.kind(),
        RuntimeDecisionKind::Act
    );
    r.capability_context.governance.hard_policy_blocked = true;
    assert_eq!(
        s.record_runtime_decision(&r, Default::default())
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Abstain
    );
}
#[test]
fn current_revision_invalidates_but_history_is_immutable() {
    use hardknock::{runtime::*, store::RuntimeStore};
    let (_f, s, mut c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let original = s
        .record_runtime_decision(&runtime_context(&c, 0), Default::default())
        .unwrap();
    let history = s.historical_composition(&c.id, 1).unwrap();
    c.revision = 2;
    c.name = "revised-composition".into();
    s.save_composition(&c).unwrap();
    assert_eq!(history, s.historical_composition(&c.id, 1).unwrap());
    assert_eq!(
        s.runtime_decision(&original.id).unwrap().decision.kind(),
        RuntimeDecisionKind::Act
    );
    assert_eq!(
        s.replay_runtime_decision(&original.id, Default::default())
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Replan
    );
    assert!(s.save_composition(&c).is_err());
}
#[test]
fn local_recovery_does_not_imply_global_recovery() {
    assert_eq!(
        composition_recovery_outcome(true, &[Some(false)], false),
        CompositionRecoveryOutcome::LocallyRecovered
    );
    assert_eq!(
        composition_recovery_outcome(true, &[None], false),
        CompositionRecoveryOutcome::Inconclusive
    );
    assert_eq!(
        composition_recovery_outcome(true, &[Some(true)], true),
        CompositionRecoveryOutcome::Compensated
    );
    assert_eq!(
        composition_recovery_outcome(true, &[Some(true)], false),
        CompositionRecoveryOutcome::FullyRecovered
    );
    assert_eq!(
        composition_recovery_outcome(false, &[Some(true)], false),
        CompositionRecoveryOutcome::Failed
    );
}
#[test]
fn contradictory_recovery_requirements_fail_preflight() {
    let (_f, _s, mut c) = setup(&["safe"]);
    c.recoveries.push(CompositionRecoveryPlan {
        failure_point: c.steps[0].id.clone(),
        recoveries: vec![CompositionRecoveryStep {
            recovery: RecoveryId::new(),
            revision: 1,
            requires: vec![condition("token", json!(true))],
            establishes: vec![condition("token", json!(false))],
        }],
        invariants: vec![],
        effect_reconciliation: vec![],
        status: CompositionRecoveryStatus::Candidate,
    });
    assert!(
        DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap()
            .findings
            .iter()
            .any(|f| f.kind == CompositionCompatibilityFindingKind::RecoveryConflict)
    );
}
#[test]
fn untrusted_conflicting_and_expired_state_stays_unknown() {
    let mut a = fact("valid", ScopeValue::Boolean(true));
    a.source = StateClaimSource::AgentReported;
    assert!(
        trusted_state(&[a.clone()], &BTreeMap::new(), Utc::now())
            .values
            .is_empty()
    );
    a.source = StateClaimSource::RuntimeObservation;
    let mut b = a.clone();
    b.value = ScopeValue::Boolean(false);
    assert!(
        trusted_state(&[a.clone(), b], &BTreeMap::new(), Utc::now())
            .values
            .is_empty()
    );
    a.freshness.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    assert!(
        trusted_state(&[a], &BTreeMap::new(), Utc::now())
            .values
            .is_empty()
    );
}
#[tokio::test]
async fn evaluator_rename_is_correlated_and_proof_tampering_rejected() {
    let (f, s, mut c) = setup(&["safe"]);
    c.contract.postconditions = vec![condition("ok", json!(true))];
    s.save_composition(&c).unwrap();
    let engine =
        CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
            .unwrap();
    let mut r = request(&f, &c);
    let a = engine.run(&r, &Default::default()).await.unwrap();
    r.evaluation.evaluator = "independent-label-only".into();
    let b = engine.run(&r, &Default::default()).await.unwrap();
    assert_eq!(a.provenance.evaluator, b.provenance.evaluator);
    assert_ne!(s.composition_assurance(&c).unwrap()["satisfied"], true);
    assert!(s.promote_composite_skill(&c.id).is_err());
    assert!(a.step_results.iter().all(|s| s.runtime_decision.is_some()));
    r.starting_state.fingerprint = "forged".into();
    assert!(engine.run(&r, &Default::default()).await.is_err());
}
#[tokio::test]
async fn evidence_and_revision_rows_reject_mutation() {
    let (f, s, c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let e = CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
        .unwrap()
        .run(&request(&f, &c), &Default::default())
        .await
        .unwrap();
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute(
            "UPDATE composition_evidence SET data='{}' WHERE id=?1",
            [e.id.to_string()]
        )
        .is_err()
    );
    assert!(
        db.execute(
            "DELETE FROM composition_revisions WHERE id=?1",
            [c.id.to_string()]
        )
        .is_err()
    );
}
#[tokio::test]
async fn ordering_counterfactual_requires_schema_handoff() {
    let (f, s, mut c) = setup(&["migrate", "rollout"]);
    c.steps[1].input_bindings = vec![binding("schema", Some(c.steps[0].id.clone()))];
    s.save_composition(&c).unwrap();
    let mut r = request(&f, &c);
    r.evaluation.checks = vec![condition("rollout_ok", json!(true))];
    let e = CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
        .unwrap()
        .run(&r, &Default::default())
        .await
        .unwrap();
    assert_eq!(e.outcome, CompositionOutcome::Pass);
    c.relations.clear();
    assert!(ordered_steps(&c).is_err());
}
#[test]
fn disjoint_sequence_phases_do_not_conflict() {
    let (_f, _s, mut c) = setup(&["safe"]);
    c.contract.preconditions = vec![condition("version", json!(1))];
    c.contract.sequence_invariants.push(SequenceInvariant {
        id: SequenceInvariantId::new(),
        scope: SequenceScope::After {
            step: c.steps[0].id.clone(),
        },
        condition: condition("version", json!(2)),
        severity: Severity::High,
        evidence: vec![],
    });
    assert!(
        !DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap()
            .findings
            .iter()
            .any(|f| f.kind == CompositionCompatibilityFindingKind::ConstraintConflict)
    );
}
#[test]
fn bounded_interaction_analysis_scales_without_permutations() {
    let (_f, _s, base) = setup(&["safe"]);
    for size in [10, 25, 50] {
        let mut c = base.clone();
        c.steps = (0..size)
            .map(|_| {
                let mut s = base.steps[0].clone();
                s.id = CompositionStepId::new();
                s
            })
            .collect();
        c.relations = c
            .steps
            .windows(2)
            .map(|p| CompositionRelation {
                from: p[0].id.clone(),
                to: p[1].id.clone(),
                kind: CompositionRelationKind::Before,
            })
            .collect();
        let start = std::time::Instant::now();
        let report = DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert!(report.untested_pairs.len() <= size * size);
        eprintln!(
            "composition preflight steps={size} elapsed_us={}",
            start.elapsed().as_micros()
        );
    }
}

#[tokio::test]
async fn cancellation_cleans_sandbox_and_keeps_inconclusive_evidence() {
    let (f, s, c) = setup(&["slow"]);
    s.save_composition(&c).unwrap();
    let mut r = request(&f, &c);
    r.budget.max_duration_ms = Some(100);
    let e = CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
        .unwrap()
        .run(&r, &Default::default())
        .await
        .unwrap();
    assert_eq!(e.outcome, CompositionOutcome::Inconclusive);
    assert!(
        s.micro_sandboxes()
            .unwrap()
            .iter()
            .all(|m| m.destroyed_at.is_some())
    );
}
#[test]
fn component_disable_requires_revalidation_without_rewriting_history() {
    let (_f, s, c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let history = s.historical_composition(&c.id, c.revision).unwrap();
    let ComposableArtifactRef::Tool(id) = &c.steps[0].component.component else {
        unreachable!()
    };
    s.disable_tool_definition(id).unwrap();
    assert_eq!(
        s.composition_dependency_health(&c).unwrap().status,
        CompositionHealthStatus::Broken
    );
    assert_eq!(
        s.composition_maturity(&c).unwrap(),
        CompositionMaturity::Stale
    );
    assert_eq!(
        history,
        s.historical_composition(&c.id, c.revision).unwrap()
    );
}
#[test]
fn partial_effect_commit_keeps_real_receipts_without_rollback_claim() {
    use hardknock::effects::*;
    let (_f, s, mut c) = setup(&["safe"]);
    let manager = EffectManager::new(&s).unwrap();
    let make = |target: &str| EffectRequest {
        session_id: "composition-fixture".into(),
        reality_id: None,
        source_action: ActionRef {
            id: target.into(),
            kind: "fixture".into(),
        },
        kind: EffectKind::HttpApi,
        target: EffectTarget { uri: target.into() },
        operation: EffectOperation::Update,
        payload: json!({"value":1}),
        adapter: None,
        evidence: vec![],
        fault: EffectFault::None,
    };
    let (first, _) = manager
        .propose_and_prepare(make("mock://composition/a"), &EffectManager::user_context())
        .unwrap();
    let (second, _) = manager
        .propose_and_prepare(make("mock://composition/b"), &EffectManager::user_context())
        .unwrap();
    let authority = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&first.id))
        .unwrap();
    manager
        .commit(&first.id, Some(&authority), &EffectManager::user_context())
        .unwrap();
    c.contract.effect_policy.effects = vec![
        CompositionEffect {
            step: c.steps[0].id.clone(),
            effect: first.id,
            adapter: "mock-http".into(),
        },
        CompositionEffect {
            step: c.steps[0].id.clone(),
            effect: second.id,
            adapter: "mock-http".into(),
        },
    ];
    let state = s.composition_effect_state(&c).unwrap();
    assert_eq!(state["partial_commit"], true);
    assert_eq!(state["committed_receipts"].as_array().unwrap().len(), 1);
    assert_eq!(state["rollback_inferred"], false);
    assert_eq!(state["compensation_is_rollback"], false);
}

#[tokio::test]
#[ignore = "explicit local comparative benchmark; no models or network"]
async fn composition_comparative_benchmark() {
    let mut rows = vec![];
    for case in ["credential", "capacity", "capability"] {
        let modes: &[&str] = match case {
            "credential" => &["rotate", "deploy", "rollback"],
            "capacity" => &["capacity", "safe"],
            _ => &["rotate", "deploy"],
        };
        let (f, s, mut c) = setup(modes);
        let mut r = request(&f, &c);
        match case {
            "credential" => {
                c.steps[2].input_bindings = vec![
                    binding("old_credential_valid", Some(c.steps[0].id.clone())),
                    binding("healthy", Some(c.steps[1].id.clone())),
                ];
                r.step_inputs
                    .insert(c.steps[1].id.clone(), json!({"fail":true}));
                r.evaluation.checks = vec![condition("service_consistent", json!(true))];
            }
            "capacity" => {
                c.steps[1].component = c.steps[0].component.clone();
                c.steps[1].input_bindings = vec![binding("capacity", Some(c.steps[0].id.clone()))];
                r.initial_state = vec![fact("capacity", ScopeValue::Integer(4))];
                r.step_inputs
                    .insert(c.steps[0].id.clone(), json!({"capacity":4}));
                r.evaluation.checks = vec![condition("capacity", json!(2))];
                c.contract.sequence_invariants.push(SequenceInvariant {
                    id: SequenceInvariantId::new(),
                    scope: SequenceScope::EntireComposition,
                    condition: BehavioralCondition::StatePredicate {
                        path: "capacity".into(),
                        operator: PredicateOperator::GreaterThanOrEqual,
                        value: json!(3),
                    },
                    severity: Severity::High,
                    evidence: vec![],
                });
            }
            _ => {
                c.steps[1].input_bindings = vec![StateBinding {
                    resource: CapabilityFlowResource::Secret,
                    ..binding("secret", Some(c.steps[0].id.clone()))
                }];
                c.steps[1].capabilities.network.mode = NetworkMode::Unrestricted;
            }
        }
        r.starting_state = composition_starting_proof(&c, &r.starting_state.state_ref).unwrap();
        s.save_composition(&c).unwrap();
        let static_start = std::time::Instant::now();
        let report = DefaultCompositionPreflightAnalyzer
            .analyze(&c, &Default::default())
            .unwrap();
        let static_us = static_start.elapsed().as_micros();
        let run_start = std::time::Instant::now();
        let result =
            CompositionExperimentEngine::new(&s, HostMicroSandboxProvider::trusted_development())
                .unwrap()
                .run(&r, &Default::default())
                .await;
        let run_us = run_start.elapsed().as_micros();
        let full_detected = match &result {
            Ok(e) => e.outcome == CompositionOutcome::Fail,
            Err(_) => report.status == CompositionCompatibilityStatus::Conflict,
        };
        assert!(full_detected);
        // The component-only rule sees only component registration/pinning; it has
        // no cross-step state or flow relation input. It is deliberately not called empirical validation.
        let component_detected = c.steps.iter().any(|step| {
            s.pin_composition_component(&step.component.component)
                .is_err()
        });
        rows.push(json!({"case":case,"component_only_detected":component_detected,"static_detected":report.status==CompositionCompatibilityStatus::Conflict,"full_detected":full_detected,"static_us":static_us,"full_us":run_us,"actual_step_runs":result.as_ref().map(|e|e.step_results.len()).unwrap_or(0),"production_commits":0}));
    }
    let report = json!({"classification":"deterministic engineering fixtures; not a statistical study","component_baseline":"registered current components without composition analysis","rows":rows});
    if let Ok(path) = std::env::var("HARDKNOCK_COMPOSITION_BENCH_OUTPUT") {
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
}

#[test]
fn economics_prioritizes_frequent_high_risk_composition() {
    let (_f, s, c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let high = s
        .composition_opportunity(&c, 100, Severity::Critical, &Default::default())
        .unwrap();
    let low = s
        .composition_opportunity(&c, 1, Severity::Low, &Default::default())
        .unwrap();
    assert!(high.value.risk_reduction > low.value.risk_reduction);
    assert!(high.value.decision_relevance > low.value.decision_relevance);
}
#[test]
fn predictive_sequence_uses_positive_and_negative_controls() {
    use hardknock::{predictive::*, runtime::FailureSignatureRef};
    let (_f, s, c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let failure = FailureSignatureRef {
        signature: "credential-rollback-unavailable".into(),
    };
    let mut positives = vec![];
    let mut negatives = vec![];
    for invalid in [true, true, false, false] {
        let t = s
            .start_composition_trajectory(&c, HardknockSessionId::new())
            .unwrap();
        s.observe_composition_step(
            &t.id,
            &c.steps[0].id,
            &[fact("old_credential_valid", ScopeValue::Boolean(!invalid))],
        )
        .unwrap();
        s.finish_trajectory(
            &t.id,
            if invalid {
                TrajectoryOutcome::Failure(failure.clone())
            } else {
                TrajectoryOutcome::Success
            },
        )
        .unwrap();
        if invalid {
            positives.push(t.id)
        } else {
            negatives.push(t.id)
        };
    }
    let sig = s
        .register_warning_signature(EarlyWarningSignature {
            id: EarlyWarningSignatureId::new(),
            failure,
            ordered_conditions: vec![TrajectoryCondition::FeatureCondition {
                predicate: FeaturePredicate {
                    feature: "old_credential_valid".into(),
                    operator: ComparisonOperator::Equals,
                    value: TrajectoryValue::Boolean(false),
                },
            }],
            failure_trajectory: None,
            horizon: ForecastHorizon::Actions(2),
            scope: hardknock::lesson::ContextSelector {
                repository: None,
                required_markers: vec![],
                tags: vec![],
                os: None,
                arch: None,
            },
            evidence: vec![],
            status: RiskIndicatorStatus::Candidate,
            revision: 1,
            origin: PredictiveOrigin::Local,
            causal_basis: vec![],
            required_runtime_version: None,
            precision: None,
            recall: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .unwrap();
    s.validate_warning_signature(&sig.id, &positives, &negatives)
        .unwrap();
    let healthy = s
        .start_composition_trajectory(&c, HardknockSessionId::new())
        .unwrap();
    assert!(
        s.observe_composition_step(
            &healthy.id,
            &c.steps[0].id,
            &[fact("old_credential_valid", ScopeValue::Boolean(true))]
        )
        .unwrap()
        .is_empty()
    );
    let risky = s
        .start_composition_trajectory(&c, HardknockSessionId::new())
        .unwrap();
    assert!(
        !s.observe_composition_step(
            &risky.id,
            &c.steps[0].id,
            &[fact("old_credential_valid", ScopeValue::Boolean(false))]
        )
        .unwrap()
        .is_empty()
    );
}
#[test]
fn hierarchy_resolution_is_repeated_in_composition_context() {
    use hardknock::{knowledge_runtime::*, runtime::*, store::RuntimeStore};
    let (_f, s, c) = setup(&["safe"]);
    s.save_composition(&c).unwrap();
    let mut h: KnowledgeHierarchy = serde_json::from_str(include_str!(
        "../fixtures/hierarchy/idempotency/hierarchy.json"
    ))
    .unwrap();
    s.save_knowledge_hierarchy(&h).unwrap();
    let mut r = runtime_context(&c, 0);
    r.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::Network {
        method: "POST".into(),
        target: "https://provider.invalid/mutation".into(),
    });
    for (key, value) in [
        ("provider", ScopeValue::String("provider-x".into())),
        ("api_version", ScopeValue::String("2".into())),
        ("idempotency", ScopeValue::String("exact".into())),
        ("token_valid", ScopeValue::Boolean(true)),
    ] {
        r.context_observations.insert(
            key.into(),
            vec![ContextValue {
                value,
                source: ContextValueSource::EffectAdapterObserved,
            }],
        );
    }
    let before = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(before.decision.kind(), RuntimeDecisionKind::Act);
    assert!(before.context.operational_knowledge.is_some());
    for n in h.nodes.values_mut() {
        if n.artifact.id == "provider-x-exact-replay" {
            n.activation = KnowledgeActivationState::Disabled;
        }
    }
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    let after = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(after.decision.kind(), RuntimeDecisionKind::Replan);
    assert_eq!(
        s.runtime_decision(&before.id).unwrap().decision.kind(),
        RuntimeDecisionKind::Act
    );
}
