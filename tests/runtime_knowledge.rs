// SPDX-License-Identifier: Apache-2.0
mod support;
use chrono::Utc;
use hardknock::{
    bridge::protocol::NormalizedAction,
    core::*,
    hierarchy::*,
    knowledge_runtime::*,
    runtime::*,
    store::{RuntimeStore, Store},
};
use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};

fn fixture(name: &str) -> KnowledgeHierarchy {
    serde_json::from_slice(&fs::read(path(name)).unwrap()).unwrap()
}
fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/hierarchy/idempotency")
        .join(name)
}
fn node(n: usize) -> KnowledgeNodeId {
    format!("knowledge-node-00000000-0000-4000-8000-{n:012}")
        .parse()
        .unwrap()
}
fn setup(name: &str) -> (tempfile::TempDir, Store, KnowledgeHierarchy) {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(&t.path().join("store")).unwrap();
    let h = fixture(name);
    s.save_knowledge_hierarchy(&h).unwrap();
    (t, s, h)
}
fn runtime(token: Option<bool>) -> RuntimeDecisionContext {
    let scenario: RuntimeScenario = serde_json::from_slice(
        &fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/runtime-scenarios/known-safe.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut r = scenario.decision_context().unwrap();
    r.proposed_action = Some(NormalizedAction::Network {
        method: "POST".into(),
        target: "https://provider.invalid/mutation".into(),
    });
    r.knowledge_action_id = Some("retry-1".into());
    for (key, value) in [
        ("provider", ScopeValue::String("provider-x".into())),
        ("api_version", ScopeValue::String("2".into())),
        ("idempotency", ScopeValue::String("exact".into())),
    ] {
        r.context_observations.insert(
            key.into(),
            vec![ContextValue {
                value,
                source: ContextValueSource::EffectAdapterObserved,
            }],
        );
    }
    if let Some(v) = token {
        r.context_observations.insert(
            "token_valid".into(),
            vec![ContextValue {
                value: ScopeValue::Boolean(v),
                source: ContextValueSource::EffectAdapterObserved,
            }],
        );
    }
    r
}
fn resolve(s: &Store, r: &RuntimeDecisionContext) -> RuntimeKnowledgeResolution {
    DefaultRuntimeKnowledgeResolver {
        store: s,
        policy: Default::default(),
        budget: Default::default(),
        persist: true,
    }
    .resolve_for_runtime(r)
    .unwrap()
}
#[test]
fn runtime_idempotency_and_external_authority() {
    let (_t, s, _) = setup("hierarchy.json");
    for (token, expected) in [
        (Some(true), RuntimeDecisionKind::Act),
        (Some(false), RuntimeDecisionKind::Replan),
        (None, RuntimeDecisionKind::Replan),
    ] {
        let record = s
            .record_runtime_decision(&runtime(token), Default::default())
            .unwrap();
        assert_eq!(record.decision.kind(), expected);
        let k = record.context.operational_knowledge.unwrap();
        assert!(!k.snapshot.fingerprint.is_empty());
        assert!(!k.provenance.applied_artifacts.is_empty());
    }
    let mut r = runtime(Some(true));
    r.capability_context.governance.approval_required = true;
    let record = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(record.decision.kind(), RuntimeDecisionKind::RequireApproval);
    assert_eq!(
        record.evaluation.governance,
        GovernanceDisposition::ApprovalOverride
    );
    assert!(
        !record
            .context
            .operational_knowledge
            .unwrap()
            .bundle
            .exceptions
            .is_empty()
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
fn untrusted_values_never_activate_exception_and_conflicts_are_recorded() {
    let (_t, s, _) = setup("hierarchy.json");
    let mut r = runtime(None);
    r.context_observations.insert(
        "token_valid".into(),
        vec![ContextValue {
            value: ScopeValue::Boolean(true),
            source: ContextValueSource::AgentReported,
        }],
    );
    let k = resolve(&s, &r);
    assert!(k.bundle.exceptions.is_empty());
    assert!(!k.effective.unknown.is_empty());
    r.context_observations
        .get_mut("token_valid")
        .unwrap()
        .push(ContextValue {
            value: ScopeValue::Boolean(false),
            source: ContextValueSource::EffectAdapterObserved,
        });
    let k = resolve(&s, &r);
    assert_eq!(k.context.values["token_valid"], ScopeValue::Boolean(false));
    assert_eq!(k.context_conflicts.len(), 1);
    r.context_observations
        .get_mut("token_valid")
        .unwrap()
        .push(ContextValue {
            value: ScopeValue::Boolean(true),
            source: ContextValueSource::EffectAdapterObserved,
        });
    assert!(!resolve(&s, &r).context.values.contains_key("token_valid"));
}
#[test]
fn stale_and_contradicted_exceptions_cannot_relax_runtime() {
    for bad in [FreshnessStatus::Stale, FreshnessStatus::Contradicted] {
        let (_t, s, mut h) = setup("hierarchy.json");
        h.nodes.get_mut(&node(4)).unwrap().freshness = bad;
        h.revision += 1;
        s.save_knowledge_hierarchy(&h).unwrap();
        let d = s
            .record_runtime_decision(&runtime(Some(true)), Default::default())
            .unwrap();
        assert_eq!(d.decision.kind(), RuntimeDecisionKind::Replan);
        assert!(
            d.context
                .operational_knowledge
                .unwrap()
                .bundle
                .exceptions
                .is_empty()
        );
    }
}
#[test]
fn snapshots_replay_old_revisions_without_hindsight_or_writes() {
    let (_t, s, mut h) = setup("hierarchy.json");
    let full = h.clone();
    h.nodes.remove(&node(4));
    h.edges.retain(|e| e.child != node(4));
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    let original = s
        .record_runtime_decision(&runtime(Some(true)), Default::default())
        .unwrap();
    assert_eq!(original.decision.kind(), RuntimeDecisionKind::Replan);
    let k = original.context.operational_knowledge.as_ref().unwrap();
    let snapshot = s.knowledge_snapshot(&k.snapshot.id).unwrap();
    let historical = DefaultHistoricalKnowledgeResolver { store: &s }
        .resolve_snapshot(&snapshot, &k.context)
        .unwrap();
    let mut now = full;
    now.revision = 3;
    s.save_knowledge_hierarchy(&now).unwrap();
    assert_eq!(
        serde_json::to_value(&historical).unwrap(),
        serde_json::to_value(
            DefaultHistoricalKnowledgeResolver { store: &s }
                .resolve_snapshot(&snapshot, &k.context)
                .unwrap()
        )
        .unwrap()
    );
    let count = s.runtime_decisions().unwrap().len();
    let replay = s
        .replay_knowledge_decision(&original.id, Default::default())
        .unwrap();
    assert_eq!(replay["original"]["decision"]["decision"], "replan");
    assert_eq!(
        replay["current_hypothetical_decision"]["decision"]["decision"],
        "act"
    );
    assert_eq!(s.runtime_decisions().unwrap().len(), count);
    assert_eq!(
        serde_json::to_value(s.runtime_decision(&original.id).unwrap()).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    let db = rusqlite::Connection::open(s.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute("UPDATE knowledge_snapshots SET data='{}'", [])
            .is_err()
    );
    assert!(
        db.execute("DELETE FROM knowledge_hierarchy_revisions", [])
            .is_err()
    );
}
#[test]
fn snapshot_dedup_and_exact_artifact_revision_integrity() {
    let (_t, s, h) = setup("hierarchy.json");
    let a = resolve(&s, &runtime(Some(true)));
    let b = resolve(&s, &runtime(None));
    assert_eq!(a.snapshot, b.snapshot);
    let snapshot = s.knowledge_snapshot(&a.snapshot.id).unwrap();
    assert_eq!(snapshot.hierarchies[0].revision, h.revision);
    assert_eq!(snapshot.artifact_revisions.len(), h.nodes.len());
    let mut body = s
        .operational_revision(&snapshot.artifact_revisions[0])
        .unwrap();
    body.statement = "tampered".into();
    assert!(s.register_operational_revision(&body).is_err());
}
#[test]
fn hierarchy_and_context_toctou_and_exact_action_binding() {
    let (_t, s, mut h) = setup("hierarchy.json");
    let r = runtime(Some(true));
    let d = s.record_runtime_decision(&r, Default::default()).unwrap();
    let k = d.context.operational_knowledge.unwrap();
    assert!(s.guidance_is_current(&k.validity, &k.context).unwrap());
    assert!(
        s.check_knowledge_before_commit(&r.session_id.to_string(), "retry-1")
            .is_ok()
    );
    assert!(
        s.check_knowledge_before_commit(&r.session_id.to_string(), "different-action")
            .is_err()
    );
    h.nodes.get_mut(&node(4)).unwrap().freshness = FreshnessStatus::Contradicted;
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    assert!(!s.guidance_is_current(&k.validity, &k.context).unwrap());
    assert!(
        s.check_knowledge_before_commit(&r.session_id.to_string(), "retry-1")
            .is_err()
    );
}
#[test]
fn parent_invalidation_preserves_only_independent_support() {
    for independent in [false, true] {
        let mut h = fixture("hierarchy.json");
        h.nodes.retain(|id, _| [node(1), node(2)].contains(id));
        h.edges.truncate(1);
        h.nodes.get_mut(&node(1)).unwrap().freshness = FreshnessStatus::Contradicted;
        if independent {
            h.nodes.get_mut(&node(2)).unwrap().provenance.evidence.push(
                hardknock::epistemic::EvidenceRef {
                    kind: "controlled".into(),
                    id: "independent-trials".into(),
                },
            );
        }
        let (projected, changes) = health_projection(&h);
        assert_eq!(changes.is_empty(), independent);
        let effective = DeterministicKnowledgeResolver
            .resolve(
                &projected,
                &DefaultKnowledgeContextBuilder
                    .build(&runtime(Some(true)))
                    .unwrap(),
                &Default::default(),
            )
            .unwrap();
        assert_eq!(
            effective.applied.iter().any(|a| a.node == node(2)),
            independent
        );
    }
}
#[test]
fn recovery_fallback_is_executable_only_with_a_pinned_procedure() {
    let t = tempfile::tempdir().unwrap();
    let s = Store::open(&t.path().join("store")).unwrap();
    let mut h = fixture("hierarchy.json");
    h.nodes.retain(|id, _| [node(1), node(2)].contains(id));
    h.edges.truncate(1);
    for n in h.nodes.values_mut() {
        n.artifact.kind = KnowledgeArtifactKind::Recovery;
    }
    h.nodes.get_mut(&node(2)).unwrap().freshness = FreshnessStatus::Stale;
    s.save_knowledge_hierarchy(&h).unwrap();
    let recovery = RecoveryRef {
        id: RecoveryId::new(),
        version: 1,
        failure_signature: "ambiguous".into(),
        confidence: 0.9.try_into().unwrap(),
        fresh: true,
        scope_matches: true,
    };
    s.register_operational_revision(&OperationalKnowledgeRevision {
        knowledge: KnowledgeRevisionRef::from(&h.nodes[&node(1)].artifact),
        statement: "Refetch authoritative state".into(),
        recovery: Some(recovery),
    })
    .unwrap();
    let mut r = runtime(None);
    r.failure_signature = Some(FailureSignatureRef {
        signature: "ambiguous".into(),
    });
    assert_eq!(
        s.record_runtime_decision(&r, Default::default())
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Recover
    );
}
#[test]
fn antipattern_bundle_deduplicates_ancestors() {
    let (_t, s, mut h) = setup("hierarchy.json");
    h.nodes
        .retain(|id, _| [node(1), node(2), node(3)].contains(id));
    h.edges.truncate(2);
    for n in h.nodes.values_mut() {
        n.artifact.kind = KnowledgeArtifactKind::AntiPattern;
    }
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    let k = resolve(&s, &runtime(None));
    assert_eq!(k.bundle.antipatterns.len(), 1);
    assert_eq!(k.bundle.antipatterns[0].lineage.len(), 2);
}
#[test]
fn guard_candidates_export_integrity_and_never_change_authority() {
    let (_t, s, h) = setup("hierarchy.json");
    for n in [1, 4] {
        let c = guard_revision_candidate(
            &h,
            &node(n),
            Some(GuardRef {
                id: "external-reconciliation".into(),
                revision: "3".into(),
            }),
        )
        .unwrap();
        assert!(matches!(
            (&c.proposed_change, n),
            (GuardRevisionChange::AddGuardCandidate { .. }, 1)
                | (GuardRevisionChange::AddException { .. }, 4)
        ));
        s.save_guard_candidate(&c).unwrap();
        let a = GuardRevisionArtifact::export(c).unwrap();
        a.verify().unwrap();
        assert!(!a.enforcement_changed);
        let mut corrupted = a.clone();
        corrupted.candidate.evidence.hierarchy.revision += 1;
        assert!(corrupted.verify().is_err());
        let mut corrupted = a;
        corrupted.candidate.source_knowledge[0].revision += 1;
        assert!(corrupted.verify().is_err());
    }
    let mut h = h;
    h.nodes.get_mut(&node(1)).unwrap().freshness = FreshnessStatus::Contradicted;
    assert!(matches!(
        guard_revision_candidate(&h, &node(1), None)
            .unwrap()
            .proposed_change,
        GuardRevisionChange::ReviewRequired
    ));
    let mut c = guard_revision_candidate(&h, &node(1), None).unwrap();
    c.status = GuardRevisionCandidateStatus::AcceptedExternally;
    assert!(s.save_guard_candidate(&c).is_err());
}
#[test]
fn conflicts_choose_experiment_or_review_and_rank_constraint_exposure() {
    let (_t, s, _) = setup("conflicting-siblings.json");
    let mut r = runtime(Some(true));
    r.available_experiments.safe_reality_available = true;
    r.available_experiments.effect_safe = true;
    let d = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(d.decision.kind(), RuntimeDecisionKind::Experiment);
    r.risk.severity = hardknock::curriculum::Severity::High;
    assert_eq!(
        s.record_runtime_decision(&r, Default::default())
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::RequireApproval
    );
    let c = s.knowledge_conflicts().unwrap().remove(0);
    let b = conflict_opportunity(&c, 100, &Default::default()).unwrap();
    let mut low = c;
    for a in &mut low.conflict.artifacts {
        a.kind = KnowledgeArtifactKind::Lesson;
    }
    let a = conflict_opportunity(&low, 1, &Default::default()).unwrap();
    assert!(b.value.risk_reduction > a.value.risk_reduction);
    assert!(b.value.decision_relevance >= a.value.decision_relevance);
}
#[test]
fn application_outcomes_require_attribution_evidence() {
    let (_t, s, _) = setup("hierarchy.json");
    s.record_runtime_decision(&runtime(None), Default::default())
        .unwrap();
    let a = s.knowledge_applications().unwrap().remove(0);
    assert!(a.outcome.is_none());
    assert!(
        s.classify_knowledge_application(&a.id, KnowledgeApplicationOutcome::FalseConstraint, &[])
            .is_err()
    );
    assert!(
        s.classify_knowledge_application(
            &a.id,
            KnowledgeApplicationOutcome::FalseConstraint,
            &[hardknock::epistemic::EvidenceRef {
                kind: "counterfactual".into(),
                id: "safe-original-action".into()
            }]
        )
        .is_err()
    );
    assert_eq!(
        s.knowledge_metrics().unwrap().false_constraint_applications,
        0
    );
}

fn experiment_template(f: &support::Fixture) -> hardknock::experimentation::ExperimentRequest {
    use hardknock::experimentation::*;
    ExperimentRequest {
        id: ExperimentRequestId::new(),
        session_id: "conflict-fixture".into(),
        question: "At seven minutes, does token replay preserve exactly-once mutation?".into(),
        hypothesis: None,
        candidates: [
            ("retry", "printf unsafe > result"),
            ("reconcile", "printf safe > result"),
        ]
        .into_iter()
        .map(|(name, command)| ExperimentCandidate {
            id: CandidateId::new(),
            name: name.into(),
            description: "Controlled fixture model at token age seven minutes".into(),
            execution: CandidateExecution::Shell {
                commands: vec![command.into()],
            },
            expected_outcome: None,
        })
        .collect(),
        starting_state: ExperimentStartingState {
            state_ref: hardknock::dojo::capture_state(&f.repo).unwrap(),
            expected_fingerprint: None,
            parent_reality: None,
            source: SnapshotSource::RepositoryCommit,
        },
        evaluator: hardknock::evaluation::EvaluationSpec {
            checks: vec!["test \"$(cat result)\" = safe".into()],
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
        intent: ExperimentIntent::ValidateTransfer,
        capabilities: Default::default(),
    }
}
#[tokio::test]
async fn conflict_compiles_existing_experiment_and_controlled_scope_revision() {
    use hardknock::experimentation::*;
    let f = support::Fixture::new();
    let s = Store::open(&f.home).unwrap();
    let h = fixture("conflicting-siblings.json");
    s.save_knowledge_hierarchy(&h).unwrap();
    let mut r = runtime(Some(true));
    r.context_observations.insert(
        "token_age_minutes".into(),
        vec![ContextValue {
            value: ScopeValue::Integer(3),
            source: ContextValueSource::EffectAdapterObserved,
        }],
    );
    resolve(&s, &r);
    let c = s
        .knowledge_conflicts()
        .unwrap()
        .into_iter()
        .find(|c| c.conflict.nodes.contains(&node(6)))
        .unwrap();
    let plan = s
        .plan_knowledge_conflict(&c.id, Some(experiment_template(&f)), &Default::default())
        .unwrap();
    assert_eq!(plan.experiments.len(), 1);
    assert_eq!(
        plan.goal.kind,
        hardknock::curriculum::CurriculumGoalKind::ValidateException
    );
    assert!(!s.experience_opportunities().unwrap().is_empty());
    let config = hardknock::bridge::config::Config::default();
    let result = ExperimentOrchestrator {
        store: &s,
        config: &config,
    }
    .run(
        plan.experiments[0].clone(),
        &hardknock::cancellation::Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .result
            .as_ref()
            .unwrap_or_else(|| panic!("experiment failed: {result:?}"))
            .quality,
        ExperimentQuality::Controlled
    );
    let success = result
        .result
        .as_ref()
        .unwrap()
        .created_experience
        .iter()
        .find(|id| s.experience(id).unwrap().outcome == hardknock::experience::Outcome::Success)
        .unwrap();
    let skill = s.register_skill("conflict-comparison", success).unwrap();
    let curriculum = s
        .compile_knowledge_curriculum(&plan, &skill.id.to_string(), &Default::default())
        .unwrap();
    assert_eq!(curriculum.trials.len(), 1);
    assert!(
        hardknock::store::CurriculumStore::get(&s, &curriculum.id)
            .unwrap()
            .is_some()
    );
    let decision = s
        .record_runtime_decision(&runtime(None), Default::default())
        .unwrap();
    let application = s
        .knowledge_applications()
        .unwrap()
        .into_iter()
        .find(|a| a.runtime_decision == decision.id)
        .unwrap();
    s.classify_knowledge_application(
        &application.id,
        KnowledgeApplicationOutcome::FalseConstraint,
        &[hardknock::epistemic::EvidenceRef {
            kind: "controlled_experiment".into(),
            id: result.id.to_string(),
        }],
    )
    .unwrap();
    assert_eq!(
        s.knowledge_metrics().unwrap().false_constraint_applications,
        1
    );
    let mut narrower = h.nodes[&node(6)].scope.clone();
    narrower.predicates.push(ScopePredicate::IntegerRange {
        key: "token_age_minutes".into(),
        min: Some(6),
        max: Some(10),
    });
    let updated = s
        .narrow_knowledge_conflict(&c.id, &h.id, &node(6), narrower, &[result.id])
        .unwrap();
    assert_eq!(updated.revision, h.revision + 1);
    assert!(resolve(&s, &r).unresolved_conflicts.is_empty());
    assert!(s.knowledge_conflict(&c.id).unwrap().resolved);
}
#[test]
fn bounded_bundle_and_cli_snapshots_audit_guard_roundtrip() {
    use std::process::Command;
    let (t, s, h) = setup("hierarchy.json");
    let d = s
        .record_runtime_decision(&runtime(Some(true)), Default::default())
        .unwrap();
    let k = d.context.operational_knowledge.unwrap();
    assert!(k.bundle.primary_knowledge.len() <= 3);
    assert!(k.bundle.constraints.len() <= 3);
    let candidate = guard_revision_candidate(&h, &node(4), None).unwrap();
    s.save_guard_candidate(&candidate).unwrap();
    let export = t.path().join("guard.json");
    let commands = vec![
        vec!["knowledge".into(), "audit".into()],
        vec![
            "knowledge".into(),
            "snapshot".into(),
            "show".into(),
            k.snapshot.id.to_string(),
        ],
        vec!["knowledge".into(), "conflicts".into()],
        vec![
            "guard-candidate".into(),
            "export".into(),
            candidate.id.to_string(),
            "--output".into(),
            export.display().to_string(),
        ],
        vec![
            "guard-candidate".into(),
            "verify".into(),
            export.display().to_string(),
        ],
        vec!["decision".into(), "replay".into(), d.id.to_string()],
    ];
    for args in commands {
        let result = Command::new(env!("CARGO_BIN_EXE_hardknock"))
            .arg("--home")
            .arg(&s.home)
            .arg("--json")
            .args(&args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap();
    }
}
#[test]
#[ignore = "manual fixture-local runtime and snapshot benchmark"]
fn runtime_knowledge_benchmark() {
    let mut arms = BTreeMap::<&str, (usize, usize, usize, usize)>::new();
    let mut latencies = vec![];
    for (name, file, token, expected) in [
        (
            "valid",
            "hierarchy.json",
            Some(true),
            RuntimeDecisionKind::Act,
        ),
        (
            "expired",
            "hierarchy.json",
            Some(false),
            RuntimeDecisionKind::Replan,
        ),
        (
            "unknown",
            "hierarchy.json",
            None,
            RuntimeDecisionKind::Replan,
        ),
        (
            "stale",
            "stale-exception.json",
            Some(true),
            RuntimeDecisionKind::Replan,
        ),
        (
            "conflict",
            "conflicting-siblings.json",
            Some(true),
            RuntimeDecisionKind::RequireApproval,
        ),
        (
            "version",
            "supersession.json",
            Some(true),
            RuntimeDecisionKind::Replan,
        ),
        (
            "parent",
            "hierarchy.json",
            Some(true),
            RuntimeDecisionKind::Replan,
        ),
        (
            "recovery",
            "hierarchy.json",
            None,
            RuntimeDecisionKind::Replan,
        ),
        (
            "antipattern",
            "hierarchy.json",
            None,
            RuntimeDecisionKind::Replan,
        ),
    ] {
        let (_t, s, mut h) = setup(file);
        let mut r = runtime(token);
        if ["parent", "recovery", "antipattern"].contains(&name) {
            h.nodes.retain(|id, _| [node(1), node(2)].contains(id));
            h.edges.truncate(1);
            if name == "parent" {
                h.nodes.get_mut(&node(1)).unwrap().freshness = FreshnessStatus::Contradicted;
                h.nodes.get_mut(&node(2)).unwrap().provenance.evidence.push(
                    hardknock::epistemic::EvidenceRef {
                        kind: "controlled".into(),
                        id: "independent".into(),
                    },
                );
            }
            if name == "recovery" {
                for n in h.nodes.values_mut() {
                    n.artifact.kind = KnowledgeArtifactKind::Recovery;
                }
                h.nodes.get_mut(&node(2)).unwrap().freshness = FreshnessStatus::Stale;
                r.failure_signature = Some(FailureSignatureRef {
                    signature: "ambiguous".into(),
                });
            }
            if name == "antipattern" {
                for n in h.nodes.values_mut() {
                    n.artifact.kind = KnowledgeArtifactKind::AntiPattern;
                }
            }
            h.revision += 1;
            s.save_knowledge_hierarchy(&h).unwrap();
        }
        if name == "version" {
            r.context_observations.get_mut("api_version").unwrap()[0].value =
                ScopeValue::String("3".into());
        }
        for arm in ["flat", "naive", "hierarchy"] {
            let mut c = r.clone();
            let start = Instant::now();
            let decision = if arm == "hierarchy" {
                s.record_runtime_decision(&c, Default::default())
                    .unwrap()
                    .decision
                    .kind()
            } else {
                // Fixture ablations share typed applicability. Flat retains top-k
                // warnings; naive selects one most-specific match without freshness.
                let context = DefaultKnowledgeContextBuilder.build(&c).unwrap();
                let mut candidates = h
                    .nodes
                    .values()
                    .filter(|n| {
                        DeterministicApplicabilityEvaluator
                            .evaluate(&n.scope, &context)
                            .status
                            == ApplicabilityStatus::Applicable
                    })
                    .collect::<Vec<_>>();
                candidates
                    .sort_by_key(|n| (std::cmp::Reverse(n.scope.predicates.len()), n.id.clone()));
                if arm == "flat" {
                    candidates.truncate(3);
                } else {
                    candidates.truncate(1);
                }
                let blocking = candidates.iter().any(|n| {
                    !(arm == "naive"
                        && h.edges.iter().any(|e| {
                            e.child == n.id && e.relation == KnowledgeHierarchyRelation::Excepts
                        }))
                });
                c.knowledge_signals.known_failure_precursor = blocking;
                DeterministicRuntimeController::default()
                    .evaluate(&c)
                    .unwrap()
                    .decision
                    .kind()
            };
            if arm == "hierarchy" {
                latencies.push(start.elapsed().as_micros());
            }
            let row = arms.entry(arm).or_default();
            row.0 += 1;
            row.1 += usize::from(decision == expected);
            row.2 += usize::from(
                decision == RuntimeDecisionKind::Act && expected != RuntimeDecisionKind::Act,
            );
            row.3 += usize::from(
                decision != RuntimeDecisionKind::Act && expected == RuntimeDecisionKind::Act,
            );
            println!("scenario={name} arm={arm} expected={expected:?} decision={decision:?}");
        }
    }
    println!("arms evaluated/correct/incorrect_allows/unnecessary_interventions={arms:?}");
    assert_eq!(arms["hierarchy"].1, arms["hierarchy"].0);
    assert_eq!(arms["hierarchy"].2, 0);
    let (_t, s, _) = setup("hierarchy.json");
    let mut stage = vec![];
    for _ in 0..50 {
        let start = Instant::now();
        let mut r = runtime(Some(true));
        let context_start = Instant::now();
        DefaultKnowledgeContextBuilder.build(&r).unwrap();
        let build = context_start.elapsed().as_micros();
        let resolution_start = Instant::now();
        s.attach_runtime_knowledge(&mut r).unwrap();
        let resolution = resolution_start.elapsed().as_micros();
        let controller_start = Instant::now();
        DeterministicRuntimeController::default()
            .evaluate(&r)
            .unwrap();
        let controller = controller_start.elapsed().as_micros();
        stage.push((start.elapsed().as_micros(), build, resolution, controller));
    }
    stage.sort();
    let db = rusqlite::Connection::open(s.home.join("hardknock.db")).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM knowledge_snapshots", [], |r| r.get(0))
        .unwrap();
    let bytes: i64 = db
        .query_row(
            "SELECT sum(length(data)) FROM knowledge_snapshots",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!(
        "cached_p95_us total/build/resolution_including_retrieval_bundle/controller={:?}; snapshots={count}; snapshot_json_bytes={bytes}; refs_per_snapshot=5",
        stage[47]
    );
    assert_eq!(count, 1);
}

#[test]
fn guard_dependency_contradiction_creates_review_without_authority_change() {
    let (_t, s, mut h) = setup("hierarchy.json");
    let guard = GuardRef {
        id: "reconciliation".into(),
        revision: "4".into(),
    };
    s.save_guard_candidate(&guard_revision_candidate(&h, &node(1), Some(guard.clone())).unwrap())
        .unwrap();
    h.nodes.get_mut(&node(1)).unwrap().freshness = FreshnessStatus::Contradicted;
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    assert!(
        s.guard_candidates()
            .unwrap()
            .iter()
            .any(|c| c.source_guard == Some(guard.clone())
                && matches!(c.proposed_change, GuardRevisionChange::ReviewRequired))
    );
    assert_eq!(s.knowledge_audit().unwrap()["enforcement_changed"], false);
}

#[test]
fn retired_exception_and_missing_recovery_preserve_constraint() {
    let (_t, s, mut h) = setup("hierarchy.json");
    h.nodes.get_mut(&node(4)).unwrap().maturity = KnowledgeMaturity::Retired;
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    let mut r = runtime(Some(true));
    r.failure_signature = Some(FailureSignatureRef {
        signature: "ambiguous".into(),
    });
    let d = s.record_runtime_decision(&r, Default::default()).unwrap();
    assert_eq!(d.decision.kind(), RuntimeDecisionKind::Replan);
    assert!(
        d.context
            .operational_knowledge
            .unwrap()
            .bundle
            .exceptions
            .is_empty()
    );
}

#[test]
fn changing_authoritative_context_invalidates_guidance() {
    let (_t, s, _) = setup("hierarchy.json");
    let r = runtime(Some(true));
    s.record_runtime_decision(&r, Default::default()).unwrap();
    s.record_context_observation(
        &r.session_id,
        "token_valid",
        &ContextValue {
            value: ScopeValue::Boolean(false),
            source: ContextValueSource::EffectAdapterObserved,
        },
    )
    .unwrap();
    assert!(
        s.check_knowledge_before_commit(&r.session_id.to_string(), "retry-1")
            .is_err()
    );
}

#[test]
fn fixture_exception_escape_rates_are_zero_with_nonzero_denominators() {
    let mut evaluated = 0;
    let mut invalid = 0;
    let mut stale_attempts = 0;
    let mut stale_escapes = 0;
    for freshness in [
        FreshnessStatus::Fresh,
        FreshnessStatus::Stale,
        FreshnessStatus::Contradicted,
    ] {
        for token in [Some(true), Some(false), None] {
            let (_t, s, mut h) = setup("hierarchy.json");
            h.nodes.get_mut(&node(4)).unwrap().freshness = freshness;
            h.revision += 1;
            s.save_knowledge_hierarchy(&h).unwrap();
            let d = s
                .record_runtime_decision(&runtime(token), Default::default())
                .unwrap();
            let k = d.context.operational_knowledge.unwrap();
            let applied = k
                .effective
                .applied
                .iter()
                .any(|a| a.node == node(4) && a.role == AppliedKnowledgeRole::Exception);
            evaluated += 1;
            invalid += usize::from(
                applied && !(freshness == FreshnessStatus::Fresh && token == Some(true)),
            );
            if freshness == FreshnessStatus::Stale && token == Some(true) {
                stale_attempts += 1;
                stale_escapes += usize::from(applied);
            }
        }
    }
    assert_eq!((invalid, evaluated), (0, 9));
    assert_eq!((stale_escapes, stale_attempts), (0, 1));
    let (_t, s, _) = setup("conflicting-siblings.json");
    assert!(
        !resolve(&s, &runtime(Some(true)))
            .unresolved_conflicts
            .is_empty()
    );
}

#[tokio::test]
async fn api_v3_token_boundary_negative_control_replay_and_guard_demo() {
    use hardknock::experimentation::*;
    let f = support::Fixture::new();
    let s = Store::open(&f.home).unwrap();
    let mut h = fixture("hierarchy.json");
    // The local fixture explicitly models API v3 validity through five minutes.
    h = serde_json::from_str(&serde_json::to_string(&h).unwrap().replace("\"2\"", "\"3\""))
        .unwrap();
    h.nodes
        .get_mut(&node(4))
        .unwrap()
        .scope
        .predicates
        .push(ScopePredicate::IntegerRange {
            key: "token_age_minutes".into(),
            min: Some(0),
            max: Some(5),
        });
    s.save_knowledge_hierarchy(&h).unwrap();
    let mut old = runtime(None);
    old.context_observations.get_mut("api_version").unwrap()[0].value =
        ScopeValue::String("3".into());
    let original = s.record_runtime_decision(&old, Default::default()).unwrap();
    assert_eq!(original.decision.kind(), RuntimeDecisionKind::Replan);
    let config = hardknock::bridge::config::Config::default();
    let mut evidence = vec![];
    for (age, token, safe) in [
        (3, true, true),
        (7, true, false),
        (11, true, false),
        (3, false, false),
    ] {
        let mut request = experiment_template(&f);
        request.intent = ExperimentIntent::CompareStrategies;
        request.question = format!("Fixture API v3 replay at {age} minutes, token={token}");
        request.candidates[0].execution = CandidateExecution::Shell {
            commands: vec![format!(
                "if [ {age} -le 5 ] && [ {token} = true ]; then printf safe; else printf unsafe; fi > result"
            )],
        };
        let result = ExperimentOrchestrator {
            store: &s,
            config: &config,
        }
        .run(request, &hardknock::cancellation::Cancellation::default())
        .await
        .unwrap();
        let comparison = result
            .result
            .as_ref()
            .unwrap_or_else(|| panic!("{result:?}"));
        assert_eq!(comparison.quality, ExperimentQuality::Controlled);
        evidence.push(hardknock::epistemic::EvidenceRef {
            kind: "controlled_experiment".into(),
            id: result.id.to_string(),
        });
        let mut r = old.clone();
        r.context_observations.insert(
            "token_valid".into(),
            vec![ContextValue {
                value: ScopeValue::Boolean(token),
                source: ContextValueSource::EffectAdapterObserved,
            }],
        );
        r.context_observations.insert(
            "token_age_minutes".into(),
            vec![ContextValue {
                value: ScopeValue::Integer(age),
                source: ContextValueSource::EffectAdapterObserved,
            }],
        );
        let decision = s.record_runtime_decision(&r, Default::default()).unwrap();
        assert_eq!(
            decision.decision.kind(),
            if safe {
                RuntimeDecisionKind::Act
            } else {
                RuntimeDecisionKind::Replan
            }
        );
        if safe {
            r.capability_context.governance.approval_required = true;
            assert_eq!(
                s.record_runtime_decision(&r, Default::default())
                    .unwrap()
                    .decision
                    .kind(),
                RuntimeDecisionKind::RequireApproval
            );
        }
    }
    h.nodes
        .get_mut(&node(4))
        .unwrap()
        .provenance
        .evidence
        .extend(evidence);
    h.nodes.get_mut(&node(4)).unwrap().artifact.revision += 1;
    h.revision += 1;
    s.save_knowledge_hierarchy(&h).unwrap();
    let replay = s
        .replay_knowledge_decision(&original.id, Default::default())
        .unwrap();
    assert_eq!(
        replay["current_hypothetical_decision"]["decision"]["decision"],
        "replan"
    );
    let candidate = guard_revision_candidate(
        &h,
        &node(4),
        Some(GuardRef {
            id: "reconcile-after-ambiguity".into(),
            revision: "1".into(),
        }),
    )
    .unwrap();
    assert_eq!(candidate.evidence.evidence.len(), 5);
    let artifact = GuardRevisionArtifact::export(candidate).unwrap();
    artifact.verify().unwrap();
    assert!(!artifact.enforcement_changed);
}

#[test]
fn persisted_operational_projection_cannot_omit_constraint() {
    let (_t, s, _) = setup("hierarchy.json");
    let mut record = s
        .record_runtime_decision(&runtime(None), Default::default())
        .unwrap();
    record.id = RuntimeDecisionId::new();
    record
        .context
        .operational_knowledge
        .as_mut()
        .unwrap()
        .constraints
        .clear();
    record.evaluation = DeterministicRuntimeController::default()
        .evaluate(&record.context)
        .unwrap();
    record.decision = record.evaluation.decision.clone();
    record.context_hash = record.context.context_hash().unwrap();
    assert!(
        s.persist_runtime_decision(&record, Default::default())
            .is_err()
    );
}
