// SPDX-License-Identifier: Apache-2.0
use hardknock::{core::*, hierarchy::*, store::Store};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Instant};
fn fixture(name: &str) -> KnowledgeHierarchy {
    serde_json::from_str(&std::fs::read_to_string(path(name)).unwrap()).unwrap()
}
fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/hierarchy/idempotency")
        .join(name)
}
fn context(name: &str) -> KnowledgeContext {
    KnowledgeContext::from_json(
        serde_json::from_str(&std::fs::read_to_string(path(name)).unwrap()).unwrap(),
    )
    .unwrap()
}
fn nid(n: usize) -> KnowledgeNodeId {
    format!("knowledge-node-00000000-0000-4000-8000-{n:012}")
        .parse()
        .unwrap()
}
fn eid(n: usize) -> KnowledgeHierarchyEdgeId {
    format!("hierarchy-edge-00000000-0000-4000-8000-{n:012}")
        .parse()
        .unwrap()
}
fn scope(predicates: Vec<ScopePredicate>) -> KnowledgeScope {
    KnowledgeScope { predicates }
}
fn eq(key: &str, value: &str) -> ScopePredicate {
    ScopePredicate::Equals {
        key: key.into(),
        value: ScopeValue::String(value.into()),
    }
}
fn resolve(h: &KnowledgeHierarchy) -> EffectiveKnowledge {
    DeterministicKnowledgeResolver
        .resolve(
            h,
            &context("valid-token.json"),
            &KnowledgeResolutionPolicy::default(),
        )
        .unwrap()
}
fn pair(relation: KnowledgeHierarchyRelation) -> KnowledgeHierarchy {
    let mut h = fixture("hierarchy.json");
    h.nodes.retain(|id, _| id == &nid(1) || id == &nid(2));
    h.edges.truncate(1);
    h.edges[0].relation = relation;
    h
}
fn has_issue(h: &KnowledgeHierarchy, kind: HierarchyValidationIssueKind) {
    assert!(
        validate_hierarchy(h).errors.iter().any(|i| i.kind == kind),
        "{:?}",
        validate_hierarchy(h)
    );
    assert!(
        DeterministicKnowledgeResolver
            .resolve(
                h,
                &KnowledgeContext::default(),
                &KnowledgeResolutionPolicy::default()
            )
            .is_err()
    );
}
#[test]
fn builtin_scope_semantics_and_missing_values() {
    let c = KnowledgeContext::from_json(json!({"provider":"X","token":true,"n":12,"v":"2.1.3"}))
        .unwrap();
    let predicates = vec![
        eq("provider", "X"),
        ScopePredicate::NotEquals {
            key: "provider".into(),
            value: ScopeValue::String("Y".into()),
        },
        ScopePredicate::In {
            key: "provider".into(),
            values: vec![ScopeValue::String("X".into())],
        },
        ScopePredicate::Exists {
            key: "provider".into(),
        },
        ScopePredicate::Bool {
            key: "token".into(),
            expected: true,
        },
        ScopePredicate::IntegerRange {
            key: "n".into(),
            min: Some(10),
            max: Some(20),
        },
        ScopePredicate::VersionRange {
            key: "v".into(),
            requirement: "^2.1".into(),
        },
    ];
    for p in predicates {
        let a = DeterministicApplicabilityEvaluator.evaluate(&scope(vec![p.clone()]), &c);
        assert_eq!(a.status, ApplicabilityStatus::Applicable, "{p:?}");
        assert_eq!(
            DeterministicApplicabilityEvaluator
                .evaluate(&scope(vec![p]), &KnowledgeContext::default())
                .status,
            ApplicabilityStatus::Unknown
        );
    }
    for p in [
        eq("provider", "Y"),
        ScopePredicate::NotEquals {
            key: "provider".into(),
            value: ScopeValue::String("X".into()),
        },
        ScopePredicate::In {
            key: "provider".into(),
            values: vec![],
        },
        ScopePredicate::Bool {
            key: "token".into(),
            expected: false,
        },
        ScopePredicate::IntegerRange {
            key: "n".into(),
            min: None,
            max: Some(11),
        },
        ScopePredicate::VersionRange {
            key: "v".into(),
            requirement: ">=3".into(),
        },
    ] {
        assert_eq!(
            DeterministicApplicabilityEvaluator
                .evaluate(&scope(vec![p]), &c)
                .status,
            ApplicabilityStatus::Inapplicable
        );
    }
    assert_eq!(
        DeterministicApplicabilityEvaluator
            .evaluate(&scope(vec![eq("provider", "X"), eq("missing", "x")]), &c)
            .status,
        ApplicabilityStatus::PartiallyKnown
    );
    assert_eq!(
        DeterministicApplicabilityEvaluator
            .evaluate(&scope(vec![eq("provider", "Y"), eq("missing", "x")]), &c)
            .status,
        ApplicabilityStatus::Inapplicable
    );
    assert_eq!(
        DeterministicApplicabilityEvaluator
            .evaluate(&KnowledgeScope::default(), &c)
            .status,
        ApplicabilityStatus::Applicable
    );
    for p in [
        ScopePredicate::Custom {
            kind: "unregistered".into(),
            payload: json!({}),
        },
        ScopePredicate::VersionRange {
            key: "v".into(),
            requirement: "nonsense".into(),
        },
        ScopePredicate::IntegerRange {
            key: "n".into(),
            min: Some(2),
            max: Some(1),
        },
    ] {
        assert_eq!(
            DeterministicApplicabilityEvaluator
                .evaluate(&scope(vec![p]), &c)
                .status,
            ApplicabilityStatus::Unknown
        );
    }
}
#[test]
fn scope_relations_are_containment_not_counts() {
    let compare =
        |a: &KnowledgeScope, b: &KnowledgeScope| DeterministicScopeRelationEvaluator.compare(a, b);
    let a = scope(vec![eq("provider", "X")]);
    let b = scope(vec![eq("provider", "X"), eq("region", "west")]);
    assert_eq!(compare(&a, &a), ScopeRelation::Equal);
    assert_eq!(compare(&b, &a), ScopeRelation::Narrower);
    assert_eq!(compare(&a, &b), ScopeRelation::Broader);
    assert_eq!(
        compare(&a, &scope(vec![eq("provider", "Y")])),
        ScopeRelation::Disjoint
    );
    assert_eq!(
        compare(&a, &scope(vec![eq("region", "west")])),
        ScopeRelation::Overlapping
    );
    assert_eq!(
        compare(
            &a,
            &scope(vec![ScopePredicate::Custom {
                kind: "x".into(),
                payload: json!({})
            }])
        ),
        ScopeRelation::Unknown
    );
    let range = |min, max| {
        scope(vec![ScopePredicate::IntegerRange {
            key: "n".into(),
            min: Some(min),
            max: Some(max),
        }])
    };
    assert_eq!(
        compare(&range(1, 5), &range(0, 10)),
        ScopeRelation::Narrower
    );
    assert_eq!(
        compare(&range(1, 5), &range(4, 10)),
        ScopeRelation::Overlapping
    );
    assert_eq!(
        compare(&range(1, 5), &range(6, 10)),
        ScopeRelation::Disjoint
    );
    let exists = scope(vec![ScopePredicate::Exists {
        key: "provider".into(),
    }]);
    assert_eq!(compare(&a, &exists), ScopeRelation::Narrower);
    let ne = scope(vec![ScopePredicate::NotEquals {
        key: "provider".into(),
        value: ScopeValue::String("Y".into()),
    }]);
    assert_eq!(compare(&a, &ne), ScopeRelation::Narrower);
    let set = scope(vec![ScopePredicate::In {
        key: "provider".into(),
        values: vec![
            ScopeValue::String("Y".into()),
            ScopeValue::String("X".into()),
        ],
    }]);
    assert_eq!(compare(&a, &set), ScopeRelation::Narrower);
    let duplicate = scope(vec![eq("provider", "X"), eq("provider", "X")]);
    assert_eq!(compare(&a, &duplicate), ScopeRelation::Equal);
    assert_eq!(duplicate.specificity().exact_predicates, 1);
    let v = scope(vec![ScopePredicate::VersionRange {
        key: "v".into(),
        requirement: "^2".into(),
    }]);
    assert_eq!(compare(&v, &v), ScopeRelation::Equal);
    assert_eq!(
        compare(&scope(vec![eq("v", "2.1.0")]), &v),
        ScopeRelation::Narrower
    );
}
#[test]
fn rejects_self_missing_and_cycles() {
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.edges[0].child = nid(1);
    has_issue(&h, HierarchyValidationIssueKind::SelfReference);
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.edges[0].child = nid(99);
    has_issue(&h, HierarchyValidationIssueKind::MissingNode);
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    let mut e = h.edges[0].clone();
    e.id = eid(2);
    e.parent = nid(2);
    e.child = nid(1);
    h.edges.push(e);
    has_issue(&h, HierarchyValidationIssueKind::Cycle);
    for e in &mut h.edges {
        e.relation = KnowledgeHierarchyRelation::Supersedes
    }
    has_issue(&h, HierarchyValidationIssueKind::SupersessionCycle);
    let mut h = fixture("hierarchy.json");
    h.nodes
        .retain(|id, _| [nid(1), nid(2), nid(3)].contains(id));
    h.edges.truncate(2);
    let mut e = h.edges[0].clone();
    e.id = eid(3);
    e.parent = nid(3);
    e.child = nid(1);
    h.edges.push(e);
    has_issue(&h, HierarchyValidationIssueKind::Cycle);
}
#[test]
fn scope_and_artifact_validation() {
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.nodes.get_mut(&nid(1)).unwrap().scope = scope(vec![eq("provider", "X")]);
    h.nodes.get_mut(&nid(2)).unwrap().scope = KnowledgeScope::default();
    has_issue(&h, HierarchyValidationIssueKind::InvalidSpecializationScope);
    h.edges[0].relation = KnowledgeHierarchyRelation::Excepts;
    h.nodes.get_mut(&nid(2)).unwrap().scope = scope(vec![eq("provider", "Y")]);
    has_issue(&h, HierarchyValidationIssueKind::InvalidExceptionScope);
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.nodes.get_mut(&nid(2)).unwrap().artifact.kind = KnowledgeArtifactKind::Recovery;
    has_issue(&h, HierarchyValidationIssueKind::IncompatibleArtifactKinds);
    h.edges[0].relation = KnowledgeHierarchyRelation::DependsOn;
    h.root_nodes.push(nid(2));
    assert!(validate_hierarchy(&h).valid);
    h.nodes.get_mut(&nid(1)).unwrap().artifact.kind = KnowledgeArtifactKind::CausalMechanism;
    assert!(validate_hierarchy(&h).valid);
}
#[test]
fn specializes_and_refines() {
    let h = pair(KnowledgeHierarchyRelation::Specializes);
    let r = resolve(&h);
    assert_eq!(
        r.applied.iter().find(|a| a.node == nid(1)).unwrap().role,
        AppliedKnowledgeRole::SupportingContext
    );
    assert_eq!(
        r.applied.iter().find(|a| a.node == nid(2)).unwrap().role,
        AppliedKnowledgeRole::Primary
    );
    let mut h = pair(KnowledgeHierarchyRelation::Refines);
    for n in h.nodes.values_mut() {
        n.artifact.kind = KnowledgeArtifactKind::Recovery
    }
    let r = resolve(&h);
    assert_eq!(r.applied.len(), 2);
    assert_eq!(r.applied[0].role, AppliedKnowledgeRole::Primary);
    assert_eq!(r.applied[1].role, AppliedKnowledgeRole::Refinement);
}
#[test]
fn valid_expired_unknown_wrong_provider_and_stale_fixture() {
    let h = fixture("hierarchy.json");
    assert!(validate_hierarchy(&h).valid);
    let r = resolve(&h);
    assert!(
        r.applied
            .iter()
            .any(|a| a.node == nid(4) && a.role == AppliedKnowledgeRole::Exception)
    );
    assert!(
        r.suppressed
            .iter()
            .any(|s| s.node == nid(1) && s.reason == SuppressionReason::ExplicitException)
    );
    for c in [
        "expired-token.json",
        "unknown-token.json",
        "wrong-provider.json",
    ] {
        let r = DeterministicKnowledgeResolver
            .resolve(&h, &context(c), &KnowledgeResolutionPolicy::default())
            .unwrap();
        assert!(r.applied.iter().any(|a| a.node == nid(1)));
        assert!(!r.applied.iter().any(|a| a.node == nid(4)));
        if c == "unknown-token.json" {
            assert!(r.unknown.iter().any(|a| a.node == nid(4)));
            assert!(
                r.trace
                    .iter()
                    .any(|s| s.node == nid(4) && s.action == ResolutionAction::ScopeUnknown)
            );
        }
        if c == "wrong-provider.json" {
            assert!(
                !r.applied
                    .iter()
                    .any(|a| [nid(3), nid(4), nid(5)].contains(&a.node))
            );
        }
    }
    let r = resolve(&fixture("stale-exception.json"));
    assert!(r.applied.iter().any(|a| a.node == nid(1)));
    assert!(r.advisory.iter().any(|a| a.node == nid(4)));
}
#[test]
fn unhealthy_exceptions_never_override() {
    for state in [
        "candidate",
        "quarantined",
        "disabled",
        "contradicted",
        "partially_stale",
        "unknown",
        "unsupported",
        "retired",
        "overgeneralized",
        "advisory",
    ] {
        let mut h = pair(KnowledgeHierarchyRelation::Excepts);
        let n = h.nodes.get_mut(&nid(2)).unwrap();
        match state {
            "candidate" => n.maturity = KnowledgeMaturity::Candidate,
            "quarantined" => n.activation = KnowledgeActivationState::Quarantined,
            "disabled" => n.activation = KnowledgeActivationState::Disabled,
            "contradicted" => n.maturity = KnowledgeMaturity::Contradicted,
            "partially_stale" => n.freshness = FreshnessStatus::PartiallyStale,
            "unknown" => n.freshness = FreshnessStatus::Unknown,
            "unsupported" => n.provenance.evidence.clear(),
            "retired" => n.maturity = KnowledgeMaturity::Retired,
            "overgeneralized" => n.maturity = KnowledgeMaturity::Overgeneralized,
            _ => n.activation = KnowledgeActivationState::Advisory,
        }
        let r = resolve(&h);
        assert!(r.applied.iter().any(|a| a.node == nid(1)), "{state}");
        assert!(!r.applied.iter().any(|a| a.node == nid(2)), "{state}");
    }
    let mut h = pair(KnowledgeHierarchyRelation::Excepts);
    h.edges[0].evidence.clear();
    assert!(resolve(&h).applied.iter().any(|a| a.node == nid(1)));
}
#[test]
fn supersession_is_explicit_and_scoped() {
    let h = fixture("supersession.json");
    let r = DeterministicKnowledgeResolver
        .resolve(
            &h,
            &context("api-v3.json"),
            &KnowledgeResolutionPolicy::default(),
        )
        .unwrap();
    assert!(r.applied.iter().any(|a| a.node == nid(8)));
    assert!(
        r.suppressed
            .iter()
            .any(|s| s.node == nid(7) && s.reason == SuppressionReason::Superseded)
    );
    let r = resolve(&h);
    assert!(r.applied.iter().any(|a| a.node == nid(7)));
    assert!(!r.applied.iter().any(|a| a.node == nid(8)));
}
#[test]
fn unknown_specificity_cannot_override() {
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.nodes
        .get_mut(&nid(2))
        .unwrap()
        .scope
        .predicates
        .push(eq("unavailable", "yes"));
    let r = resolve(&h);
    assert_eq!(r.applied.len(), 1);
    assert_eq!(r.applied[0].role, AppliedKnowledgeRole::Primary);
    assert_eq!(r.unknown[0].node, nid(2));
}
#[test]
fn competing_siblings_and_composable_refinements() {
    for relation in [
        KnowledgeHierarchyRelation::Specializes,
        KnowledgeHierarchyRelation::Excepts,
        KnowledgeHierarchyRelation::Supersedes,
        KnowledgeHierarchyRelation::Refines,
    ] {
        let mut h = pair(relation);
        let mut n = h.nodes[&nid(2)].clone();
        n.id = nid(3);
        n.artifact.id = "opposite-guidance".into();
        h.nodes.insert(n.id.clone(), n);
        let mut e = h.edges[0].clone();
        e.id = eid(2);
        e.child = nid(3);
        h.edges.push(e);
        let r = resolve(&h);
        assert_eq!(
            r.conflicts.is_empty(),
            relation == KnowledgeHierarchyRelation::Refines
        );
        assert!(r.applied.iter().any(|a| a.node == nid(1)));
        assert_eq!(r.applied.len(), 3);
    }
    let r = resolve(&fixture("conflicting-siblings.json"));
    assert!(!r.conflicts.is_empty());
    assert!(
        !r.suppressed
            .iter()
            .any(|s| s.reason == SuppressionReason::ExplicitException)
    );
}
#[test]
fn dependencies_propagate_health_without_precedence() {
    let mut h = pair(KnowledgeHierarchyRelation::DependsOn);
    h.root_nodes.push(nid(2));
    h.nodes.get_mut(&nid(1)).unwrap().freshness = FreshnessStatus::Stale;
    let r = resolve(&h);
    assert!(r.applied.is_empty());
    assert!(r.unknown.iter().any(|u| u.node == nid(2)));
    let mut e = h.edges[0].clone();
    e.id = eid(2);
    e.parent = nid(2);
    e.child = nid(1);
    h.edges.push(e);
    h.nodes.get_mut(&nid(1)).unwrap().freshness = FreshnessStatus::Fresh;
    assert!(validate_hierarchy(&h).valid);
    assert!(resolve(&h).applied.is_empty());
}
#[test]
fn deterministic_output_and_trace_under_edge_permutation() {
    let mut h = fixture("hierarchy.json");
    let a = serde_json::to_value(resolve(&h)).unwrap();
    h.edges.reverse();
    h.root_nodes.reverse();
    for n in h.nodes.values_mut() {
        n.scope.predicates.reverse()
    }
    let b = serde_json::to_value(resolve(&h)).unwrap();
    assert_eq!(a, b);
    let r = resolve(&h);
    assert!(
        r.trace
            .iter()
            .enumerate()
            .all(|(i, s)| s.sequence == i as u64 + 1)
    );
}
#[test]
fn sqlite_roundtrip_revision_check_and_cli() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("store");
    let store = Store::open(&home).unwrap();
    let mut h = fixture("hierarchy.json");
    store.save_knowledge_hierarchy(&h).unwrap();
    assert!(store.save_knowledge_hierarchy(&h).is_err());
    h.revision += 1;
    store.save_knowledge_hierarchy(&h).unwrap();
    assert_eq!(store.knowledge_hierarchy(&h.id).unwrap().revision, 2);
    assert_eq!(
        serde_json::to_value(resolve(&h)).unwrap(),
        serde_json::to_value(resolve(&store.knowledge_hierarchy(&h.id).unwrap())).unwrap()
    );
    for args in [
        vec!["hierarchy", "show"],
        vec!["hierarchy", "validate"],
        vec![
            "resolve",
            "--context",
            path("valid-token.json").to_str().unwrap(),
        ],
        vec![
            "explain",
            "--context",
            path("unknown-token.json").to_str().unwrap(),
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_hardknock"))
            .arg("--home")
            .arg(&home)
            .args(["--json", "knowledge"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(v["result"]["schema_version"], 1);
    }
}
#[test]
fn legacy_adapters_preserve_unknown_and_abstract_compatibility() {
    use hardknock::abstraction::*;
    let source = ApplicabilityPredicate {
        all_of: vec![ApplicabilityClause {
            variable: ContextVariableKind::Custom("x".into()),
            name: "tags".into(),
            operator: PredicateOperator::Contains,
            value: Some(VariableValue::Set(vec!["a".into()])),
            rationale: "set membership".into(),
        }],
    };
    assert_eq!(
        DeterministicApplicabilityEvaluator
            .evaluate(&KnowledgeScope::from(&source), &KnowledgeContext::default())
            .status,
        ApplicabilityStatus::Unknown
    );
    let mut h = pair(KnowledgeHierarchyRelation::Specializes);
    h.nodes.get_mut(&nid(1)).unwrap().artifact.kind =
        KnowledgeArtifactKind::AbstractKnowledge(AbstractKnowledgeKind::AbstractConstraint);
    assert!(validate_hierarchy(&h).valid);
}
#[test]
#[ignore = "manual scaling measurement: cargo test --test knowledge_hierarchy hierarchy_scaling -- --ignored --nocapture"]
fn hierarchy_scaling() {
    for count in [100, 1000, 10000] {
        let seed = fixture("hierarchy.json");
        let mut h = seed.clone();
        h.nodes = BTreeMap::new();
        h.edges.clear();
        h.root_nodes = vec![nid(1)];
        for i in 1..=count {
            let mut n = seed.nodes[&nid(1)].clone();
            n.id = nid(i);
            n.artifact.id = format!("synthetic-{i}");
            h.nodes.insert(n.id.clone(), n);
            if i > 1 {
                let mut e = seed.edges[0].clone();
                e.id = eid(i);
                e.parent = nid((i - 2) / 4 + 1);
                e.child = nid(i);
                e.relation = KnowledgeHierarchyRelation::Refines;
                h.edges.push(e);
            }
        }
        let c = KnowledgeContext::default();
        let start = Instant::now();
        for n in h.nodes.values() {
            assert_eq!(
                DeterministicApplicabilityEvaluator
                    .evaluate(&n.scope, &c)
                    .status,
                ApplicabilityStatus::Applicable
            )
        }
        let filter = start.elapsed();
        let start = Instant::now();
        let r = DeterministicKnowledgeResolver
            .resolve(&h, &c, &KnowledgeResolutionPolicy::default())
            .unwrap();
        let resolution = start.elapsed();
        let start = Instant::now();
        let bytes = serde_json::to_vec(&r.trace).unwrap();
        let trace = start.elapsed();
        assert_eq!(r.applied.len(), count);
        println!(
            "nodes={count} filter_us={} resolution_with_trace_us={} trace_serialization_us={} trace_steps={} trace_bytes={}",
            filter.as_micros(),
            resolution.as_micros(),
            trace.as_micros(),
            r.trace.len(),
            bytes.len()
        );
    }
}

#[test]
fn multi_parent_conflict_is_found_before_any_override() {
    let mut h = pair(KnowledgeHierarchyRelation::Excepts);
    for i in [3, 4] {
        let mut n = h.nodes[&nid(1)].clone();
        n.id = nid(i);
        n.artifact.id = format!("additional-{i}");
        h.nodes.insert(n.id.clone(), n);
    }
    h.root_nodes.push(nid(3));
    for (i, p, c) in [(2, 3, 2), (3, 3, 4)] {
        let mut e = h.edges[0].clone();
        e.id = eid(i);
        e.parent = nid(p);
        e.child = nid(c);
        h.edges.push(e);
    }
    let r = resolve(&h);
    assert!(!r.conflicts.is_empty());
    assert!(r.suppressed.is_empty());
    assert!(
        r.applied
            .iter()
            .any(|a| a.node == nid(1) && a.role == AppliedKnowledgeRole::Primary)
    );
}
#[test]
fn explicit_supersession_chain_records_live_replacement() {
    let mut h = pair(KnowledgeHierarchyRelation::Supersedes);
    let mut n = h.nodes[&nid(2)].clone();
    n.id = nid(3);
    n.artifact.id = "newest".into();
    h.nodes.insert(n.id.clone(), n);
    let mut e = h.edges[0].clone();
    e.id = eid(2);
    e.parent = nid(2);
    e.child = nid(3);
    h.edges.push(e);
    let r = resolve(&h);
    assert_eq!(r.applied.len(), 1);
    assert_eq!(r.applied[0].node, nid(3));
    assert_eq!(r.suppressed.len(), 2);
    assert!(r.suppressed.iter().all(|s| s.suppressed_by == Some(nid(3))));
}
#[test]
fn version_intervals_and_prerelease_uncertainty() {
    let version = |req: &str| {
        scope(vec![ScopePredicate::VersionRange {
            key: "v".into(),
            requirement: req.into(),
        }])
    };
    let compare =
        |a: &str, b: &str| DeterministicScopeRelationEvaluator.compare(&version(a), &version(b));
    assert_eq!(compare(">=2.1, <2.5", "^2"), ScopeRelation::Narrower);
    assert_eq!(compare("^2", "^3"), ScopeRelation::Disjoint);
    assert_eq!(compare(">=2, <4", ">=3, <5"), ScopeRelation::Overlapping);
    assert_eq!(compare("~2.1", ">=2.1, <2.2"), ScopeRelation::Equal);
    assert_eq!(compare("^0.2", ">=0.2, <0.3"), ScopeRelation::Equal);
    assert_eq!(compare(">=2.0.0-alpha", "^2"), ScopeRelation::Unknown);
}
#[test]
fn invalid_policy_and_forest_roots_are_rejected() {
    let h = fixture("hierarchy.json");
    let p = KnowledgeResolutionPolicy {
        minimum_primary_maturity: KnowledgeMaturity::Retired,
        ..Default::default()
    };
    assert!(
        DeterministicKnowledgeResolver
            .resolve(&h, &KnowledgeContext::default(), &p)
            .is_err()
    );
    let p = KnowledgeResolutionPolicy {
        stale_exception_can_override: true,
        ..Default::default()
    };
    assert!(
        DeterministicKnowledgeResolver
            .resolve(&h, &KnowledgeContext::default(), &p)
            .is_err()
    );
    let mut h = h;
    h.root_nodes.clear();
    has_issue(&h, HierarchyValidationIssueKind::OrphanedReference);
}

#[test]
fn specialization_conflict_cannot_relax_another_parent_as_exception() {
    let mut h = pair(KnowledgeHierarchyRelation::Excepts);
    for i in [3, 4] {
        let mut n = h.nodes[&nid(1)].clone();
        n.id = nid(i);
        n.artifact.id = format!("additional-{i}");
        h.nodes.insert(n.id.clone(), n);
    }
    h.root_nodes.push(nid(3));
    for (i, p, c) in [(2, 3, 2), (3, 3, 4)] {
        let mut e = h.edges[0].clone();
        e.id = eid(i);
        e.parent = nid(p);
        e.child = nid(c);
        e.relation = KnowledgeHierarchyRelation::Specializes;
        h.edges.push(e);
    }
    let r = resolve(&h);
    assert!(!r.conflicts.is_empty());
    assert!(r.suppressed.is_empty());
    assert!(
        r.applied
            .iter()
            .any(|a| a.node == nid(1) && a.role == AppliedKnowledgeRole::Primary)
    );
}
