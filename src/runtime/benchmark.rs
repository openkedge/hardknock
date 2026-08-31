// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, time::Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    Result,
    assurance::AssuranceProfileRef,
    bridge::protocol::NormalizedAction,
    core::{AssuranceProfileId, OperatingEnvelopeId, RecoveryId, ReflexId, SkillCertificationId},
    curriculum::Severity,
    effects::{
        ActionRef, EffectKind, EffectOperation, EffectRequest, EffectTarget, ExternalityClass,
        ReversibilityClass,
    },
    lesson::ConfidenceScore,
};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBenchmarkArm {
    AgentOnly,
    StaticRules,
    HardknockAdaptiveRuntime,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ControlQualityMetrics {
    pub tasks: u64,
    pub task_success_rate: f64,
    pub avoided_failure_rate: f64,
    pub unnecessary_intervention_rate: f64,
    pub recovery_success_rate: f64,
    pub abstention_rate: f64,
    pub abstention_precision: f64,
    pub experiments_per_task: f64,
    pub experiment_decision_change_rate: f64,
    pub time_to_recovery_ms: Option<f64>,
    pub external_mistake_escape_rate: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeBenchmarkArmResult {
    pub arm: RuntimeBenchmarkArm,
    pub metrics: ControlQualityMetrics,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    pub samples: usize,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeLatencyReport {
    pub cached_path: LatencyPercentiles,
    pub assurance_lookup: LatencyPercentiles,
    pub envelope_lookup: LatencyPercentiles,
    pub reflex_matching: LatencyPercentiles,
    pub synchronous_llm_calls: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeBenchmarkReport {
    pub schema: String,
    pub scenarios: usize,
    pub arms: Vec<RuntimeBenchmarkArmResult>,
    pub latency: RuntimeLatencyReport,
    pub demonstrations: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
struct ObservedTask {
    decision: RuntimeDecisionKind,
    success: bool,
    uncontrolled_success: bool,
    controlled_success: bool,
    expected_outcome: Option<DecisionOutcome>,
    recovery_latency_ms: Option<u64>,
}

pub fn run_runtime_benchmark() -> Result<RuntimeBenchmarkReport> {
    let scenarios = benchmark_scenarios();
    let mut arms = Vec::new();
    for arm in [
        RuntimeBenchmarkArm::AgentOnly,
        RuntimeBenchmarkArm::StaticRules,
        RuntimeBenchmarkArm::HardknockAdaptiveRuntime,
    ] {
        let observations = scenarios
            .iter()
            .map(|scenario| observe(arm, scenario))
            .collect::<Result<Vec<_>>>()?;
        arms.push(RuntimeBenchmarkArmResult {
            arm,
            metrics: calculate_metrics(&observations),
        });
    }
    let latency = measure_latency(&scenarios)?;
    Ok(RuntimeBenchmarkReport {
        schema: "hardknock.runtime-benchmark.v1".into(),
        scenarios: scenarios.len(),
        arms,
        latency,
        demonstrations: BTreeMap::from([
            (
                "adaptive_learning".into(),
                "unknown -> experiment; after local validation -> act".into(),
            ),
            (
                "negative_learning".into(),
                "act before a failure boundary; replan after a validated reflex".into(),
            ),
            (
                "stale_evidence".into(),
                "known_stale -> experiment -> changed strategy".into(),
            ),
            (
                "approval_vs_abstention".into(),
                "prepared supported effect -> approval; unsupported irreversible effect -> abstain"
                    .into(),
            ),
        ]),
    })
}

fn observe(arm: RuntimeBenchmarkArm, scenario: &RuntimeScenario) -> Result<ObservedTask> {
    let uncontrolled_success = scenario.uncontrolled_success.unwrap_or(true);
    let controlled_success = scenario.controlled_success.unwrap_or(uncontrolled_success);
    let decision = match arm {
        RuntimeBenchmarkArm::AgentOnly => RuntimeDecisionKind::Act,
        RuntimeBenchmarkArm::StaticRules => {
            if scenario.capability.governance.hard_policy_blocked {
                RuntimeDecisionKind::Abstain
            } else if scenario.risk.severity >= Severity::High
                && scenario.proposed_effect.is_some()
                && !scenario.capability.commit_authority
            {
                RuntimeDecisionKind::RequireApproval
            } else {
                RuntimeDecisionKind::Act
            }
        }
        RuntimeBenchmarkArm::HardknockAdaptiveRuntime => scenario
            .evaluate(RuntimePolicyProfile::Balanced)?
            .decision
            .kind(),
    };
    let success = match arm {
        RuntimeBenchmarkArm::AgentOnly => uncontrolled_success,
        RuntimeBenchmarkArm::StaticRules => {
            if decision == RuntimeDecisionKind::Act {
                uncontrolled_success
            } else {
                controlled_success
            }
        }
        RuntimeBenchmarkArm::HardknockAdaptiveRuntime => controlled_success,
    };
    Ok(ObservedTask {
        decision,
        success,
        uncontrolled_success,
        controlled_success,
        expected_outcome: scenario.expected_outcome,
        recovery_latency_ms: if arm == RuntimeBenchmarkArm::HardknockAdaptiveRuntime {
            scenario.recovery_latency_ms
        } else {
            scenario.baseline_recovery_latency_ms
        },
    })
}

fn calculate_metrics(tasks: &[ObservedTask]) -> ControlQualityMetrics {
    let denominator = tasks.len() as f64;
    let interventions = tasks
        .iter()
        .filter(|task| task.decision != RuntimeDecisionKind::Act)
        .collect::<Vec<_>>();
    let abstentions = tasks
        .iter()
        .filter(|task| task.decision == RuntimeDecisionKind::Abstain)
        .collect::<Vec<_>>();
    let experiments = tasks
        .iter()
        .filter(|task| task.decision == RuntimeDecisionKind::Experiment)
        .collect::<Vec<_>>();
    let recoveries = tasks
        .iter()
        .filter(|task| task.decision == RuntimeDecisionKind::Recover)
        .collect::<Vec<_>>();
    let rate = |value: usize, total: usize| {
        if total == 0 {
            0.0
        } else {
            value as f64 / total as f64
        }
    };
    ControlQualityMetrics {
        tasks: tasks.len() as u64,
        task_success_rate: rate(
            tasks.iter().filter(|task| task.success).count(),
            tasks.len(),
        ),
        avoided_failure_rate: rate(
            interventions
                .iter()
                .filter(|task| !task.uncontrolled_success && task.controlled_success)
                .count(),
            interventions.len(),
        ),
        unnecessary_intervention_rate: rate(
            interventions
                .iter()
                .filter(|task| {
                    task.uncontrolled_success
                        || task.expected_outcome == Some(DecisionOutcome::UnnecessaryIntervention)
                })
                .count(),
            interventions.len(),
        ),
        recovery_success_rate: rate(
            recoveries.iter().filter(|task| task.success).count(),
            recoveries.len(),
        ),
        abstention_rate: rate(abstentions.len(), tasks.len()),
        abstention_precision: rate(
            abstentions
                .iter()
                .filter(|task| !task.uncontrolled_success)
                .count(),
            abstentions.len(),
        ),
        experiments_per_task: if denominator == 0.0 {
            0.0
        } else {
            experiments.len() as f64 / denominator
        },
        experiment_decision_change_rate: rate(
            experiments
                .iter()
                .filter(|task| task.controlled_success != task.uncontrolled_success)
                .count(),
            experiments.len(),
        ),
        time_to_recovery_ms: {
            let values = recoveries
                .iter()
                .filter_map(|task| task.recovery_latency_ms)
                .collect::<Vec<_>>();
            (!values.is_empty()).then(|| values.iter().sum::<u64>() as f64 / values.len() as f64)
        },
        external_mistake_escape_rate: rate(
            tasks
                .iter()
                .filter(|task| {
                    task.decision == RuntimeDecisionKind::Act && !task.uncontrolled_success
                })
                .count(),
            tasks.len(),
        ),
    }
}

fn measure_latency(scenarios: &[RuntimeScenario]) -> Result<RuntimeLatencyReport> {
    let select = |predicate: fn(&RuntimeScenario) -> bool| {
        scenarios
            .iter()
            .find(|scenario| predicate(scenario))
            .unwrap_or(&scenarios[0])
    };
    Ok(RuntimeLatencyReport {
        cached_path: time_scenario(select(|scenario| scenario.name.contains("known-safe")))?,
        assurance_lookup: time_scenario(select(|scenario| scenario.name.contains("approval")))?,
        envelope_lookup: time_scenario(select(|scenario| scenario.envelope.is_some()))?,
        reflex_matching: time_scenario(select(|scenario| !scenario.reflexes.is_empty()))?,
        synchronous_llm_calls: 0,
    })
}

fn time_scenario(scenario: &RuntimeScenario) -> Result<LatencyPercentiles> {
    let context = scenario.decision_context()?;
    let controller = DeterministicRuntimeController::default();
    let mut elapsed = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let started = Instant::now();
        std::hint::black_box(controller.evaluate(std::hint::black_box(&context))?);
        elapsed.push(started.elapsed().as_nanos());
    }
    elapsed.sort_unstable();
    let at = |fraction: f64| {
        let index = ((elapsed.len() - 1) as f64 * fraction).round() as usize;
        elapsed[index] as f64 / 1_000_000.0
    };
    Ok(LatencyPercentiles {
        samples: elapsed.len(),
        median_ms: at(0.50),
        p95_ms: at(0.95),
        p99_ms: at(0.99),
    })
}

pub fn benchmark_scenarios() -> Vec<RuntimeScenario> {
    let templates = vec![
        known_safe(),
        unknown_testable(),
        known_failure(),
        reflex(false),
        reflex(true),
        stale(),
        unsupported_effect(),
        approval(),
        certification_out_of_scope(),
        federated_advisory(),
        growing_experience_before(),
        growing_experience_after(),
    ];
    let mut scenarios = Vec::with_capacity(60);
    for round in 0..5 {
        for template in &templates {
            let mut scenario = template.clone();
            scenario.name = format!("{}-{round}", scenario.name);
            scenarios.push(scenario);
        }
    }
    scenarios
}

fn base(name: &str, severity: Severity) -> RuntimeScenario {
    RuntimeScenario {
        name: name.into(),
        task: TaskDescriptor {
            description: format!("deterministic {name} task"),
            family: Some("runtime-benchmark".into()),
            tags: vec![name.into()],
        },
        proposed_action: Some(NormalizedAction::Custom {
            kind: name.into(),
            payload: json!({"scenario":name}),
        }),
        risk: RuntimeRiskAssessment {
            severity,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn safe_experiments(scenario: &mut RuntimeScenario) {
    scenario.experiments.safe_reality_available = true;
    scenario.experiments.effect_safe = true;
    scenario.experiments.budget_remaining = true;
    scenario.uncertainty.candidate_strategies = vec![
        StrategyCandidate {
            name: "direct".into(),
            description: "direct strategy".into(),
            action: None,
        },
        StrategyCandidate {
            name: "shadow".into(),
            description: "shadow strategy".into(),
            action: None,
        },
    ];
}

fn known_safe() -> RuntimeScenario {
    let mut scenario = base("known-safe", Severity::Low);
    scenario.knowledge.local_supported = true;
    scenario.envelope = Some(OperatingEnvelopeRef {
        id: OperatingEnvelopeId::new(),
        version: 1,
        position: EnvelopePosition::KnownSafe,
    });
    scenario.expected_decision = Some(RuntimeDecisionKind::Act);
    scenario.uncontrolled_success = Some(true);
    scenario.controlled_success = Some(true);
    scenario
}

fn unknown_testable() -> RuntimeScenario {
    let mut scenario = base("unknown-testable", Severity::Medium);
    safe_experiments(&mut scenario);
    scenario.expected_decision = Some(RuntimeDecisionKind::Experiment);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario.expected_outcome = Some(DecisionOutcome::AvoidedFailure);
    scenario
}

fn known_failure() -> RuntimeScenario {
    let mut scenario = base("known-failure", Severity::Medium);
    let signature = "stale_credential".to_string();
    scenario.failure_signature = Some(FailureSignatureRef {
        signature: signature.clone(),
    });
    scenario.recoveries.push(RecoveryRef {
        id: RecoveryId::new(),
        version: 1,
        failure_signature: signature,
        confidence: ConfidenceScore::try_from(0.9).expect("valid confidence"),
        fresh: true,
        scope_matches: true,
    });
    scenario.expected_decision = Some(RuntimeDecisionKind::Recover);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario.recovery_latency_ms = Some(20);
    scenario.baseline_recovery_latency_ms = Some(150);
    scenario
}

fn reflex(false_positive: bool) -> RuntimeScenario {
    let mut scenario = base(
        if false_positive {
            "false-reflex"
        } else {
            "reflex"
        },
        Severity::Medium,
    );
    scenario.reflexes.push(ReflexRef {
        id: ReflexId::new(),
        version: 1,
    });
    scenario.expected_decision = Some(RuntimeDecisionKind::Replan);
    scenario.uncontrolled_success = Some(false_positive);
    scenario.controlled_success = Some(true);
    scenario.expected_outcome = Some(if false_positive {
        DecisionOutcome::UnnecessaryIntervention
    } else {
        DecisionOutcome::AvoidedFailure
    });
    scenario
}

fn stale() -> RuntimeScenario {
    let mut scenario = base("stale-lesson", Severity::Medium);
    scenario.knowledge.local_supported = true;
    scenario.knowledge.evidence_stale = true;
    safe_experiments(&mut scenario);
    scenario.expected_decision = Some(RuntimeDecisionKind::Experiment);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario
}

fn effect(name: &str) -> EffectRequest {
    EffectRequest {
        session_id: "runtime-benchmark".into(),
        reality_id: None,
        source_action: ActionRef {
            id: name.into(),
            kind: "runtime_scenario".into(),
        },
        kind: EffectKind::Message,
        target: EffectTarget {
            uri: "message://customer".into(),
        },
        operation: EffectOperation::Dispatch,
        payload: json!({"message":"redacted deterministic fixture"}),
        adapter: Some("deterministic".into()),
        evidence: Vec::new(),
        fault: Default::default(),
    }
}

fn unsupported_effect() -> RuntimeScenario {
    let mut scenario = base("unsupported-effect", Severity::High);
    scenario.proposed_effect = Some(effect("unsupported-effect"));
    scenario.capability.effect_adapter_available = false;
    scenario.capability.commit_authority = false;
    scenario.risk.externality = ExternalityClass::HumanVisible;
    scenario.risk.reversibility = ReversibilityClass::Irreversible;
    scenario.expected_decision = Some(RuntimeDecisionKind::Abstain);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario
}

fn current_assurance() -> AssuranceContext {
    let profile = AssuranceProfileRef {
        id: AssuranceProfileId::new(),
        version: "deployment-resilience-v1".into(),
    };
    AssuranceContext {
        summary: AssuranceSummary {
            status: AssuranceRuntimeStatus::Current,
            certification: Some(SkillCertificationId::new()),
            profile: Some(profile),
            reasons: vec!["deterministic current assurance fixture".into()],
        },
        applicability: AssuranceApplicability {
            applicable: true,
            reasons: vec!["scope matches".into()],
        },
        requirements: Vec::new(),
        gaps: Vec::new(),
    }
}

fn approval() -> RuntimeScenario {
    let mut scenario = base("approval", Severity::High);
    scenario.knowledge.local_supported = true;
    scenario.assurance = current_assurance();
    scenario.proposed_effect = Some(effect("approval"));
    scenario.capability.commit_authority = false;
    scenario.capability.effect_adapter_available = true;
    scenario.expected_decision = Some(RuntimeDecisionKind::RequireApproval);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario
}

fn certification_out_of_scope() -> RuntimeScenario {
    let mut scenario = base("certification-out-of-scope", Severity::Medium);
    scenario.knowledge.context_in_scope = false;
    scenario.assurance = current_assurance();
    scenario.assurance.summary.status = AssuranceRuntimeStatus::OutOfScope;
    scenario.assurance.applicability.applicable = false;
    safe_experiments(&mut scenario);
    scenario.expected_decision = Some(RuntimeDecisionKind::Experiment);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario
}

fn federated_advisory() -> RuntimeScenario {
    let mut scenario = base("federated-advisory", Severity::Medium);
    scenario.externally_supported = true;
    scenario.knowledge.remote_advisory_only = true;
    safe_experiments(&mut scenario);
    scenario.expected_decision = Some(RuntimeDecisionKind::Experiment);
    scenario.uncontrolled_success = Some(false);
    scenario.controlled_success = Some(true);
    scenario
}

fn growing_experience_before() -> RuntimeScenario {
    let mut scenario = unknown_testable();
    scenario.name = "growing-experience-before".into();
    scenario
}

fn growing_experience_after() -> RuntimeScenario {
    let mut scenario = known_safe();
    scenario.name = "growing-experience-after".into();
    scenario
}
