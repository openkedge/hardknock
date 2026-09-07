// SPDX-License-Identifier: Apache-2.0

use chrono::{TimeZone, Utc};
use hardknock::{
    budget::ExperienceBudget,
    core::ExperienceOpportunityId,
    curriculum::{Severity, TrialSafety},
    economics::{self, *},
    effects::EffectRisk,
    store::Store,
};
use std::{collections::BTreeSet, path::Path, time::Duration};

fn budget(trials: usize) -> ExperienceBudget {
    ExperienceBudget {
        max_realities: trials,
        max_curriculum_trials: Some(trials),
        max_agent_runs: trials,
        max_duration_ms: Some(60_000),
        max_commands_per_reality: None,
        max_parallel_trials: Some(1),
        max_human_approvals: Some(0),
        allowed_effect_risk: EffectRisk::ReadOnly,
    }
}

fn evidence(
    observations: usize,
    replications: usize,
    contexts: usize,
    counterfactuals: usize,
    diversity: usize,
    contradictions: usize,
) -> EvidenceSummary {
    EvidenceSummary {
        observations,
        equivalent_replications: replications,
        distinct_contexts: contexts,
        counterfactuals,
        diversity_domains: diversity,
        contradictions,
        fresh: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn gap(
    name: &str,
    kind: ExperienceOpportunityKind,
    reason: OpportunityReason,
    severity: Severity,
    exposure: ExposureBand,
    learning: ValueBand,
    reuse: ReusePotential,
    novelty: EvidenceNovelty,
    trials: usize,
    observations: EvidenceSummary,
) -> ExperienceGap {
    ExperienceGap {
        kind,
        target: ExperienceOpportunityTarget::RuntimeGap(name.into()),
        reasons: vec![reason],
        severity,
        exposure,
        mitigation_gap: if severity >= Severity::High {
            MitigationGap::Unmitigated
        } else {
            MitigationGap::Partial
        },
        learning: LearningValueEstimate {
            band: learning,
            possible_outcomes: vec![
                LearningOutcomeClass::Validate,
                LearningOutcomeClass::NoMaterialChange,
            ],
            decision_changing_outcomes: usize::from(learning >= ValueBand::High),
            rationale: vec!["test fixture".into()],
        },
        decision_relevance: DecisionRelevance {
            affected_decisions: match exposure {
                ExposureBand::VeryFrequent => 100,
                ExposureBand::Frequent => 20,
                ExposureBand::Occasional => 5,
                ExposureBand::Rare | ExposureBand::Unknown => 1,
            },
            affected_task_families: match reuse {
                ReusePotential::Broad => 5,
                ReusePotential::Reusable => 2,
                _ => 1,
            },
            current_runtime_use: RuntimeUseBand::Medium,
            likely_decision_change: learning,
        },
        reuse,
        novelty,
        evidence: observations,
        estimated_cost: ExperimentCost {
            trials,
            estimated_duration: Some(Duration::from_secs(trials as u64)),
            external_effect_risk: EffectRisk::ReadOnly,
            ..Default::default()
        },
        risk: OpportunityRisk {
            trial_safety: TrialSafety::Safe,
            ..default_opportunity_risk()
        },
        dependencies: vec![],
    }
}

fn context(gaps: Vec<ExperienceGap>) -> ExperiencePlanningContext {
    ExperiencePlanningContext {
        gaps,
        completed_dependencies: BTreeSet::new(),
        objective: ExperiencePortfolioObjective::Balanced,
        now: Utc.timestamp_opt(1_735_689_600, 0).unwrap(),
    }
}

fn generate(context: &ExperiencePlanningContext) -> Vec<ExperienceOpportunity> {
    DeterministicExperienceOpportunityGenerator
        .generate(context)
        .unwrap()
}

fn selected_target(
    portfolio: &ExperiencePortfolio,
    opportunities: &[ExperienceOpportunity],
) -> ExperienceOpportunityTarget {
    let id = &portfolio.selected[0].opportunity;
    opportunities
        .iter()
        .find(|item| &item.id == id)
        .unwrap()
        .target
        .clone()
}

#[test]
fn opportunity_generation_and_portfolio_order_are_deterministic() {
    let context = context(vec![
        gap(
            "b",
            ExperienceOpportunityKind::ValidateRecovery,
            OpportunityReason::RecoveryMissing,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::ContextExtension,
            2,
            EvidenceSummary::default(),
        ),
        gap(
            "a",
            ExperienceOpportunityKind::ValidateLesson,
            OpportunityReason::EvidenceStale,
            Severity::Medium,
            ExposureBand::Occasional,
            ValueBand::Medium,
            ReusePotential::Reusable,
            EvidenceNovelty::Replication,
            1,
            EvidenceSummary::default(),
        ),
    ]);
    let first = generate(&context);
    let second = generate(&context);
    assert_eq!(
        first.iter().map(|item| &item.id).collect::<Vec<_>>(),
        second.iter().map(|item| &item.id).collect::<Vec<_>>()
    );
    let policy = DeterministicExperienceAllocationPolicy;
    let a = policy.allocate(&first, &budget(3), &context).unwrap();
    let b = policy.allocate(&second, &budget(3), &context).unwrap();
    assert_eq!(
        a.selected
            .iter()
            .map(|item| &item.opportunity)
            .collect::<Vec<_>>(),
        b.selected
            .iter()
            .map(|item| &item.opportunity)
            .collect::<Vec<_>>()
    );
}

#[test]
fn saturation_prioritizes_counterfactual_gap_and_spends_nothing_on_saturated_work() {
    let context = context(vec![
        gap(
            "twelve-equivalent-passes",
            ExperienceOpportunityKind::HardenSkill,
            OpportunityReason::EvidenceSaturated,
            Severity::Low,
            ExposureBand::Rare,
            ValueBand::Low,
            ReusePotential::Narrow,
            EvidenceNovelty::Duplicate,
            1,
            evidence(12, 12, 3, 3, 3, 0),
        ),
        gap(
            "missing-counterfactual",
            ExperienceOpportunityKind::ChallengeCausalHypothesis,
            OpportunityReason::CausalMechanismUnknown,
            Severity::Medium,
            ExposureBand::Occasional,
            ValueBand::High,
            ReusePotential::Reusable,
            EvidenceNovelty::MechanismChallenge,
            1,
            evidence(1, 1, 1, 0, 1, 0),
        ),
    ]);
    let opportunities = generate(&context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &context)
        .unwrap();
    assert_eq!(portfolio.selected.len(), 1);
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("missing-counterfactual".into())
    );
    assert_eq!(portfolio.ledger.reserved.trials, 1);
    assert!(
        portfolio
            .deferred
            .iter()
            .any(|item| { item.reasons.contains(&DeferralReason::EvidenceSaturated) })
    );
}

#[test]
fn all_saturated_low_risk_work_leaves_the_budget_unused() {
    let context = context(vec![gap(
        "nothing-worth-buying",
        ExperienceOpportunityKind::HardenSkill,
        OpportunityReason::EvidenceSaturated,
        Severity::Low,
        ExposureBand::Rare,
        ValueBand::Low,
        ReusePotential::Narrow,
        EvidenceNovelty::Duplicate,
        1,
        evidence(12, 12, 3, 3, 3, 0),
    )]);
    let opportunities = generate(&context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(5), &context)
        .unwrap();
    assert!(portfolio.selected.is_empty());
    assert_eq!(portfolio.ledger.remaining.trials, 5);
    assert_eq!(portfolio.ledger.reserved.trials, 0);
}

#[test]
fn critical_four_trial_gap_reserves_budget_before_four_cheap_low_gaps() {
    let mut gaps = vec![gap(
        "critical",
        ExperienceOpportunityKind::ValidateRecovery,
        OpportunityReason::CriticalFailureUnmitigated,
        Severity::Critical,
        ExposureBand::Frequent,
        ValueBand::Critical,
        ReusePotential::Broad,
        EvidenceNovelty::NewFailureClass,
        4,
        EvidenceSummary::default(),
    )];
    for index in 0..4 {
        gaps.push(gap(
            &format!("cheap-{index}"),
            ExperienceOpportunityKind::ValidateLesson,
            OpportunityReason::Custom("minor gap".into()),
            Severity::Low,
            ExposureBand::Rare,
            ValueBand::Medium,
            ReusePotential::Narrow,
            EvidenceNovelty::Replication,
            1,
            EvidenceSummary::default(),
        ));
    }
    let context = context(gaps);
    let opportunities = generate(&context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(4), &context)
        .unwrap();
    assert_eq!(portfolio.selected.len(), 1);
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("critical".into())
    );
}

#[test]
fn one_contradiction_resets_maturity_and_large_blast_radius_breaks_ties() {
    let context = context(vec![
        gap(
            "small-blast-radius",
            ExperienceOpportunityKind::ResolveContradiction,
            OpportunityReason::EvidenceContradicted,
            Severity::High,
            ExposureBand::Occasional,
            ValueBand::High,
            ReusePotential::Narrow,
            EvidenceNovelty::MechanismChallenge,
            1,
            evidence(12, 8, 3, 3, 3, 1),
        ),
        gap(
            "large-blast-radius",
            ExperienceOpportunityKind::ResolveContradiction,
            OpportunityReason::EvidenceContradicted,
            Severity::High,
            ExposureBand::VeryFrequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::MechanismChallenge,
            1,
            evidence(12, 8, 3, 3, 3, 1),
        ),
    ]);
    let opportunities = generate(&context);
    assert!(opportunities.iter().all(|item| {
        item.saturation == EvidenceSaturation::Contradicted
            && item.marginal_value.band == ValueBand::Critical
    }));
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &context)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("large-blast-radius".into())
    );
}

#[test]
fn reuse_then_lower_cost_are_explicit_lexicographic_tiebreakers() {
    let base = |name, reuse, trials| {
        gap(
            name,
            ExperienceOpportunityKind::Custom("neutral".into()),
            OpportunityReason::Custom("neutral".into()),
            Severity::Medium,
            ExposureBand::Occasional,
            ValueBand::Medium,
            reuse,
            EvidenceNovelty::Replication,
            trials,
            EvidenceSummary::default(),
        )
    };
    let reuse_context = context(vec![
        base("one-off", ReusePotential::OneOff, 1),
        base("broad", ReusePotential::Broad, 1),
    ]);
    let opportunities = generate(&reuse_context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &reuse_context)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("broad".into())
    );
    let one_off = opportunities
        .iter()
        .find(|item| item.target == ExperienceOpportunityTarget::RuntimeGap("one-off".into()))
        .unwrap();
    let broad = opportunities
        .iter()
        .find(|item| item.target == ExperienceOpportunityTarget::RuntimeGap("broad".into()))
        .unwrap();
    assert_eq!(
        dominance(one_off, broad),
        DominanceStatus::DominatedBy(broad.id.clone())
    );

    let context = context(vec![
        base("expensive", ReusePotential::Reusable, 2),
        base("cheap", ReusePotential::Reusable, 1),
    ]);
    let opportunities = generate(&context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(2), &context)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("cheap".into())
    );
}

#[test]
fn federation_causal_forecast_and_capability_opportunities_use_domain_economics() {
    let equal = |name, kind, reason, cost, evidence, learning, exposure| {
        gap(
            name,
            kind,
            reason,
            Severity::Medium,
            exposure,
            learning,
            ReusePotential::Reusable,
            EvidenceNovelty::ContextExtension,
            cost,
            evidence,
        )
    };

    let fed = context(vec![
        equal(
            "rediscover",
            ExperienceOpportunityKind::Custom("rediscover".into()),
            OpportunityReason::Custom("local rediscovery".into()),
            3,
            EvidenceSummary::default(),
            ValueBand::High,
            ExposureBand::Occasional,
        ),
        equal(
            "reproduce",
            ExperienceOpportunityKind::ReproduceFederatedExperience,
            OpportunityReason::FederatedEvidenceUnreproduced,
            1,
            EvidenceSummary::default(),
            ValueBand::High,
            ExposureBand::Occasional,
        ),
    ]);
    let opportunities = generate(&fed);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &fed)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("reproduce".into())
    );

    let causal = context(vec![
        equal(
            "repeat-failure",
            ExperienceOpportunityKind::ValidateLesson,
            OpportunityReason::RepeatedFailure,
            1,
            evidence(2, 1, 1, 0, 1, 0),
            ValueBand::Medium,
            ExposureBand::Occasional,
        ),
        equal(
            "discriminate-h3",
            ExperienceOpportunityKind::DiscriminateCausalHypotheses,
            OpportunityReason::CausalMechanismUnknown,
            1,
            evidence(2, 1, 1, 0, 1, 0),
            ValueBand::High,
            ExposureBand::Occasional,
        ),
    ]);
    let opportunities = generate(&causal);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &causal)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("discriminate-h3".into())
    );
    // A mechanism-discriminating design wins once failure risk is held equal.
    let causal_equal = context(vec![
        equal(
            "repeat-only",
            ExperienceOpportunityKind::DiscriminateCausalHypotheses,
            OpportunityReason::CausalMechanismUnknown,
            1,
            evidence(2, 1, 1, 0, 1, 0),
            ValueBand::Medium,
            ExposureBand::Occasional,
        ),
        equal(
            "discriminate",
            ExperienceOpportunityKind::DiscriminateCausalHypotheses,
            OpportunityReason::CausalMechanismUnknown,
            1,
            EvidenceSummary::default(),
            ValueBand::High,
            ExposureBand::Occasional,
        ),
    ]);
    let opportunities = generate(&causal_equal);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &causal_equal)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("discriminate".into())
    );

    let forecast = context(vec![
        gap(
            "mature-recovery",
            ExperienceOpportunityKind::ValidateRecovery,
            OpportunityReason::RecoveryMissing,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::Replication,
            1,
            evidence(6, 3, 2, 1, 2, 0),
        ),
        gap(
            "missing-warning",
            ExperienceOpportunityKind::ValidateEarlyWarning,
            OpportunityReason::ForecastMiss,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::NewFailureClass,
            1,
            EvidenceSummary::default(),
        ),
    ]);
    let opportunities = generate(&forecast);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &forecast)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("missing-warning".into())
    );

    let capability = context(vec![
        equal(
            "rare-gap",
            ExperienceOpportunityKind::Custom("ordinary".into()),
            OpportunityReason::Custom("ordinary".into()),
            1,
            EvidenceSummary::default(),
            ValueBand::High,
            ExposureBand::Rare,
        ),
        equal(
            "ten-thousand-tool-uses",
            ExperienceOpportunityKind::MinimizeCapability,
            OpportunityReason::CapabilityOverprovisioned,
            1,
            EvidenceSummary::default(),
            ValueBand::High,
            ExposureBand::VeryFrequent,
        ),
    ]);
    let opportunities = generate(&capability);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(1), &capability)
        .unwrap();
    assert_eq!(
        selected_target(&portfolio, &opportunities),
        ExperienceOpportunityTarget::RuntimeGap("ten-thousand-tool-uses".into())
    );
}

#[test]
fn runtime_exposure_and_assurance_use_break_otherwise_equal_ties() {
    for (kind, reason) in [
        (
            ExperienceOpportunityKind::ValidateLesson,
            OpportunityReason::EvidenceStale,
        ),
        (
            ExperienceOpportunityKind::CloseAssuranceGap,
            OpportunityReason::AssuranceBlocked,
        ),
    ] {
        let context = context(vec![
            gap(
                "rare",
                kind.clone(),
                reason.clone(),
                Severity::High,
                ExposureBand::Rare,
                ValueBand::High,
                ReusePotential::Reusable,
                EvidenceNovelty::ContextExtension,
                1,
                EvidenceSummary::default(),
            ),
            gap(
                "heavily-used",
                kind,
                reason,
                Severity::High,
                ExposureBand::VeryFrequent,
                ValueBand::High,
                ReusePotential::Reusable,
                EvidenceNovelty::ContextExtension,
                1,
                EvidenceSummary::default(),
            ),
        ]);
        let opportunities = generate(&context);
        let portfolio = DeterministicExperienceAllocationPolicy
            .allocate(&opportunities, &budget(1), &context)
            .unwrap();
        assert_eq!(
            selected_target(&portfolio, &opportunities),
            ExperienceOpportunityTarget::RuntimeGap("heavily-used".into())
        );
    }
}

#[test]
fn dependencies_safety_effects_and_approvals_block_reservations() {
    let missing: ExperienceOpportunityId = "opportunity-00000000-0000-4000-8000-000000000001"
        .parse()
        .unwrap();
    let mut dependency = gap(
        "dependency",
        ExperienceOpportunityKind::ValidateLesson,
        OpportunityReason::EvidenceStale,
        Severity::High,
        ExposureBand::Frequent,
        ValueBand::High,
        ReusePotential::Broad,
        EvidenceNovelty::ContextExtension,
        1,
        EvidenceSummary::default(),
    );
    dependency.dependencies.push(missing);
    let mut unsafe_gap = dependency.clone();
    unsafe_gap.target = ExperienceOpportunityTarget::RuntimeGap("unsafe".into());
    unsafe_gap.dependencies.clear();
    unsafe_gap.risk.trial_safety = TrialSafety::Unsupported;
    let mut approval = dependency.clone();
    approval.target = ExperienceOpportunityTarget::RuntimeGap("approval".into());
    approval.dependencies.clear();
    approval.risk.approval_required = true;
    let mut effect = dependency.clone();
    effect.target = ExperienceOpportunityTarget::RuntimeGap("effect".into());
    effect.dependencies.clear();
    effect.risk.external_effect_risk = EffectRisk::High;
    effect.estimated_cost.external_effect_risk = EffectRisk::High;
    let context = context(vec![dependency, unsafe_gap, approval, effect]);
    let opportunities = generate(&context);
    let portfolio = DeterministicExperienceAllocationPolicy
        .allocate(&opportunities, &budget(4), &context)
        .unwrap();
    assert!(portfolio.selected.is_empty());
    assert_eq!(portfolio.ledger.reserved, BudgetUsage::default());
    assert_eq!(portfolio.deferred.len(), 4);
}

#[test]
fn early_stop_releases_reserved_budget_and_actual_overrun_is_not_hidden() {
    let estimate = ExperimentCost {
        trials: 4,
        estimated_duration: Some(Duration::from_secs(40)),
        external_effect_risk: EffectRisk::ReadOnly,
        ..Default::default()
    };
    let mut ledger = ExperienceBudgetLedger::new(budget(4));
    ledger.reserve(&estimate, 0).unwrap();
    ledger.consume(
        &estimate,
        0,
        &ActualExperimentCost {
            trials: 1,
            duration: Duration::from_secs(5),
            ..Default::default()
        },
    );
    assert_eq!(ledger.consumed.trials, 1);
    assert_eq!(ledger.remaining.trials, 3);
    assert_eq!(ledger.reserved.trials, 0);
    let opportunity_context = context(vec![gap(
        "resolved-hypothesis",
        ExperienceOpportunityKind::DiscriminateCausalHypotheses,
        OpportunityReason::CausalMechanismUnknown,
        Severity::High,
        ExposureBand::Frequent,
        ValueBand::High,
        ReusePotential::Broad,
        EvidenceNovelty::MechanismChallenge,
        4,
        EvidenceSummary::default(),
    )]);
    let opportunity = generate(&opportunity_context).remove(0);
    assert_eq!(
        DeterministicOpportunityStopPolicy
            .should_stop(&opportunity, &["objective_satisfied".into()]),
        StopDecision::StopSatisfied
    );

    let mut overrun = ExperienceBudgetLedger::new(budget(2));
    let estimate = ExperimentCost {
        trials: 1,
        external_effect_risk: EffectRisk::ReadOnly,
        ..Default::default()
    };
    overrun.reserve(&estimate, 0).unwrap();
    overrun.consume(
        &estimate,
        0,
        &ActualExperimentCost {
            trials: 3,
            ..Default::default()
        },
    );
    assert_eq!(overrun.consumed.trials, 3);
    assert_eq!(overrun.remaining.trials, 0);
}

#[test]
fn completion_with_lower_actual_cost_replans_and_introduces_new_critical_work() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let initial = context(
        ["a", "b", "c"]
            .into_iter()
            .map(|name| {
                gap(
                    name,
                    ExperienceOpportunityKind::ValidateRecovery,
                    OpportunityReason::RecoveryMissing,
                    Severity::High,
                    ExposureBand::Frequent,
                    ValueBand::High,
                    ReusePotential::Broad,
                    EvidenceNovelty::ContextExtension,
                    1,
                    EvidenceSummary::default(),
                )
            })
            .collect(),
    );
    let opportunities = store.generate_experience_opportunities(&initial).unwrap();
    let portfolio = store
        .create_experience_portfolio(&opportunities, &budget(3), &initial)
        .unwrap();
    assert_eq!(portfolio.selected.len(), 3);
    assert_eq!(
        store
            .begin_experience_portfolio(&portfolio.id)
            .unwrap()
            .len(),
        3
    );
    assert!(
        store
            .begin_experience_portfolio(&portfolio.id)
            .unwrap()
            .is_empty()
    );
    let new_context = context(vec![gap(
        "replacement-recovery",
        ExperienceOpportunityKind::ValidateRecovery,
        OpportunityReason::CriticalFailureUnmitigated,
        Severity::Critical,
        ExposureBand::VeryFrequent,
        ValueBand::Critical,
        ReusePotential::Broad,
        EvidenceNovelty::NewFailureClass,
        2,
        EvidenceSummary::default(),
    )]);
    let additional = generate(&new_context);
    let revised = store
        .complete_opportunity_and_replan(
            &portfolio.id,
            ExperienceOpportunityResult {
                opportunity: opportunities[0].id.clone(),
                outcome: OpportunityOutcome::ContradictedExistingEvidence,
                learning_outcomes: vec![],
                actual_cost: ActualExperimentCost {
                    trials: 1,
                    duration: Duration::from_secs(1),
                    ..Default::default()
                },
                evidence_refs: vec!["trial:first-discriminating-result".into()],
                completed_at: Utc::now(),
            },
            &additional,
        )
        .unwrap();
    assert_eq!(revised.revision, 2);
    assert_eq!(revised.ledger.consumed.trials, 1);
    assert_eq!(revised.selected[0].opportunity, additional[0].id);
    assert_eq!(revised.ledger.reserved.trials, 2);
    assert_eq!(revised.ledger.remaining.trials, 0);
    assert_eq!(revised.deferred.len(), 2);
}

#[test]
fn compiler_routes_to_existing_learning_engine_types() {
    let context = context(vec![
        gap(
            "curriculum",
            ExperienceOpportunityKind::ValidateEarlyWarning,
            OpportunityReason::ForecastMiss,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::NewFailureClass,
            1,
            EvidenceSummary::default(),
        ),
        gap(
            "experiment",
            ExperienceOpportunityKind::ValidateRecovery,
            OpportunityReason::RecoveryMissing,
            Severity::High,
            ExposureBand::Frequent,
            ValueBand::High,
            ReusePotential::Broad,
            EvidenceNovelty::ContextExtension,
            1,
            EvidenceSummary::default(),
        ),
    ]);
    let opportunities = generate(&context);
    let plans = opportunities
        .iter()
        .map(|item| {
            DeterministicExperienceOpportunityCompiler
                .compile(item)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        plans
            .iter()
            .any(|plan| matches!(plan, ExecutableLearningPlan::Curriculum { .. }))
    );
    assert!(
        plans
            .iter()
            .any(|plan| matches!(plan, ExecutableLearningPlan::Experiment { .. }))
    );
}

#[test]
fn benchmark_and_fixture_library_cover_the_required_comparison() {
    let result = economics::benchmark::run().unwrap();
    assert_eq!(result.budget.max_trials(), 20);
    assert_eq!(result.budget.max_agent_runs, 5);
    assert!(result.backlog_size >= 22);
    let round_robin = &result.arms[0];
    let adaptive = &result.arms[2];
    assert!(
        adaptive.acquisition.critical_gaps_closed > round_robin.acquisition.critical_gaps_closed
    );
    assert!(
        adaptive.acquisition.saturated_evidence_spend
            < round_robin.acquisition.saturated_evidence_spend
    );
    assert!(adaptive.held_out.task_success_rate > round_robin.held_out.task_success_rate);

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/economics");
    for name in [
        "portfolio-basic",
        "saturation",
        "contradiction",
        "high-cost-critical",
        "reuse",
        "runtime-exposure",
        "early-stop",
        "adaptive-replan",
        "no-value",
        "federated-reproduction",
        "causal-discrimination",
        "forecast",
        "capability",
        "assurance",
    ] {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(name).join("fixture.json")).unwrap())
                .unwrap();
        assert_eq!(value["version"], 1, "fixture {name}");
    }
}
