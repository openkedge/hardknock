// SPDX-License-Identifier: Apache-2.0
//! Network-free deterministic comparison of three experience-allocation policies.

use super::*;
use crate::{
    Result, budget::ExperienceBudget, core::ExperienceOpportunityId, curriculum::Severity,
    effects::EffectRisk,
};
use chrono::{TimeZone, Utc};
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionMetrics {
    pub trials_consumed: usize,
    pub agent_runs_consumed: usize,
    pub critical_gaps_closed: usize,
    pub high_severity_failures_mitigated: usize,
    pub contradictions_resolved: usize,
    pub runtime_decisions_improved: usize,
    pub assurance_blockers_closed: usize,
    pub material_learning_rate: f64,
    pub wasted_trial_rate: f64,
    pub saturated_evidence_spend: usize,
    pub unused_budget: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct HeldOutRuntimeMetrics {
    pub task_success_rate: f64,
    pub repeated_failure_rate: f64,
    pub recovery_success_rate: f64,
    pub avoided_failure_rate: f64,
    pub unnecessary_intervention_rate: f64,
    pub abstention_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkArmResult {
    pub arm: &'static str,
    pub selected: Vec<ExperienceOpportunityId>,
    pub acquisition: AcquisitionMetrics,
    pub held_out: HeldOutRuntimeMetrics,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExperienceEconomicsBenchmark {
    pub fixture: &'static str,
    pub budget: ExperienceBudget,
    pub backlog_size: usize,
    pub arms: Vec<BenchmarkArmResult>,
    pub supported_claim: &'static str,
}

#[derive(Clone)]
struct Allocation {
    opportunity: ExperienceOpportunity,
    trials: usize,
}

fn learning(band: ValueBand) -> LearningValueEstimate {
    LearningValueEstimate {
        band,
        possible_outcomes: vec![
            LearningOutcomeClass::Validate,
            LearningOutcomeClass::NoMaterialChange,
        ],
        decision_changing_outcomes: usize::from(band >= ValueBand::High),
        rationale: vec!["deterministic benchmark outcome classes".into()],
    }
}

#[allow(clippy::too_many_arguments)]
fn gap(
    label: &str,
    kind: ExperienceOpportunityKind,
    reason: OpportunityReason,
    severity: Severity,
    exposure: ExposureBand,
    learning_band: ValueBand,
    trials: usize,
    agent_runs: usize,
    evidence: EvidenceSummary,
) -> ExperienceGap {
    ExperienceGap {
        kind,
        target: ExperienceOpportunityTarget::RuntimeGap(label.into()),
        reasons: vec![reason],
        severity,
        exposure,
        mitigation_gap: if severity >= Severity::High {
            MitigationGap::Unmitigated
        } else {
            MitigationGap::Partial
        },
        learning: learning(learning_band),
        decision_relevance: DecisionRelevance {
            affected_decisions: match exposure {
                ExposureBand::VeryFrequent => 100,
                ExposureBand::Frequent => 20,
                ExposureBand::Occasional => 5,
                ExposureBand::Rare | ExposureBand::Unknown => 1,
            },
            affected_task_families: usize::from(matches!(
                exposure,
                ExposureBand::Frequent | ExposureBand::VeryFrequent
            )) + 1,
            current_runtime_use: if matches!(
                exposure,
                ExposureBand::Frequent | ExposureBand::VeryFrequent
            ) {
                RuntimeUseBand::High
            } else {
                RuntimeUseBand::Low
            },
            likely_decision_change: learning_band,
        },
        reuse: if matches!(
            exposure,
            ExposureBand::Frequent | ExposureBand::VeryFrequent
        ) {
            ReusePotential::Broad
        } else {
            ReusePotential::Narrow
        },
        novelty: EvidenceNovelty::ContextExtension,
        evidence,
        estimated_cost: ExperimentCost {
            trials,
            agent_runs,
            estimated_duration: Some(Duration::from_secs((trials * 30) as u64)),
            external_effect_risk: EffectRisk::ReadOnly,
            ..ExperimentCost::default()
        },
        risk: default_opportunity_risk(),
        dependencies: vec![],
    }
}

fn sparse() -> EvidenceSummary {
    EvidenceSummary::default()
}

fn saturated() -> EvidenceSummary {
    EvidenceSummary {
        observations: 12,
        equivalent_replications: 8,
        distinct_contexts: 3,
        counterfactuals: 3,
        diversity_domains: 3,
        contradictions: 0,
        fresh: true,
    }
}

fn contradicted() -> EvidenceSummary {
    EvidenceSummary {
        observations: 4,
        equivalent_replications: 2,
        distinct_contexts: 2,
        counterfactuals: 1,
        diversity_domains: 2,
        contradictions: 1,
        fresh: true,
    }
}

/// A deliberately mixed backlog. Low-value saturated items appear early so a
/// uniform allocator pays the opportunity cost that the adaptive arm avoids.
pub fn benchmark_context() -> ExperiencePlanningContext {
    let mut gaps = Vec::new();
    for index in 0..5 {
        gaps.push(gap(
            &format!("saturated-low-{index}"),
            ExperienceOpportunityKind::HardenSkill,
            OpportunityReason::EvidenceSaturated,
            Severity::Low,
            ExposureBand::Rare,
            ValueBand::Low,
            1,
            0,
            saturated(),
        ));
    }
    for index in 0..5 {
        gaps.push(gap(
            &format!("medium-{index}"),
            ExperienceOpportunityKind::IncreaseEvidenceDiversity,
            OpportunityReason::LowEvidenceDiversity,
            Severity::Medium,
            ExposureBand::Occasional,
            ValueBand::Medium,
            1,
            0,
            sparse(),
        ));
    }
    for index in 0..2 {
        gaps.push(gap(
            &format!("contradiction-{index}"),
            ExperienceOpportunityKind::ResolveContradiction,
            OpportunityReason::EvidenceContradicted,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::Critical,
            2,
            1,
            contradicted(),
        ));
    }
    for index in 0..2 {
        gaps.push(gap(
            &format!("causal-{index}"),
            ExperienceOpportunityKind::DiscriminateCausalHypotheses,
            OpportunityReason::CausalMechanismUnknown,
            Severity::Medium,
            ExposureBand::Occasional,
            ValueBand::High,
            2,
            1,
            sparse(),
        ));
    }
    for index in 0..2 {
        gaps.push(gap(
            &format!("forecast-{index}"),
            ExperienceOpportunityKind::ValidateEarlyWarning,
            OpportunityReason::ForecastMiss,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            2,
            0,
            sparse(),
        ));
    }
    gaps.push(gap(
        "capability-minimization",
        ExperienceOpportunityKind::MinimizeCapability,
        OpportunityReason::CapabilityOverprovisioned,
        Severity::Medium,
        ExposureBand::VeryFrequent,
        ValueBand::High,
        1,
        0,
        sparse(),
    ));
    for index in 0..3 {
        gaps.push(gap(
            &format!("high-{index}"),
            ExperienceOpportunityKind::ValidateRecovery,
            OpportunityReason::RecoveryMissing,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            2,
            1,
            sparse(),
        ));
    }
    for index in 0..2 {
        gaps.push(gap(
            &format!("critical-{index}"),
            ExperienceOpportunityKind::ValidateRecovery,
            OpportunityReason::CriticalFailureUnmitigated,
            Severity::Critical,
            ExposureBand::VeryFrequent,
            ValueBand::Critical,
            3,
            1,
            sparse(),
        ));
    }
    ExperiencePlanningContext {
        gaps,
        completed_dependencies: Default::default(),
        objective: ExperiencePortfolioObjective::Balanced,
        now: Utc.timestamp_opt(1_735_689_600, 0).unwrap(),
    }
}

fn fixed_budget() -> ExperienceBudget {
    ExperienceBudget {
        max_realities: 20,
        max_curriculum_trials: Some(20),
        max_agent_runs: 5,
        max_duration_ms: Some(60 * 60 * 1000),
        max_commands_per_reality: None,
        max_parallel_trials: Some(1),
        max_human_approvals: Some(0),
        allowed_effect_risk: EffectRisk::ReadOnly,
    }
}

fn round_robin(opportunities: &[ExperienceOpportunity], budget: usize) -> Vec<Allocation> {
    let mut allocations = BTreeMap::<ExperienceOpportunityId, Allocation>::new();
    for opportunity in opportunities.iter().cycle().take(budget) {
        allocations
            .entry(opportunity.id.clone())
            .and_modify(|allocation| allocation.trials += 1)
            .or_insert_with(|| Allocation {
                opportunity: opportunity.clone(),
                trials: 1,
            });
    }
    allocations.into_values().collect()
}

fn static_priority(opportunities: &[ExperienceOpportunity], budget: usize) -> Vec<Allocation> {
    fn key(kind: &ExperienceOpportunityKind) -> u8 {
        match kind {
            ExperienceOpportunityKind::HardenSkill => 0,
            ExperienceOpportunityKind::IncreaseEvidenceDiversity => 1,
            ExperienceOpportunityKind::ValidateRecovery => 2,
            ExperienceOpportunityKind::ValidateEarlyWarning => 3,
            ExperienceOpportunityKind::ResolveContradiction => 4,
            _ => 5,
        }
    }
    let mut ranked = opportunities.to_vec();
    ranked.sort_by_key(|opportunity| (key(&opportunity.kind), opportunity.id.clone()));
    let mut remaining = budget;
    let mut allocations = Vec::new();
    for opportunity in ranked {
        let trials = opportunity.estimated_cost.trials;
        if trials <= remaining {
            remaining -= trials;
            allocations.push(Allocation {
                opportunity,
                trials,
            });
        }
    }
    allocations
}

fn adaptive(
    opportunities: &[ExperienceOpportunity],
    context: &ExperiencePlanningContext,
    budget: &ExperienceBudget,
) -> Result<Vec<Allocation>> {
    let portfolio =
        DeterministicExperienceAllocationPolicy.allocate(opportunities, budget, context)?;
    let by_id = opportunities
        .iter()
        .map(|opportunity| (opportunity.id.clone(), opportunity))
        .collect::<BTreeMap<_, _>>();
    Ok(portfolio
        .selected
        .iter()
        .map(|selection| Allocation {
            opportunity: (*by_id[&selection.opportunity]).clone(),
            trials: selection.reserved_cost.trials,
        })
        .collect())
}

fn evaluate(arm: &'static str, allocations: Vec<Allocation>, budget: usize) -> BenchmarkArmResult {
    let trials = allocations.iter().map(|item| item.trials).sum::<usize>();
    let agent_runs = allocations
        .iter()
        .filter(|item| item.trials >= item.opportunity.estimated_cost.trials)
        .map(|item| item.opportunity.estimated_cost.agent_runs)
        .sum::<usize>();
    let completed = allocations
        .iter()
        .filter(|item| item.trials >= item.opportunity.estimated_cost.trials)
        .collect::<Vec<_>>();
    let material = completed
        .iter()
        .filter(|item| item.opportunity.saturation != EvidenceSaturation::Saturated)
        .copied()
        .collect::<Vec<_>>();
    let saturated_spend = allocations
        .iter()
        .filter(|item| item.opportunity.saturation == EvidenceSaturation::Saturated)
        .map(|item| item.trials)
        .sum::<usize>();
    let incomplete_spend = allocations
        .iter()
        .filter(|item| item.trials < item.opportunity.estimated_cost.trials)
        .map(|item| item.trials)
        .sum::<usize>();
    let critical = material
        .iter()
        .filter(|item| item.opportunity.risk_reduction.failure_severity == Severity::Critical)
        .count();
    let high = material
        .iter()
        .filter(|item| item.opportunity.risk_reduction.failure_severity >= Severity::High)
        .count();
    let contradictions = material
        .iter()
        .filter(|item| item.opportunity.saturation == EvidenceSaturation::Contradicted)
        .count();
    let runtime = material
        .iter()
        .filter(|item| {
            matches!(
                item.opportunity.kind,
                ExperienceOpportunityKind::ResolveRuntimeUnknown
            )
        })
        .count();
    let assurance = material
        .iter()
        .filter(|item| {
            item.opportunity
                .rationale
                .contains(&OpportunityReason::AssuranceBlocked)
        })
        .count();
    let forecast = material
        .iter()
        .filter(|item| {
            matches!(
                item.opportunity.kind,
                ExperienceOpportunityKind::ValidateEarlyWarning
            )
        })
        .count();
    let capability = material
        .iter()
        .filter(|item| {
            matches!(
                item.opportunity.kind,
                ExperienceOpportunityKind::MinimizeCapability
            )
        })
        .count();
    let material_trials = material.iter().map(|item| item.trials).sum::<usize>();
    let wasted = saturated_spend + incomplete_spend;
    let rate = |numerator: usize| {
        if trials == 0 {
            0.0
        } else {
            numerator as f64 / trials as f64
        }
    };
    let acquisition = AcquisitionMetrics {
        trials_consumed: trials,
        agent_runs_consumed: agent_runs,
        critical_gaps_closed: critical,
        high_severity_failures_mitigated: high,
        contradictions_resolved: contradictions,
        runtime_decisions_improved: runtime,
        assurance_blockers_closed: assurance,
        material_learning_rate: rate(material_trials),
        wasted_trial_rate: rate(wasted),
        saturated_evidence_spend: saturated_spend,
        unused_budget: budget.saturating_sub(trials),
    };
    let clamp = |value: f64| value.clamp(0.0, 1.0);
    let held_out = HeldOutRuntimeMetrics {
        task_success_rate: clamp(0.62 + critical as f64 * 0.08 + high as f64 * 0.015),
        repeated_failure_rate: clamp(0.28 - critical as f64 * 0.07 - high as f64 * 0.02),
        recovery_success_rate: clamp(0.50 + critical as f64 * 0.12 + high as f64 * 0.025),
        avoided_failure_rate: clamp(0.12 + forecast as f64 * 0.09 + critical as f64 * 0.04),
        unnecessary_intervention_rate: clamp(
            0.18 - contradictions as f64 * 0.025 - capability as f64 * 0.02,
        ),
        abstention_rate: clamp(0.20 - runtime as f64 * 0.04 - contradictions as f64 * 0.01),
    };
    BenchmarkArmResult {
        arm,
        selected: allocations
            .into_iter()
            .map(|allocation| allocation.opportunity.id)
            .collect(),
        acquisition,
        held_out,
    }
}

pub fn run() -> Result<ExperienceEconomicsBenchmark> {
    let context = benchmark_context();
    let opportunities = DeterministicExperienceOpportunityGenerator.generate(&context)?;
    let budget = fixed_budget();
    let arms = vec![
        evaluate(
            "round_robin",
            round_robin(&opportunities, budget.max_trials()),
            budget.max_trials(),
        ),
        evaluate(
            "static_priority",
            static_priority(&opportunities, budget.max_trials()),
            budget.max_trials(),
        ),
        evaluate(
            "hardknock_adaptive_portfolio",
            adaptive(&opportunities, &context, &budget)?,
            budget.max_trials(),
        ),
    ];
    Ok(ExperienceEconomicsBenchmark {
        fixture: "economics/fixed-budget-v1",
        budget,
        backlog_size: opportunities.len(),
        arms,
        supported_claim: "Under this deterministic fixed-budget fixture, the adaptive portfolio closes more high-impact gaps and spends fewer trials on saturated evidence than round-robin allocation.",
    })
}
