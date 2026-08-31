// SPDX-License-Identifier: Apache-2.0

mod support;

use std::{collections::BTreeSet, fs, path::Path};

use chrono::{Duration, Utc};
use hardknock::{
    bridge::protocol::NormalizedAction,
    core::{ChaosTrialId, ReflexId},
    lesson::{ActionPattern, ConfidenceScore, ContextSelector},
    resilience::{Reflex, ReflexResponse, ReflexStatus, ResilienceTestStatus, TriggerPattern},
    runtime::*,
    store::{RuntimeStore, Store},
};
use rusqlite::params;
use support::Fixture;

fn fixture(name: &str) -> RuntimeScenario {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/runtime-scenarios")
        .join(format!("{name}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn scenario_library_is_structured_and_matches_declared_decisions() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/runtime-scenarios");
    let mut count = 0;
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let scenario: RuntimeScenario =
            serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
        scenario.validate().unwrap();
        let expected = scenario
            .expected_decision
            .expect("fixture declares decision");
        assert_eq!(
            scenario
                .evaluate(RuntimePolicyProfile::Balanced)
                .unwrap()
                .decision
                .kind(),
            expected,
            "{}",
            scenario.name
        );
        count += 1;
    }
    assert!(count >= 12);
}

#[test]
fn all_six_runtime_outcomes_are_first_class() {
    let decisions = benchmark_scenarios()
        .into_iter()
        .map(|scenario| {
            scenario
                .evaluate(RuntimePolicyProfile::Balanced)
                .unwrap()
                .decision
                .kind()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        decisions,
        BTreeSet::from([
            RuntimeDecisionKind::Act,
            RuntimeDecisionKind::Experiment,
            RuntimeDecisionKind::Replan,
            RuntimeDecisionKind::Recover,
            RuntimeDecisionKind::RequireApproval,
            RuntimeDecisionKind::Abstain,
        ])
    );
}

#[test]
fn adaptive_learning_negative_learning_staleness_and_trust_change_control() {
    let scenarios = benchmark_scenarios();
    let kind = |prefix: &str| {
        scenarios
            .iter()
            .find(|scenario| scenario.name.starts_with(prefix))
            .unwrap()
            .evaluate(RuntimePolicyProfile::Balanced)
            .unwrap()
            .decision
            .kind()
    };
    assert_eq!(
        kind("growing-experience-before"),
        RuntimeDecisionKind::Experiment
    );
    assert_eq!(kind("growing-experience-after"), RuntimeDecisionKind::Act);
    assert_eq!(kind("reflex-"), RuntimeDecisionKind::Replan);
    assert_eq!(kind("stale-lesson"), RuntimeDecisionKind::Experiment);
    assert_eq!(kind("federated-advisory"), RuntimeDecisionKind::Experiment);
    assert_eq!(
        kind("certification-out-of-scope"),
        RuntimeDecisionKind::Experiment
    );
}

#[test]
fn balanced_experiments_where_conservative_abstains() {
    let scenario = fixture("unknown-high-risk");
    assert_eq!(
        scenario
            .evaluate(RuntimePolicyProfile::Balanced)
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Experiment
    );
    assert_eq!(
        scenario
            .evaluate(RuntimePolicyProfile::Conservative)
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Abstain
    );
}

#[test]
fn hard_policy_and_capability_precede_experience() {
    let mut scenario = fixture("known-safe");
    scenario.capability.governance.hard_policy_blocked = true;
    scenario.capability.governance.block_reason = Some("organization policy".into());
    let evaluation = scenario.evaluate(RuntimePolicyProfile::Balanced).unwrap();
    assert_eq!(evaluation.decision.kind(), RuntimeDecisionKind::Abstain);
    assert_eq!(
        evaluation.governance,
        GovernanceDisposition::SecurityBlocked
    );
    assert!(
        evaluation
            .reasons
            .contains(&DecisionReason::HardPolicyPrecedence)
    );

    let mut missing = fixture("known-safe");
    missing.capability.required_available = false;
    missing
        .capability
        .missing
        .push(hardknock::capability::ExecutionCapability::ProcessExecute(
            hardknock::capability::ExecutablePattern("deploy".into()),
        ));
    assert_eq!(
        missing
            .evaluate(RuntimePolicyProfile::Balanced)
            .unwrap()
            .decision
            .kind(),
        RuntimeDecisionKind::Abstain
    );
}

#[test]
fn approval_and_abstention_remain_distinct() {
    let approval = fixture("approval-required")
        .evaluate(RuntimePolicyProfile::Balanced)
        .unwrap();
    assert_eq!(
        approval.decision.kind(),
        RuntimeDecisionKind::RequireApproval
    );
    assert!(
        approval
            .blockers
            .contains(&DecisionBlocker::MissingCommitAuthority)
    );

    let abstention = fixture("unsupported-effect")
        .evaluate(RuntimePolicyProfile::Balanced)
        .unwrap();
    assert_eq!(abstention.decision.kind(), RuntimeDecisionKind::Abstain);
    let RuntimeDecision::Abstain(detail) = abstention.decision else {
        unreachable!()
    };
    assert_eq!(detail.reason, AbstentionReason::UnsupportedEffect);
    assert!(!detail.possible_next_steps.is_empty());
}

#[test]
fn structured_effect_risk_and_narrow_assured_tool_are_selected() {
    let scenario = fixture("unsupported-effect");
    let context = scenario.decision_context().unwrap();
    let risk = DeterministicRuntimeRiskPolicy.assess(&context);
    assert_eq!(risk.severity, hardknock::curriculum::Severity::High);
    assert_eq!(
        risk.externality,
        hardknock::effects::ExternalityClass::HumanVisible
    );

    let mut tool_scenario = fixture("known-safe");
    tool_scenario.tool_candidates = vec![
        ToolCandidate {
            name: "generic-shell".into(),
            satisfies_task: true,
            current_assurance: false,
            capability_width: 9,
        },
        ToolCandidate {
            name: "deploy-shadow".into(),
            satisfies_task: true,
            current_assurance: true,
            capability_width: 2,
        },
    ];
    let evaluation = tool_scenario
        .evaluate(RuntimePolicyProfile::Balanced)
        .unwrap();
    let RuntimeDecision::Act(decision) = evaluation.decision else {
        unreachable!()
    };
    assert_eq!(decision.recommended_tool.as_deref(), Some("deploy-shadow"));
}

#[test]
fn runtime_decisions_feedback_replay_gaps_and_policy_versions_are_immutable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let scenario = fixture("unknown-high-risk");
    let original = store
        .record_runtime_decision(
            &scenario.decision_context().unwrap(),
            RuntimePolicyConfig::default(),
        )
        .unwrap();
    assert_eq!(original.decision.kind(), RuntimeDecisionKind::Experiment);
    assert_eq!(
        store.runtime_decision(&original.id).unwrap().context_hash,
        original.context_hash
    );

    let feedback = RuntimeDecisionFeedback {
        decision_id: original.id.clone(),
        outcome: DecisionOutcome::AvoidedFailure,
        evidence: Vec::new(),
        observed_at: original.created_at + Duration::milliseconds(1),
        agent_disagreed: false,
    };
    store.record_runtime_feedback(&feedback).unwrap();
    assert_eq!(
        store.runtime_audit(100).unwrap().outcomes[&DecisionOutcome::AvoidedFailure],
        1
    );
    assert!(!store.runtime_gaps().unwrap().is_empty());
    assert!(
        !store
            .runtime_curriculum_recommendations()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .runtime_development_metrics()
            .unwrap()
            .experiments_per_task,
        Some(1.0)
    );

    let replay = store
        .replay_runtime_decision(
            &original.id,
            RuntimePolicyConfig {
                profile: RuntimePolicyProfile::Conservative,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(replay.decision.kind(), RuntimeDecisionKind::Abstain);
    assert_eq!(store.runtime_decisions().unwrap().len(), 2);

    let connection = rusqlite::Connection::open(temp.path().join("hardknock.db")).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE runtime_decisions SET context_hash='tampered' WHERE id=?1",
                [original.id.to_string()]
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE runtime_policy_versions SET data='{}' WHERE version=?1",
                [original.evaluation.policy_version]
            )
            .is_err()
    );
}

#[test]
fn unnecessary_intervention_feedback_disables_and_lowers_reflex() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut scenario = fixture("reflex");
    let reflex_id: ReflexId = "reflex-00000000-0000-4000-8000-000000000001"
        .parse()
        .unwrap();
    let source_trial = ChaosTrialId::new();
    let now = Utc::now();
    let reflex = Reflex {
        id: reflex_id.clone(),
        version: 1,
        source_lessons: Vec::new(),
        source_trial: source_trial.clone(),
        trigger: TriggerPattern {
            context: ContextSelector {
                repository: None,
                required_markers: Vec::new(),
                tags: Vec::new(),
                os: None,
                arch: None,
            },
            proposed_action: ActionPattern::Custom {
                kind: "reflex".into(),
                value: "fixture".into(),
            },
            repeated_failures: None,
            no_state_change: false,
            config_changed: false,
        },
        response: ReflexResponse::Replan,
        confidence: ConfidenceScore::try_from(0.9).unwrap(),
        status: ReflexStatus::Active,
        evidence: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    let connection = rusqlite::Connection::open(temp.path().join("hardknock.db")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .unwrap();
    connection.execute(
        "INSERT INTO chaos_trials(id,campaign_id,trial_index,experience_id,control_experience_id,reality_id,execution_id,evaluation_id,data) VALUES(?1,?2,0,?3,NULL,?4,?5,?6,'{}')",
        params![
            source_trial.to_string(),
            "chaos-00000000-0000-4000-8000-000000000001",
            "exp-00000000-0000-4000-8000-000000000001",
            "r-00000000-0000-4000-8000-000000000001",
            "exec-00000000-0000-4000-8000-000000000001",
            "eval-00000000-0000-4000-8000-000000000001"
        ],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO reflexes(id,source_trial,version,data) VALUES(?1,?2,1,?3)",
            params![
                reflex_id.to_string(),
                source_trial.to_string(),
                serde_json::to_string(&reflex).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO reflex_versions(reflex_id,version,data) VALUES(?1,1,?2)",
            params![
                reflex_id.to_string(),
                serde_json::to_string(&reflex).unwrap()
            ],
        )
        .unwrap();
    drop(connection);

    scenario.proposed_action = Some(NormalizedAction::Custom {
        kind: "reflex".into(),
        payload: serde_json::json!({}),
    });
    let record = store
        .record_runtime_decision(
            &scenario.decision_context().unwrap(),
            RuntimePolicyConfig::default(),
        )
        .unwrap();
    assert_eq!(record.decision.kind(), RuntimeDecisionKind::Replan);
    store
        .record_runtime_feedback(&RuntimeDecisionFeedback {
            decision_id: record.id,
            outcome: DecisionOutcome::UnnecessaryIntervention,
            evidence: Vec::new(),
            observed_at: record.created_at + Duration::milliseconds(1),
            agent_disagreed: true,
        })
        .unwrap();
    let revised = store.reflex(&reflex_id).unwrap();
    assert_eq!(revised.status, ReflexStatus::Disabled);
    assert_eq!(revised.version, 2);
    assert_eq!(f64::from(revised.confidence), 0.30);
    assert!(store.resilience_tests().unwrap().iter().any(|test| {
        test.reflex_id.as_ref() == Some(&reflex_id)
            && test.status == ResilienceTestStatus::FalsePositive
    }));
}

#[test]
fn control_benchmark_reports_quality_and_fast_path_without_models() {
    let report = run_runtime_benchmark().unwrap();
    assert_eq!(report.scenarios, 60);
    assert_eq!(report.latency.synchronous_llm_calls, 0);
    assert!(report.latency.cached_path.p95_ms < 30.0);
    let adaptive = report
        .arms
        .iter()
        .find(|arm| arm.arm == RuntimeBenchmarkArm::HardknockAdaptiveRuntime)
        .unwrap();
    let baseline = report
        .arms
        .iter()
        .find(|arm| arm.arm == RuntimeBenchmarkArm::AgentOnly)
        .unwrap();
    assert!(adaptive.metrics.task_success_rate > baseline.metrics.task_success_rate);
    assert!(adaptive.metrics.avoided_failure_rate > 0.0);
    assert!(adaptive.metrics.unnecessary_intervention_rate > 0.0);
    assert_eq!(adaptive.metrics.recovery_success_rate, 1.0);
    assert_eq!(adaptive.metrics.abstention_precision, 1.0);
}

#[test]
fn runtime_cli_covers_simulation_audit_replay_feedback_compare_and_run() {
    let fixture = Fixture::new();
    let simulated = fixture.cli(
        &[
            "decision",
            "simulate",
            "--action",
            "printf hello",
            "--risk",
            "medium",
            "--testable",
        ],
        0,
    );
    let id = simulated["result"]["record"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        simulated["result"]["record"]["decision"]["decision"],
        "experiment"
    );
    assert_eq!(
        fixture.cli(&["decision", "show", &id], 0)["result"]["record"]["id"],
        id
    );
    assert_eq!(
        fixture.cli(&["why", "--decision", &id], 0)["result"]["kind"],
        "decision_why"
    );
    fixture.cli(
        &["decision", "feedback", &id, "--outcome", "avoided-failure"],
        0,
    );
    assert_eq!(
        fixture.cli(&["runtime", "audit"], 0)["result"]["audit"]["total"],
        1
    );
    assert!(
        fixture.cli(&["runtime", "gaps"], 0)["result"]["gaps"]
            .as_array()
            .is_some_and(|gaps| !gaps.is_empty())
    );
    let scenario = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/runtime-scenarios/unknown-high-risk.json");
    let compared = fixture.cli(
        &[
            "decision",
            "compare",
            "--scenario",
            scenario.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(
        compared["result"]["comparisons"].as_array().unwrap().len(),
        2
    );
    let replay = fixture.cli(&["decision", "replay", &id, "--policy", "conservative"], 0);
    assert_ne!(replay["result"]["replay"]["id"], id);

    let run = fixture.cli(
        &[
            "run",
            "--runtime-mode",
            "adaptive",
            "--script",
            "true",
            "--check",
            "true",
            "runtime controlled run",
        ],
        0,
    );
    assert_eq!(run["runtime_decision"]["decision"]["decision"], "act");
    fixture.assert_source_unchanged();
}
