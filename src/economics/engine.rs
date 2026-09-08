// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Error, Result, curriculum::Severity, effects::EffectRisk};
use std::{cmp::Reverse, collections::BTreeSet, time::Duration};

pub const OPPORTUNITY_GENERATOR_VERSION: &str = "deterministic-opportunity-generator-v1";
pub const ALLOCATION_POLICY_VERSION: &str = "lexicographic-experience-allocation-v1";
pub const SATURATION_POLICY_VERSION: &str = "contextual-saturation-v1";
pub const COST_ESTIMATOR_VERSION: &str = "explicit-cost-estimator-v1";

pub trait ExperienceOpportunityGenerator {
    fn generate(&self, context: &ExperiencePlanningContext) -> Result<Vec<ExperienceOpportunity>>;
}

pub trait EvidenceSaturationPolicy {
    fn assess(
        &self,
        target: &ExperienceOpportunityTarget,
        evidence: &EvidenceSummary,
    ) -> EvidenceSaturation;
}

pub trait ExperienceAllocationPolicy {
    fn allocate(
        &self,
        opportunities: &[ExperienceOpportunity],
        budget: &crate::budget::ExperienceBudget,
        context: &ExperiencePlanningContext,
    ) -> Result<ExperiencePortfolio>;
}

pub trait ExperienceOpportunityCompiler {
    fn compile(&self, opportunity: &ExperienceOpportunity) -> Result<ExecutableLearningPlan>;
}

pub trait OpportunityStopPolicy {
    fn should_stop(&self, opportunity: &ExperienceOpportunity, evidence: &[String])
    -> StopDecision;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicEvidenceSaturationPolicy;

impl EvidenceSaturationPolicy for DeterministicEvidenceSaturationPolicy {
    fn assess(
        &self,
        _target: &ExperienceOpportunityTarget,
        evidence: &EvidenceSummary,
    ) -> EvidenceSaturation {
        if evidence.contradictions > 0 {
            return EvidenceSaturation::Contradicted;
        }
        if evidence.observations == 0 {
            EvidenceSaturation::Sparse
        } else if evidence.counterfactuals >= 2
            && evidence.distinct_contexts >= 2
            && evidence.diversity_domains >= 2
            && evidence.equivalent_replications >= 4
        {
            EvidenceSaturation::Saturated
        } else if evidence.counterfactuals > 0
            && evidence.distinct_contexts > 0
            && evidence.equivalent_replications >= 2
        {
            EvidenceSaturation::Mature
        } else {
            EvidenceSaturation::Developing
        }
    }
}

fn marginal_value(
    saturation: EvidenceSaturation,
    evidence: &EvidenceSummary,
) -> MarginalEvidenceValue {
    if saturation == EvidenceSaturation::Contradicted {
        return MarginalEvidenceValue {
            band: ValueBand::Critical,
            reason: MarginalValueReason::ResolvesContradiction,
        };
    }
    if !evidence.fresh && evidence.observations > 0 {
        return MarginalEvidenceValue {
            band: ValueBand::High,
            reason: MarginalValueReason::FirstContextExtension,
        };
    }
    match saturation {
        EvidenceSaturation::Sparse => MarginalEvidenceValue {
            band: ValueBand::High,
            reason: MarginalValueReason::FirstEvidence,
        },
        EvidenceSaturation::Developing if evidence.counterfactuals == 0 => MarginalEvidenceValue {
            band: ValueBand::High,
            reason: MarginalValueReason::FirstCounterfactual,
        },
        EvidenceSaturation::Developing => MarginalEvidenceValue {
            band: ValueBand::Medium,
            reason: MarginalValueReason::Replication,
        },
        EvidenceSaturation::Mature => MarginalEvidenceValue {
            band: ValueBand::Low,
            reason: MarginalValueReason::RepeatedEquivalentReplication,
        },
        EvidenceSaturation::Saturated => MarginalEvidenceValue {
            band: ValueBand::None,
            reason: MarginalValueReason::Saturated,
        },
        EvidenceSaturation::Contradicted => unreachable!("handled above"),
    }
}

fn severity_value(severity: Severity) -> ValueBand {
    match severity {
        Severity::Informational => ValueBand::None,
        Severity::Low => ValueBand::Low,
        Severity::Medium => ValueBand::Medium,
        Severity::High => ValueBand::High,
        Severity::Critical => ValueBand::Critical,
    }
}

fn exposure_value(exposure: ExposureBand) -> ValueBand {
    match exposure {
        ExposureBand::Rare => ValueBand::Low,
        ExposureBand::Occasional => ValueBand::Medium,
        ExposureBand::Frequent => ValueBand::High,
        ExposureBand::VeryFrequent => ValueBand::Critical,
        ExposureBand::Unknown => ValueBand::None,
    }
}

fn reuse_value(reuse: ReusePotential) -> ValueBand {
    match reuse {
        ReusePotential::OneOff => ValueBand::Low,
        ReusePotential::Narrow => ValueBand::Medium,
        ReusePotential::Reusable => ValueBand::High,
        ReusePotential::Broad => ValueBand::Critical,
        ReusePotential::Unknown => ValueBand::None,
    }
}

fn novelty_value(novelty: EvidenceNovelty) -> ValueBand {
    match novelty {
        EvidenceNovelty::Duplicate => ValueBand::None,
        EvidenceNovelty::Replication => ValueBand::Low,
        EvidenceNovelty::ContextExtension => ValueBand::Medium,
        EvidenceNovelty::MechanismChallenge => ValueBand::High,
        EvidenceNovelty::NewFailureClass => ValueBand::Critical,
        EvidenceNovelty::Unknown => ValueBand::None,
    }
}

fn stable_opportunity_id(gap: &ExperienceGap) -> Result<crate::core::ExperienceOpportunityId> {
    let digest = blake3::hash(&serde_json::to_vec(&(
        &gap.kind,
        &gap.target,
        &gap.reasons,
    ))?);
    let bytes = digest.as_bytes();
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        (bytes[6] & 0x0f) | 0x40,
        bytes[7],
        (bytes[8] & 0x3f) | 0x80,
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    );
    format!("opportunity-{uuid}").parse()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicExperienceOpportunityGenerator;

impl ExperienceOpportunityGenerator for DeterministicExperienceOpportunityGenerator {
    fn generate(&self, context: &ExperiencePlanningContext) -> Result<Vec<ExperienceOpportunity>> {
        let saturation_policy = DeterministicEvidenceSaturationPolicy;
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        for gap in &context.gaps {
            let id = stable_opportunity_id(gap)?;
            if !seen.insert(id.clone()) {
                continue;
            }
            let saturation = saturation_policy.assess(&gap.target, &gap.evidence);
            let marginal = marginal_value(saturation, &gap.evidence);
            let evidence_gap = if gap.evidence.contradictions > 0 {
                ValueBand::Critical
            } else {
                match saturation {
                    EvidenceSaturation::Sparse => ValueBand::High,
                    EvidenceSaturation::Developing => ValueBand::Medium,
                    EvidenceSaturation::Mature => ValueBand::Low,
                    EvidenceSaturation::Saturated => ValueBand::None,
                    EvidenceSaturation::Contradicted => ValueBand::Critical,
                }
            };
            let risk_reduction = RiskReductionEstimate {
                failure_severity: gap.severity,
                occurrence_exposure: gap.exposure,
                mitigation_gap: gap.mitigation_gap,
                rationale: vec![format!(
                    "{:?} exposure with {:?} mitigation",
                    gap.exposure, gap.mitigation_gap
                )],
            };
            let mut rationale = gap.reasons.clone();
            if saturation == EvidenceSaturation::Saturated
                && !rationale.contains(&OpportunityReason::EvidenceSaturated)
            {
                rationale.push(OpportunityReason::EvidenceSaturated);
            }
            result.push(ExperienceOpportunity {
                id,
                kind: gap.kind.clone(),
                target: gap.target.clone(),
                rationale,
                value: ExperienceValueVector {
                    risk_reduction: severity_value(gap.severity),
                    learning_value: gap.learning.band,
                    reuse_potential: reuse_value(gap.reuse),
                    decision_relevance: gap
                        .decision_relevance
                        .likely_decision_change
                        .max(exposure_value(gap.exposure)),
                    evidence_gap,
                    novelty: novelty_value(gap.novelty),
                },
                estimated_cost: gap.estimated_cost.clone(),
                risk: gap.risk.clone(),
                dependencies: gap.dependencies.clone(),
                status: if saturation == EvidenceSaturation::Saturated && gap.evidence.fresh {
                    ExperienceOpportunityStatus::Saturated
                } else {
                    ExperienceOpportunityStatus::Eligible
                },
                created_at: context.now,
                risk_reduction,
                learning: gap.learning.clone(),
                decision_relevance: gap.decision_relevance.clone(),
                reuse: gap.reuse,
                novelty: gap.novelty,
                saturation,
                marginal_value: marginal,
                generator_version: OPPORTUNITY_GENERATOR_VERSION.into(),
            });
        }
        result.sort_by_key(|item| item.id.clone());
        Ok(result)
    }
}

fn duration_ms(duration: Option<Duration>) -> u64 {
    duration
        .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl BudgetUsage {
    pub fn from_cost(cost: &ExperimentCost, approvals: usize) -> Self {
        Self {
            trials: cost.trials,
            agent_runs: cost.agent_runs,
            duration_ms: duration_ms(cost.estimated_duration),
            human_approvals: approvals,
        }
    }
}

impl ExperienceBudgetLedger {
    pub fn new(budget: crate::budget::ExperienceBudget) -> Self {
        let remaining = BudgetUsage {
            trials: budget.max_trials(),
            agent_runs: budget.max_agent_runs,
            duration_ms: budget.max_duration_ms.unwrap_or(u64::MAX),
            human_approvals: budget.max_human_approvals.unwrap_or(usize::MAX),
        };
        Self {
            id: crate::core::ExperienceBudgetLedgerId::new(),
            budget,
            reserved: BudgetUsage::default(),
            consumed: BudgetUsage::default(),
            remaining,
        }
    }

    pub fn can_reserve(&self, cost: &ExperimentCost, approvals: usize) -> bool {
        let usage = BudgetUsage::from_cost(cost, approvals);
        usage.trials <= self.remaining.trials
            && usage.agent_runs <= self.remaining.agent_runs
            && usage.duration_ms <= self.remaining.duration_ms
            && usage.human_approvals <= self.remaining.human_approvals
            && cost.external_effect_risk <= self.budget.allowed_effect_risk
    }

    pub fn reserve(&mut self, cost: &ExperimentCost, approvals: usize) -> Result<()> {
        if !self.can_reserve(cost, approvals) {
            return Err(Error::Intervention(
                "Experience opportunity exceeds remaining acquisition budget".into(),
            ));
        }
        let usage = BudgetUsage::from_cost(cost, approvals);
        add_usage(&mut self.reserved, &usage);
        subtract_usage(&mut self.remaining, &usage);
        Ok(())
    }

    pub fn release(&mut self, cost: &ExperimentCost, approvals: usize) {
        let usage = BudgetUsage::from_cost(cost, approvals);
        subtract_usage(&mut self.reserved, &usage);
        add_usage(&mut self.remaining, &usage);
        self.cap_remaining();
    }

    pub fn consume(
        &mut self,
        reserved: &ExperimentCost,
        approval_reserved: usize,
        actual: &ActualExperimentCost,
    ) {
        self.release(reserved, approval_reserved);
        let actual_usage = BudgetUsage {
            trials: actual.trials,
            agent_runs: actual.agent_runs,
            duration_ms: u64::try_from(actual.duration.as_millis()).unwrap_or(u64::MAX),
            human_approvals: actual.approvals_requested,
        };
        add_usage(&mut self.consumed, &actual_usage);
        subtract_usage(&mut self.remaining, &actual_usage);
    }

    fn cap_remaining(&mut self) {
        self.remaining.trials = self.remaining.trials.min(
            self.budget
                .max_trials()
                .saturating_sub(self.consumed.trials),
        );
        self.remaining.agent_runs = self.remaining.agent_runs.min(
            self.budget
                .max_agent_runs
                .saturating_sub(self.consumed.agent_runs),
        );
        if let Some(limit) = self.budget.max_duration_ms {
            self.remaining.duration_ms = self
                .remaining
                .duration_ms
                .min(limit.saturating_sub(self.consumed.duration_ms));
        }
        if let Some(limit) = self.budget.max_human_approvals {
            self.remaining.human_approvals = self
                .remaining
                .human_approvals
                .min(limit.saturating_sub(self.consumed.human_approvals));
        }
    }
}

fn add_usage(target: &mut BudgetUsage, value: &BudgetUsage) {
    target.trials = target.trials.saturating_add(value.trials);
    target.agent_runs = target.agent_runs.saturating_add(value.agent_runs);
    target.duration_ms = target.duration_ms.saturating_add(value.duration_ms);
    target.human_approvals = target.human_approvals.saturating_add(value.human_approvals);
}

fn subtract_usage(target: &mut BudgetUsage, value: &BudgetUsage) {
    target.trials = target.trials.saturating_sub(value.trials);
    target.agent_runs = target.agent_runs.saturating_sub(value.agent_runs);
    target.duration_ms = target.duration_ms.saturating_sub(value.duration_ms);
    target.human_approvals = target.human_approvals.saturating_sub(value.human_approvals);
}

fn exposure_rank(value: ExposureBand) -> u8 {
    match value {
        ExposureBand::Unknown => 0,
        ExposureBand::Rare => 1,
        ExposureBand::Occasional => 2,
        ExposureBand::Frequent => 3,
        ExposureBand::VeryFrequent => 4,
    }
}

fn reuse_rank(value: ReusePotential) -> u8 {
    match value {
        ReusePotential::Unknown => 0,
        ReusePotential::OneOff => 1,
        ReusePotential::Narrow => 2,
        ReusePotential::Reusable => 3,
        ReusePotential::Broad => 4,
    }
}

fn novelty_rank(value: EvidenceNovelty) -> u8 {
    match value {
        EvidenceNovelty::Unknown => 0,
        EvidenceNovelty::Duplicate => 1,
        EvidenceNovelty::Replication => 2,
        EvidenceNovelty::ContextExtension => 3,
        EvidenceNovelty::MechanismChallenge => 4,
        EvidenceNovelty::NewFailureClass => 5,
    }
}

fn priority_class(
    opportunity: &ExperienceOpportunity,
    objective: ExperiencePortfolioObjective,
) -> u8 {
    let has = |reason: &OpportunityReason| opportunity.rationale.contains(reason);
    if opportunity.risk_reduction.failure_severity == Severity::Critical
        && matches!(
            opportunity.risk_reduction.mitigation_gap,
            MitigationGap::Significant | MitigationGap::Unmitigated
        )
    {
        0
    } else if has(&OpportunityReason::EvidenceContradicted)
        || opportunity.saturation == EvidenceSaturation::Contradicted
    {
        1
    } else if (objective == ExperiencePortfolioObjective::Assurance
        && has(&OpportunityReason::AssuranceBlocked))
        || (objective == ExperiencePortfolioObjective::Efficiency
            && has(&OpportunityReason::CapabilityOverprovisioned))
        || (matches!(
            opportunity.kind,
            ExperienceOpportunityKind::DiscriminateCausalHypotheses
        ) && has(&OpportunityReason::CausalMechanismUnknown))
    {
        // A single discriminating intervention can dominate another replay of
        // the same failure when it separates competing mechanisms.
        2
    } else if has(&OpportunityReason::RepeatedFailure)
        || has(&OpportunityReason::RecoveryMissing)
        || has(&OpportunityReason::ForecastMiss)
    {
        2
    } else if has(&OpportunityReason::EvidenceStale) {
        3
    } else if has(&OpportunityReason::RuntimeAbstention) || has(&OpportunityReason::RuntimeUnknown)
    {
        4
    } else if has(&OpportunityReason::AssuranceBlocked) {
        5
    } else if has(&OpportunityReason::CausalMechanismUnknown)
        || has(&OpportunityReason::LowEvidenceDiversity)
    {
        if objective == ExperiencePortfolioObjective::Research {
            3
        } else {
            6
        }
    } else if has(&OpportunityReason::CapabilityOverprovisioned) {
        7
    } else {
        8
    }
}

type OpportunityRankKey = (
    u8,
    Reverse<ValueBand>,
    Reverse<usize>,
    Reverse<u8>,
    Reverse<ValueBand>,
    Reverse<u8>,
    Reverse<u8>,
    usize,
    usize,
    crate::core::ExperienceOpportunityId,
);

fn rank_key(
    opportunity: &ExperienceOpportunity,
    objective: ExperiencePortfolioObjective,
) -> OpportunityRankKey {
    (
        priority_class(opportunity, objective),
        Reverse(opportunity.value.risk_reduction),
        Reverse(opportunity.decision_relevance.affected_decisions),
        Reverse(exposure_rank(
            opportunity.risk_reduction.occurrence_exposure,
        )),
        Reverse(opportunity.value.learning_value),
        Reverse(reuse_rank(opportunity.reuse)),
        Reverse(novelty_rank(opportunity.novelty)),
        opportunity.estimated_cost.trials,
        opportunity.estimated_cost.agent_runs,
        opportunity.id.clone(),
    )
}

fn portfolio_category(kind: &ExperienceOpportunityKind) -> EvidencePortfolioCategory {
    match kind {
        ExperienceOpportunityKind::RevalidateStaleExperience => {
            EvidencePortfolioCategory::Revalidation
        }
        ExperienceOpportunityKind::ResolveContradiction
        | ExperienceOpportunityKind::ChallengeCausalHypothesis
        | ExperienceOpportunityKind::DiscriminateCausalHypotheses
        | ExperienceOpportunityKind::ChallengeAbstraction
        | ExperienceOpportunityKind::ReduceReflexFalsePositives
        | ExperienceOpportunityKind::ReduceForecastFalsePositives => {
            EvidencePortfolioCategory::Challenge
        }
        ExperienceOpportunityKind::ExploreOperatingEnvelope
        | ExperienceOpportunityKind::InvestigateForecastMiss
        | ExperienceOpportunityKind::ResolveRuntimeUnknown
        | ExperienceOpportunityKind::IncreaseEvidenceDiversity
        | ExperienceOpportunityKind::ReduceKnowledgeFragmentation => {
            EvidencePortfolioCategory::Discovery
        }
        ExperienceOpportunityKind::HardenSkill => EvidencePortfolioCategory::Hardening,
        ExperienceOpportunityKind::ValidateEarlyWarning
        | ExperienceOpportunityKind::ValidatePreventiveIntervention => {
            EvidencePortfolioCategory::Prevention
        }
        ExperienceOpportunityKind::MinimizeCapability => EvidencePortfolioCategory::Efficiency,
        ExperienceOpportunityKind::ValidateLesson
        | ExperienceOpportunityKind::ValidateRecovery
        | ExperienceOpportunityKind::ValidateReflex
        | ExperienceOpportunityKind::ReproduceFederatedExperience
        | ExperienceOpportunityKind::CloseAssuranceGap
        | ExperienceOpportunityKind::ValidateAbstraction
        | ExperienceOpportunityKind::ValidateTransfer
        | ExperienceOpportunityKind::Custom(_) => EvidencePortfolioCategory::Validation,
    }
}

pub fn dominance(
    candidate: &ExperienceOpportunity,
    other: &ExperienceOpportunity,
) -> DominanceStatus {
    let a = &candidate.value;
    let b = &other.value;
    let no_worse = a.risk_reduction >= b.risk_reduction
        && a.learning_value >= b.learning_value
        && a.reuse_potential >= b.reuse_potential
        && a.decision_relevance >= b.decision_relevance
        && a.evidence_gap >= b.evidence_gap
        && a.novelty >= b.novelty
        && candidate.estimated_cost.trials <= other.estimated_cost.trials
        && candidate.estimated_cost.agent_runs <= other.estimated_cost.agent_runs;
    let better = a != b
        || candidate.estimated_cost.trials < other.estimated_cost.trials
        || candidate.estimated_cost.agent_runs < other.estimated_cost.agent_runs;
    if no_worse && better {
        DominanceStatus::NonDominated
    } else {
        let reverse_no_worse = b.risk_reduction >= a.risk_reduction
            && b.learning_value >= a.learning_value
            && b.reuse_potential >= a.reuse_potential
            && b.decision_relevance >= a.decision_relevance
            && b.evidence_gap >= a.evidence_gap
            && b.novelty >= a.novelty
            && other.estimated_cost.trials <= candidate.estimated_cost.trials
            && other.estimated_cost.agent_runs <= candidate.estimated_cost.agent_runs;
        let reverse_better = a != b
            || other.estimated_cost.trials < candidate.estimated_cost.trials
            || other.estimated_cost.agent_runs < candidate.estimated_cost.agent_runs;
        if reverse_no_worse && reverse_better {
            DominanceStatus::DominatedBy(other.id.clone())
        } else {
            DominanceStatus::Incomparable
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicExperienceAllocationPolicy;

impl ExperienceAllocationPolicy for DeterministicExperienceAllocationPolicy {
    fn allocate(
        &self,
        opportunities: &[ExperienceOpportunity],
        budget: &crate::budget::ExperienceBudget,
        context: &ExperiencePlanningContext,
    ) -> Result<ExperiencePortfolio> {
        if budget.max_parallel_trials == Some(0) {
            return Err(Error::InvalidInput(
                "Experience budget must allow at least one parallel trial".into(),
            ));
        }
        let mut ledger = ExperienceBudgetLedger::new(budget.clone());
        let mut ranked = opportunities.iter().collect::<Vec<_>>();
        ranked.sort_by_key(|item| rank_key(item, context.objective));
        let dominated = opportunities
            .iter()
            .filter_map(|candidate| {
                opportunities
                    .iter()
                    .filter(|other| {
                        other.id != candidate.id
                            && priority_class(other, context.objective)
                                == priority_class(candidate, context.objective)
                    })
                    .find_map(|other| match dominance(candidate, other) {
                        DominanceStatus::DominatedBy(id) => Some((candidate.id.clone(), id)),
                        DominanceStatus::NonDominated | DominanceStatus::Incomparable => None,
                    })
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut selected = Vec::new();
        let mut deferred = Vec::new();
        let mut selected_targets = BTreeSet::new();
        for opportunity in ranked {
            let target_key = serde_json::to_string(&opportunity.target)?;
            let mut reasons = Vec::new();
            if let Some(dominator) = dominated.get(&opportunity.id) {
                reasons.push(DeferralReason::DominatedBy(dominator.clone()));
            }
            if !selected_targets.insert(target_key) {
                reasons.push(DeferralReason::DuplicateOpportunity);
            }
            if opportunity.status == ExperienceOpportunityStatus::Saturated
                || (opportunity.saturation == EvidenceSaturation::Saturated
                    && !opportunity
                        .rationale
                        .contains(&OpportunityReason::EvidenceStale))
            {
                reasons.push(DeferralReason::EvidenceSaturated);
            }
            if opportunity.status == ExperienceOpportunityStatus::Invalidated {
                reasons.push(DeferralReason::StaleOpportunity);
            }
            if opportunity.risk.trial_safety == crate::curriculum::TrialSafety::Unsupported
                || opportunity.risk.external_effect_risk > budget.allowed_effect_risk
            {
                reasons.push(DeferralReason::UnsafeToExperiment);
            }
            if opportunity
                .dependencies
                .iter()
                .any(|id| !context.completed_dependencies.contains(id))
            {
                reasons.push(DeferralReason::DependencyNotSatisfied);
            }
            let approvals = usize::from(opportunity.risk.approval_required);
            if opportunity.risk.approval_required && budget.max_human_approvals == Some(0) {
                reasons.push(DeferralReason::HumanApprovalUnavailable);
            }
            if reasons.is_empty()
                && opportunity.marginal_value.band <= ValueBand::Low
                && opportunity.risk_reduction.failure_severity <= Severity::Low
                && matches!(
                    opportunity.risk_reduction.occurrence_exposure,
                    ExposureBand::Rare | ExposureBand::Unknown
                )
            {
                reasons.push(DeferralReason::LowerMarginalValue);
            }
            if reasons.is_empty() && !ledger.can_reserve(&opportunity.estimated_cost, approvals) {
                reasons.push(DeferralReason::BudgetInsufficient);
                if selected.iter().any(|item: &PortfolioSelection| {
                    opportunities.iter().any(|selected_opportunity| {
                        selected_opportunity.id == item.opportunity
                            && priority_class(selected_opportunity, context.objective) == 0
                    })
                }) {
                    reasons.push(DeferralReason::HigherPriorityRiskExists);
                }
            }
            if !reasons.is_empty() {
                deferred.push(PortfolioDeferral {
                    opportunity: opportunity.id.clone(),
                    reasons,
                });
                continue;
            }
            ledger.reserve(&opportunity.estimated_cost, approvals)?;
            let mut selection_reasons = vec![
                SelectionReason::PriorityClass(format!(
                    "class-{}",
                    priority_class(opportunity, context.objective)
                )),
                SelectionReason::HigherRiskReduction,
                SelectionReason::HigherDecisionRelevance,
                SelectionReason::HigherLearningValue,
                SelectionReason::HigherReusePotential,
                SelectionReason::FitsRemainingBudget,
            ];
            if priority_class(opportunity, context.objective) == 0 {
                selection_reasons.push(SelectionReason::CriticalBudgetReserved);
            }
            selected.push(PortfolioSelection {
                opportunity: opportunity.id.clone(),
                priority: selected.len() + 1,
                category: portfolio_category(&opportunity.kind),
                reasons: selection_reasons,
                reserved_cost: opportunity.estimated_cost.clone(),
            });
        }
        Ok(ExperiencePortfolio {
            id: crate::core::ExperiencePortfolioId::new(),
            opportunities: opportunities.iter().map(|item| item.id.clone()).collect(),
            selected,
            deferred,
            budget: budget.clone(),
            ledger,
            policy: ExperienceAllocationPolicyRef {
                id: crate::core::ExperienceAllocationPolicyId::new(),
                version: ALLOCATION_POLICY_VERSION.into(),
            },
            objective: context.objective,
            policy_versions: PortfolioPolicyVersions {
                opportunity_generator: OPPORTUNITY_GENERATOR_VERSION.into(),
                allocation_policy: ALLOCATION_POLICY_VERSION.into(),
                saturation_policy: SATURATION_POLICY_VERSION.into(),
                cost_estimator: COST_ESTIMATOR_VERSION.into(),
            },
            created_at: context.now,
            revision: 1,
            revision_reason: None,
        })
    }
}

impl DeterministicExperienceAllocationPolicy {
    pub fn replan(
        &self,
        previous: &ExperiencePortfolio,
        opportunities: &[ExperienceOpportunity],
        context: &ExperiencePlanningContext,
        reason: PortfolioRevisionReason,
    ) -> Result<ExperiencePortfolio> {
        let mut budget = previous.budget.clone();
        budget.max_realities = previous.ledger.remaining.trials;
        budget.max_curriculum_trials = Some(previous.ledger.remaining.trials);
        budget.max_agent_runs = previous.ledger.remaining.agent_runs;
        budget.max_duration_ms = Some(previous.ledger.remaining.duration_ms);
        budget.max_human_approvals = Some(previous.ledger.remaining.human_approvals);
        let mut revised = self.allocate(opportunities, &budget, context)?;
        revised.id = previous.id.clone();
        revised.revision = previous.revision.saturating_add(1);
        revised.revision_reason = Some(reason);
        revised.budget = previous.budget.clone();
        revised.ledger.budget = previous.budget.clone();
        revised.ledger.consumed = previous.ledger.consumed.clone();
        Ok(revised)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicExperienceOpportunityCompiler;

impl ExperienceOpportunityCompiler for DeterministicExperienceOpportunityCompiler {
    fn compile(&self, opportunity: &ExperienceOpportunity) -> Result<ExecutableLearningPlan> {
        use crate::curriculum::CurriculumGoalKind as Goal;
        use crate::experimentation::ExperimentIntent;
        let target = opportunity.target.clone();
        match (&opportunity.kind, &opportunity.target) {
            (
                ExperienceOpportunityKind::ChallengeCausalHypothesis
                | ExperienceOpportunityKind::DiscriminateCausalHypotheses,
                ExperienceOpportunityTarget::CausalHypothesis(id),
            ) => Ok(ExecutableLearningPlan::CausalInvestigation(id.clone())),
            (
                ExperienceOpportunityKind::ReproduceFederatedExperience,
                ExperienceOpportunityTarget::FederatedObject(object),
            ) => Ok(ExecutableLearningPlan::FederationReproduction(
                object.clone(),
            )),
            (
                ExperienceOpportunityKind::MinimizeCapability,
                ExperienceOpportunityTarget::Tool(tool),
            ) => Ok(ExecutableLearningPlan::CapabilityCurriculum(tool.clone())),
            (ExperienceOpportunityKind::ValidateRecovery, _) => {
                Ok(ExecutableLearningPlan::Experiment {
                    target,
                    intent: ExperimentIntent::ValidateRecovery,
                })
            }
            (ExperienceOpportunityKind::ExploreOperatingEnvelope, _) => {
                Ok(ExecutableLearningPlan::Experiment {
                    target,
                    intent: ExperimentIntent::MapBoundary,
                })
            }
            (ExperienceOpportunityKind::ValidateLesson, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ValidateSkill,
                })
            }
            (ExperienceOpportunityKind::HardenSkill, _) => Ok(ExecutableLearningPlan::Curriculum {
                target,
                goal: Goal::ValidateSkill,
            }),
            (
                ExperienceOpportunityKind::ValidateReflex
                | ExperienceOpportunityKind::ReduceReflexFalsePositives,
                _,
            ) => Ok(ExecutableLearningPlan::Curriculum {
                target,
                goal: Goal::ValidateReflex,
            }),
            (ExperienceOpportunityKind::RevalidateStaleExperience, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::RevalidateOldExperience,
                })
            }
            (ExperienceOpportunityKind::IncreaseEvidenceDiversity, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::IncreaseEvidenceDiversity,
                })
            }
            (ExperienceOpportunityKind::CloseAssuranceGap, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::SatisfyAssuranceRequirement,
                })
            }
            (ExperienceOpportunityKind::ValidateEarlyWarning, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ValidateEarlyWarning,
                })
            }
            (ExperienceOpportunityKind::ValidatePreventiveIntervention, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ValidatePreventiveIntervention,
                })
            }
            (ExperienceOpportunityKind::InvestigateForecastMiss, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::DiscoverEarlyWarning,
                })
            }
            (ExperienceOpportunityKind::ReduceForecastFalsePositives, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ReduceForecastFalsePositives,
                })
            }
            (ExperienceOpportunityKind::ResolveContradiction, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ResolveContradiction,
                })
            }
            (
                ExperienceOpportunityKind::ValidateAbstraction
                | ExperienceOpportunityKind::ValidateTransfer,
                _,
            ) => Ok(ExecutableLearningPlan::Experiment {
                target,
                intent: ExperimentIntent::ValidateTransfer,
            }),
            (ExperienceOpportunityKind::ChallengeAbstraction, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::ChallengeAbstraction,
                })
            }
            (ExperienceOpportunityKind::ReduceKnowledgeFragmentation, _) => {
                Ok(ExecutableLearningPlan::Curriculum {
                    target,
                    goal: Goal::FindGeneralizationBoundary,
                })
            }
            _ => Ok(ExecutableLearningPlan::Experiment {
                target,
                intent: ExperimentIntent::ResolveUncertainty,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicOpportunityStopPolicy;

impl OpportunityStopPolicy for DeterministicOpportunityStopPolicy {
    fn should_stop(
        &self,
        opportunity: &ExperienceOpportunity,
        evidence: &[String],
    ) -> StopDecision {
        if opportunity.risk.trial_safety == crate::curriculum::TrialSafety::Unsupported {
            StopDecision::StopUnsafe
        } else if opportunity.saturation == EvidenceSaturation::Contradicted {
            StopDecision::StopContradicted
        } else if opportunity.saturation == EvidenceSaturation::Saturated {
            StopDecision::StopSaturated
        } else if evidence.iter().any(|item| item == "objective_satisfied") {
            StopDecision::StopSatisfied
        } else {
            StopDecision::Continue
        }
    }
}

pub fn default_opportunity_risk() -> OpportunityRisk {
    OpportunityRisk {
        trial_safety: crate::curriculum::TrialSafety::Safe,
        external_effect_risk: EffectRisk::ReadOnly,
        isolation_required: crate::capability::RealityRequirements {
            filesystem_isolation: crate::capability::IsolationLevel::None,
            process_isolation: crate::capability::IsolationLevel::None,
            network_isolation: crate::capability::IsolationLevel::None,
            credential_isolation: crate::capability::IsolationLevel::None,
            effect_gating: false,
        },
        approval_required: false,
    }
}
