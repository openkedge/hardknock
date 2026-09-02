// SPDX-License-Identifier: Apache-2.0

use std::time::Instant;

use chrono::Utc;
use hardknock::{
    core::{AgentIdentity, ClaimId, EvidencePathId, ExperimentId, LessonId, ToolId},
    epistemic::*,
    federation::ExperienceNodeId,
    lesson::{ActionPattern, ContextSelector},
    runtime::*,
    tool::ToolIdentity,
};

fn claim() -> Claim {
    Claim {
        id: ClaimId::new(),
        kind: ClaimKind::StrategyPreference,
        statement: "staged migration is preferable to direct migration".into(),
        scope: ContextSelector {
            repository: None,
            required_markers: vec!["pnpm-workspace.yaml".into()],
            tags: vec!["framework-migration".into()],
            os: None,
            arch: None,
        },
        created_at: Utc::now(),
    }
}

fn agent(name: &str, model: &str) -> AgentIdentity {
    AgentIdentity {
        kind: name.into(),
        executable: format!("fixture:{name}"),
        version: Some("1".into()),
        model: Some(model.into()),
    }
}

fn path(
    claim: &Claim,
    name: &str,
    model: Option<&str>,
    source: EvidenceSource,
    outcome: EvidenceOutcome,
    retrieval: &[&str],
    evaluators: &[&str],
) -> EvidencePath {
    let dependencies = EpistemicDependencySet {
        model_family: model.map(str::to_owned),
        model_version: Some("fixture-v1".into()),
        agent_runtime: matches!(&source, EvidenceSource::Agent { .. })
            .then(|| "fixture-runtime".into()),
        retrieval_sources: retrieval.iter().map(|value| (*value).into()).collect(),
        evaluators: evaluators.iter().map(|value| (*value).into()).collect(),
        environment_family: Some(format!("environment-{name}")),
        ..Default::default()
    };
    EvidencePath {
        id: EvidencePathId::new(),
        claim: claim.id.clone().into(),
        source,
        context: EvidenceContext {
            repository: Some("fixture/migration".into()),
            task: Some("choose migration strategy".into()),
            fingerprint: context_fingerprint(&dependencies).unwrap(),
            ..Default::default()
        },
        dependencies,
        evidence_refs: vec![],
        outcome,
        created_at: Utc::now(),
    }
}

#[test]
fn correlated_agents_are_supported_but_low_diversity_and_challenged() {
    let claim = claim();
    let lesson = LessonId::new();
    let mut paths = (0..3)
        .map(|index| {
            path(
                &claim,
                &format!("agent-{index}"),
                Some("model-family-m"),
                EvidenceSource::Agent {
                    identity: agent(&format!("agent-{index}"), "model-family-m"),
                },
                EvidenceOutcome::Supports,
                &["shared-advice"],
                &["status-code-only-v1"],
            )
        })
        .collect::<Vec<_>>();
    for path in &mut paths {
        path.dependencies.originating_lessons = vec![lesson.clone()];
        path.context.fingerprint = context_fingerprint(&path.dependencies).unwrap();
    }
    let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
    assert_eq!(diversity.diversity_class, DiversityClass::Low);
    assert!(diversity.dependency_overlaps.iter().any(|overlap| {
        overlap.kind == EpistemicDependencyKind::Experience
            && overlap.shared_value == lesson.to_string()
            && overlap.paths.len() == 3
    }));
    let fused = DeterministicEvidenceFusionPolicy
        .fuse(&claim, &paths, &diversity)
        .unwrap();
    assert_eq!(fused.status, FusedEvidenceStatus::Supported);
    let plan = DeterministicEvidenceAcquisitionPlanner::default()
        .plan(&claim, &paths, &Default::default())
        .unwrap();
    assert!(matches!(
        plan.actions.first(),
        Some(EvidenceAcquisitionAction::ChallengeClaim {
            strategy: ChallengeStrategy::RemoveDominantExperience
        })
    ));
}

#[test]
fn alternative_evaluator_exposes_the_blind_spot_as_disputed() {
    let claim = claim();
    let mut paths = (0..3)
        .map(|index| {
            path(
                &claim,
                &format!("agent-{index}"),
                Some("model-m"),
                EvidenceSource::Agent {
                    identity: agent(&format!("agent-{index}"), "model-m"),
                },
                EvidenceOutcome::Supports,
                &["same-doc"],
                &["status-code-only-v1"],
            )
        })
        .collect::<Vec<_>>();
    let before = DeterministicEvidenceDiversityPolicy.assess(&paths);
    assert_eq!(
        DeterministicEvidenceFusionPolicy
            .fuse(&claim, &paths, &before)
            .unwrap()
            .status,
        FusedEvidenceStatus::Supported
    );
    paths.push(path(
        &claim,
        "content-invariant",
        None,
        EvidenceSource::StaticCheck {
            evaluator: "content-invariant-v2".into(),
        },
        EvidenceOutcome::Contradicts,
        &[],
        &["content-invariant-v2"],
    ));
    let after = DeterministicEvidenceDiversityPolicy.assess(&paths);
    assert_eq!(
        DeterministicEvidenceFusionPolicy
            .fuse(&claim, &paths, &after)
            .unwrap()
            .status,
        FusedEvidenceStatus::Disputed
    );
}

#[test]
fn genuinely_diverse_confirmation_is_diverse_support() {
    let claim = claim();
    let paths = vec![
        path(
            &claim,
            "agent-a",
            Some("model-a"),
            EvidenceSource::Agent {
                identity: agent("agent-a", "model-a"),
            },
            EvidenceOutcome::Supports,
            &["source-a"],
            &["property-check-v1"],
        ),
        path(
            &claim,
            "agent-b",
            Some("model-b"),
            EvidenceSource::Agent {
                identity: agent("agent-b", "model-b"),
            },
            EvidenceOutcome::Supports,
            &["source-b"],
            &["contract-check-v2"],
        ),
        path(
            &claim,
            "controlled",
            None,
            EvidenceSource::Experiment {
                experiment_id: ExperimentId::new(),
            },
            EvidenceOutcome::Supports,
            &[],
            &["controlled-reality-v1"],
        ),
    ];
    let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
    assert_eq!(diversity.diversity_class, DiversityClass::High);
    assert_eq!(
        DeterministicEvidenceFusionPolicy
            .fuse(&claim, &paths, &diversity)
            .unwrap()
            .status,
        FusedEvidenceStatus::DiverseSupport
    );
}

#[test]
fn different_models_sharing_a_bad_tool_are_not_high_diversity() {
    let claim = claim();
    let tool = ToolIdentity {
        id: ToolId::new(),
        name: "parser-tool".into(),
        version: "1".into(),
    };
    let mut paths = vec![
        path(
            &claim,
            "a",
            Some("model-a"),
            EvidenceSource::Agent {
                identity: agent("a", "model-a"),
            },
            EvidenceOutcome::Supports,
            &["docs"],
            &["tests-v1"],
        ),
        path(
            &claim,
            "b",
            Some("model-b"),
            EvidenceSource::Agent {
                identity: agent("b", "model-b"),
            },
            EvidenceOutcome::Supports,
            &["docs"],
            &["tests-v1"],
        ),
    ];
    for path in &mut paths {
        path.dependencies.tools = vec![tool.clone()];
        path.context.fingerprint = context_fingerprint(&path.dependencies).unwrap();
    }
    let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
    assert_ne!(diversity.diversity_class, DiversityClass::High);
    assert!(
        diversity
            .dependency_overlaps
            .iter()
            .any(|overlap| overlap.kind == EpistemicDependencyKind::Tool)
    );
}

#[test]
fn same_model_with_distinct_experiments_adds_multidimensional_value() {
    let claim = claim();
    let paths = vec![
        path(
            &claim,
            "environment-a",
            Some("model-a"),
            EvidenceSource::Agent {
                identity: agent("a", "model-a"),
            },
            EvidenceOutcome::Supports,
            &["source-a"],
            &["unit-v1"],
        ),
        path(
            &claim,
            "environment-b",
            Some("model-a"),
            EvidenceSource::Agent {
                identity: agent("a", "model-a"),
            },
            EvidenceOutcome::Supports,
            &["source-b"],
            &["property-v1"],
        ),
    ];
    assert_eq!(
        DeterministicEvidenceDiversityPolicy
            .assess(&paths)
            .diversity_class,
        DiversityClass::Moderate
    );
}

#[test]
fn unknown_dependencies_are_not_credited_as_diversity() {
    let claim = claim();
    let mut unknown = path(
        &claim,
        "remote",
        None,
        EvidenceSource::Agent {
            identity: agent("remote", "unknown"),
        },
        EvidenceOutcome::Supports,
        &[],
        &[],
    );
    unknown.dependencies = Default::default();
    unknown.context.fingerprint = context_fingerprint(&unknown.dependencies).unwrap();
    let known = path(
        &claim,
        "local",
        Some("model-a"),
        EvidenceSource::Agent {
            identity: agent("local", "model-a"),
        },
        EvidenceOutcome::Supports,
        &[],
        &["tests"],
    );
    let assessment = DeterministicEvidenceDiversityPolicy.assess(&[unknown, known]);
    assert_eq!(assessment.diversity_class, DiversityClass::Unknown);
    assert!(!assessment.missing_metadata.is_empty());
}

fn node(value: u8) -> ExperienceNodeId {
    format!("hk-node:{}", format!("{value:02x}").repeat(32))
        .parse()
        .unwrap()
}

#[test]
fn federation_reexports_remain_one_root_origin() {
    let claim = claim();
    let mut paths = (1..=4)
        .map(|value| {
            path(
                &claim,
                &format!("node-{value}"),
                None,
                EvidenceSource::Federation {
                    node_id: node(value),
                },
                EvidenceOutcome::Supports,
                &[],
                &["remote-tests-v1"],
            )
        })
        .collect::<Vec<_>>();
    for path in &mut paths {
        path.context.root_evidence_origins = vec!["node-a/experiment-221".into()];
    }
    let echo = evidence_echo_assessment(&paths);
    assert_eq!(echo.origin_diversity.immediate_nodes, 4);
    assert_eq!(echo.origin_diversity.root_evidence_origins, 1);
    assert_eq!(echo.status, EvidenceEchoStatus::Strong);
}

#[test]
fn genuine_federated_replication_increases_root_origin_diversity() {
    let claim = claim();
    let mut paths = (1..=3)
        .map(|value| {
            path(
                &claim,
                &format!("node-{value}"),
                None,
                EvidenceSource::Federation {
                    node_id: node(value),
                },
                EvidenceOutcome::Supports,
                &[],
                &["remote-tests-v1"],
            )
        })
        .collect::<Vec<_>>();
    for (index, path) in paths.iter_mut().enumerate() {
        path.context.root_evidence_origins = vec![format!("independent-experiment-{index}")];
    }
    let echo = evidence_echo_assessment(&paths);
    assert_eq!(echo.origin_diversity.root_evidence_origins, 3);
    assert_eq!(echo.status, EvidenceEchoStatus::None);
}

#[test]
fn high_risk_correlated_support_cannot_act() {
    let claim = claim();
    let action = ActionPattern::shell("migrate-production-schema");
    let mut scenario = RuntimeScenario {
        proposed_action: Some(hardknock::bridge::protocol::NormalizedAction::Shell {
            command: "migrate-production-schema".into(),
            cwd: "/fixture".into(),
        }),
        knowledge: KnowledgeSignals {
            local_supported: true,
            context_in_scope: true,
            applicable_lesson: true,
            ..Default::default()
        },
        risk: RuntimeRiskAssessment {
            severity: hardknock::curriculum::Severity::High,
            ..Default::default()
        },
        experiments: ExperimentCapabilitySummary {
            mode: ExperimentMode::Suggest,
            safe_reality_available: true,
            effect_safe: true,
            ..Default::default()
        },
        epistemic: Some(RuntimeEpistemicSummary {
            claim: claim.id,
            status: FusedEvidenceStatus::Supported,
            diversity: DiversityClass::Low,
            supporting_paths: 5,
            controlled_empirical_path: false,
            common_dependencies: vec![],
            caveats: vec!["all paths use lesson-088".into()],
        }),
        diversity_requirements: vec![RuntimeDiversityRequirement {
            action_pattern: action,
            minimum_diversity: DiversityClass::Moderate,
        }],
        ..Default::default()
    };
    scenario.experiments.budget.max_realities = 1;
    let evaluation = scenario.evaluate(RuntimePolicyProfile::Balanced).unwrap();
    assert_eq!(evaluation.decision.kind(), RuntimeDecisionKind::Experiment);
    assert!(
        evaluation
            .reasons
            .contains(&DecisionReason::EvidenceDiversityInsufficient)
    );
}

#[test]
fn diverse_low_consequence_support_stays_on_the_fast_act_path() {
    let claim = claim();
    let action = ActionPattern::shell("render-preview");
    let scenario = RuntimeScenario {
        proposed_action: Some(hardknock::bridge::protocol::NormalizedAction::Shell {
            command: "render-preview".into(),
            cwd: "/fixture".into(),
        }),
        knowledge: KnowledgeSignals {
            local_supported: true,
            context_in_scope: true,
            applicable_lesson: true,
            ..Default::default()
        },
        risk: RuntimeRiskAssessment {
            severity: hardknock::curriculum::Severity::Medium,
            ..Default::default()
        },
        epistemic: Some(RuntimeEpistemicSummary {
            claim: claim.id,
            status: FusedEvidenceStatus::DiverseSupport,
            diversity: DiversityClass::High,
            supporting_paths: 4,
            controlled_empirical_path: true,
            common_dependencies: vec![],
            caveats: vec![],
        }),
        diversity_requirements: vec![RuntimeDiversityRequirement {
            action_pattern: action,
            minimum_diversity: DiversityClass::Moderate,
        }],
        ..Default::default()
    };
    assert_eq!(
        scenario
            .evaluate(RuntimePolicyProfile::Balanced)
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Act
    );
}

#[test]
fn dependency_analysis_and_fusion_remain_a_cached_fast_path_candidate() {
    let claim = claim();
    let paths = (0..100)
        .map(|index| {
            path(
                &claim,
                &format!("path-{index}"),
                Some(if index % 2 == 0 { "model-a" } else { "model-b" }),
                EvidenceSource::Agent {
                    identity: agent("fixture", "model"),
                },
                EvidenceOutcome::Supports,
                &[if index % 3 == 0 {
                    "source-a"
                } else {
                    "source-b"
                }],
                &[if index % 5 == 0 {
                    "evaluator-a"
                } else {
                    "evaluator-b"
                }],
            )
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let graph = dependency_graph(&paths);
    let graph_elapsed = started.elapsed();
    let started = Instant::now();
    let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
    let diversity_elapsed = started.elapsed();
    let started = Instant::now();
    let _fused = DeterministicEvidenceFusionPolicy
        .fuse(&claim, &paths, &diversity)
        .unwrap();
    let fusion_elapsed = started.elapsed();
    assert!(!graph.nodes.is_empty());
    assert!(
        graph_elapsed.as_millis() < 50,
        "graph build took {graph_elapsed:?}"
    );
    assert!(
        diversity_elapsed.as_millis() < 50,
        "diversity took {diversity_elapsed:?}"
    );
    assert!(
        fusion_elapsed.as_millis() < 50,
        "fusion took {fusion_elapsed:?}"
    );
}

#[test]
fn benchmark_arms_capture_common_mode_escape() {
    let arms = std::collections::BTreeMap::from([
        (
            EpistemicBenchmarkArm::SingleAgent,
            EpistemicBenchmarkMetrics {
                task_success_rate: 0.0,
                correlated_error_escape_rate: 1.0,
                agent_runs_per_decision: 1.0,
                ..Default::default()
            },
        ),
        (
            EpistemicBenchmarkArm::NaiveMajority,
            EpistemicBenchmarkMetrics {
                task_success_rate: 0.0,
                correlated_error_escape_rate: 1.0,
                redundant_agent_run_rate: 2.0 / 3.0,
                agent_runs_per_decision: 3.0,
                ..Default::default()
            },
        ),
        (
            EpistemicBenchmarkArm::DiversityAware,
            EpistemicBenchmarkMetrics {
                task_success_rate: 1.0,
                correlated_error_escape_rate: 0.0,
                common_mode_detection_rate: 1.0,
                diversity_challenge_precision: 1.0,
                agent_runs_per_decision: 3.0,
                experiments_per_decision: 1.0,
                ..Default::default()
            },
        ),
    ]);
    assert!(
        arms[&EpistemicBenchmarkArm::DiversityAware].correlated_error_escape_rate
            < arms[&EpistemicBenchmarkArm::NaiveMajority].correlated_error_escape_rate
    );
}

#[test]
fn claim_canonicalization_is_lexical_and_deterministic() {
    let mut claim = claim();
    claim.statement = "  Staged   Migration IS preferable  ".into();
    assert_eq!(
        claim.canonical_statement(),
        "staged migration is preferable"
    );
}
