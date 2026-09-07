// SPDX-License-Identifier: Apache-2.0
use crate::{
    budget::ExperienceBudget,
    capability::RealityRequirements,
    core::*,
    curriculum::{LearningOutcome, Severity, TrialSafety},
    effects::EffectRisk,
    epistemic::FederatedObjectRef,
    runtime::FailureSignatureRef,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::Duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueBand {
    #[default]
    None,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceValueVector {
    pub risk_reduction: ValueBand,
    pub learning_value: ValueBand,
    pub reuse_potential: ValueBand,
    pub decision_relevance: ValueBand,
    pub evidence_gap: ValueBand,
    pub novelty: ValueBand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposureBand {
    Rare,
    Occasional,
    Frequent,
    VeryFrequent,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MitigationGap {
    None,
    Partial,
    Significant,
    Unmitigated,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskReductionEstimate {
    pub failure_severity: Severity,
    pub occurrence_exposure: ExposureBand,
    pub mitigation_gap: MitigationGap,
    pub rationale: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcomeClass {
    Strengthen,
    Contradict,
    NarrowScope,
    Validate,
    Retire,
    DiscoverFailureBoundary,
    DiscoverRecovery,
    ChangeRuntimeDecision,
    ChangeAssurance,
    NoMaterialChange,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningValueEstimate {
    pub band: ValueBand,
    pub possible_outcomes: Vec<LearningOutcomeClass>,
    pub decision_changing_outcomes: usize,
    pub rationale: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeUseBand {
    None,
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionRelevance {
    pub affected_decisions: usize,
    pub affected_task_families: usize,
    pub current_runtime_use: RuntimeUseBand,
    pub likely_decision_change: ValueBand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusePotential {
    OneOff,
    Narrow,
    Reusable,
    Broad,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceNovelty {
    Duplicate,
    Replication,
    ContextExtension,
    MechanismChallenge,
    NewFailureClass,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeCostBand {
    #[default]
    Negligible,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCostBand {
    #[default]
    None,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCostBand {
    #[default]
    None,
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentCost {
    pub trials: usize,
    pub agent_runs: usize,
    pub estimated_duration: Option<Duration>,
    pub compute: ComputeCostBand,
    pub human_attention: AttentionCostBand,
    pub staging_resources: ResourceCostBand,
    pub external_effect_risk: EffectRisk,
}
impl Default for ExperimentCost {
    fn default() -> Self {
        Self {
            trials: 1,
            agent_runs: 0,
            estimated_duration: None,
            compute: ComputeCostBand::Negligible,
            human_attention: AttentionCostBand::None,
            staging_resources: ResourceCostBand::None,
            external_effect_risk: EffectRisk::ReadOnly,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActualExperimentCost {
    pub trials: usize,
    pub agent_runs: usize,
    pub duration: Duration,
    pub compute_units: Option<f64>,
    pub approvals_requested: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunityRisk {
    pub trial_safety: TrialSafety,
    pub external_effect_risk: EffectRisk,
    pub isolation_required: RealityRequirements,
    pub approval_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceOpportunityKind {
    ValidateLesson,
    RevalidateStaleExperience,
    ResolveContradiction,
    ValidateRecovery,
    HardenSkill,
    ExploreOperatingEnvelope,
    ValidateReflex,
    ReduceReflexFalsePositives,
    ChallengeCausalHypothesis,
    DiscriminateCausalHypotheses,
    IncreaseEvidenceDiversity,
    ReproduceFederatedExperience,
    ValidateEarlyWarning,
    ValidatePreventiveIntervention,
    InvestigateForecastMiss,
    ReduceForecastFalsePositives,
    MinimizeCapability,
    ResolveRuntimeUnknown,
    CloseAssuranceGap,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", content = "id", rename_all = "snake_case")]
pub enum ExperienceOpportunityTarget {
    Skill(SkillId),
    Lesson(LessonId),
    Reflex(ReflexId),
    Recovery(RecoveryId),
    CausalHypothesis(CausalHypothesisId),
    EarlyWarning(EarlyWarningSignatureId),
    FailureSignature(FailureSignatureRef),
    AssuranceGap(String),
    RuntimeGap(String),
    FederatedObject(FederatedObjectRef),
    Tool(ToolId),
    TaskFamily(TaskFamilyId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityReason {
    CriticalFailureUnmitigated,
    RepeatedFailure,
    HighUsageSkillGap,
    RuntimeAbstention,
    RuntimeUnknown,
    EvidenceStale,
    EvidenceContradicted,
    LargeBlastRadius,
    AssuranceBlocked,
    RecoveryMissing,
    ReflexNoisy,
    ForecastMiss,
    ForecastNoisy,
    CausalMechanismUnknown,
    LowEvidenceDiversity,
    FederatedEvidenceUnreproduced,
    CapabilityOverprovisioned,
    OperatingEnvelopeUnknown,
    EvidenceSaturated,
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceOpportunityStatus {
    Candidate,
    Eligible,
    Selected,
    Running,
    Completed,
    Deferred,
    Saturated,
    Blocked,
    Invalidated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSaturation {
    Sparse,
    Developing,
    Mature,
    Saturated,
    Contradicted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginalValueReason {
    FirstEvidence,
    FirstCounterfactual,
    FirstTransfer,
    FirstContextExtension,
    ResolvesContradiction,
    AddsDiversity,
    Replication,
    RepeatedEquivalentReplication,
    Saturated,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarginalEvidenceValue {
    pub band: ValueBand,
    pub reason: MarginalValueReason,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvidenceSummary {
    pub observations: usize,
    pub equivalent_replications: usize,
    pub distinct_contexts: usize,
    pub counterfactuals: usize,
    pub diversity_domains: usize,
    pub contradictions: usize,
    pub fresh: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceOpportunity {
    pub id: ExperienceOpportunityId,
    pub kind: ExperienceOpportunityKind,
    pub target: ExperienceOpportunityTarget,
    pub rationale: Vec<OpportunityReason>,
    pub value: ExperienceValueVector,
    pub estimated_cost: ExperimentCost,
    pub risk: OpportunityRisk,
    pub dependencies: Vec<ExperienceOpportunityId>,
    pub status: ExperienceOpportunityStatus,
    pub created_at: DateTime<Utc>,
    pub risk_reduction: RiskReductionEstimate,
    pub learning: LearningValueEstimate,
    pub decision_relevance: DecisionRelevance,
    pub reuse: ReusePotential,
    pub novelty: EvidenceNovelty,
    pub saturation: EvidenceSaturation,
    pub marginal_value: MarginalEvidenceValue,
    pub generator_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceGap {
    pub kind: ExperienceOpportunityKind,
    pub target: ExperienceOpportunityTarget,
    pub reasons: Vec<OpportunityReason>,
    pub severity: Severity,
    pub exposure: ExposureBand,
    pub mitigation_gap: MitigationGap,
    pub learning: LearningValueEstimate,
    pub decision_relevance: DecisionRelevance,
    pub reuse: ReusePotential,
    pub novelty: EvidenceNovelty,
    pub evidence: EvidenceSummary,
    pub estimated_cost: ExperimentCost,
    pub risk: OpportunityRisk,
    pub dependencies: Vec<ExperienceOpportunityId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperiencePlanningContext {
    pub gaps: Vec<ExperienceGap>,
    #[serde(default)]
    pub completed_dependencies: BTreeSet<ExperienceOpportunityId>,
    pub objective: ExperiencePortfolioObjective,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub trials: usize,
    pub agent_runs: usize,
    pub duration_ms: u64,
    pub human_approvals: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceBudgetLedger {
    pub id: ExperienceBudgetLedgerId,
    pub budget: ExperienceBudget,
    pub reserved: BudgetUsage,
    pub consumed: BudgetUsage,
    pub remaining: BudgetUsage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperiencePortfolioObjective {
    #[default]
    Balanced,
    Resilience,
    Assurance,
    Research,
    Efficiency,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorationPolicy {
    pub minimum_replication_fraction: Option<f64>,
    pub maximum_novel_fraction: Option<f64>,
    pub prioritize_critical_unknowns: bool,
}
impl Default for ExplorationPolicy {
    fn default() -> Self {
        Self {
            minimum_replication_fraction: None,
            maximum_novel_fraction: None,
            prioritize_critical_unknowns: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceAllocationPolicyRef {
    pub id: ExperienceAllocationPolicyId,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionReason {
    PriorityClass(String),
    HigherRiskReduction,
    HigherDecisionRelevance,
    HigherLearningValue,
    HigherReusePotential,
    HigherNovelty,
    LowerCost,
    CriticalBudgetReserved,
    FitsRemainingBudget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePortfolioCategory {
    #[default]
    Validation,
    Revalidation,
    Challenge,
    Discovery,
    Hardening,
    Prevention,
    Efficiency,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferralReason {
    BudgetInsufficient,
    LowerMarginalValue,
    EvidenceSaturated,
    DependencyNotSatisfied,
    UnsafeToExperiment,
    MissingRealityCapability,
    HumanApprovalUnavailable,
    HigherPriorityRiskExists,
    DuplicateOpportunity,
    StaleOpportunity,
    DominatedBy(ExperienceOpportunityId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioSelection {
    pub opportunity: ExperienceOpportunityId,
    pub priority: usize,
    #[serde(default)]
    pub category: EvidencePortfolioCategory,
    pub reasons: Vec<SelectionReason>,
    pub reserved_cost: ExperimentCost,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioDeferral {
    pub opportunity: ExperienceOpportunityId,
    pub reasons: Vec<DeferralReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DominanceStatus {
    NonDominated,
    DominatedBy(ExperienceOpportunityId),
    Incomparable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortfolioRevisionReason {
    NewEvidence,
    Contradiction,
    OpportunityCompleted,
    OpportunityInvalidated,
    BudgetChanged,
    RuntimeExposureChanged,
    ForecastMiss,
    AssuranceChanged,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioPolicyVersions {
    pub opportunity_generator: String,
    pub allocation_policy: String,
    pub saturation_policy: String,
    pub cost_estimator: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperiencePortfolio {
    pub id: ExperiencePortfolioId,
    pub opportunities: Vec<ExperienceOpportunityId>,
    pub selected: Vec<PortfolioSelection>,
    pub deferred: Vec<PortfolioDeferral>,
    pub budget: ExperienceBudget,
    pub ledger: ExperienceBudgetLedger,
    pub policy: ExperienceAllocationPolicyRef,
    pub objective: ExperiencePortfolioObjective,
    pub policy_versions: PortfolioPolicyVersions,
    pub created_at: DateTime<Utc>,
    pub revision: u64,
    pub revision_reason: Option<PortfolioRevisionReason>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "plan", content = "target", rename_all = "snake_case")]
pub enum ExecutableLearningPlan {
    Curriculum {
        target: ExperienceOpportunityTarget,
        goal: crate::curriculum::CurriculumGoalKind,
    },
    Experiment {
        target: ExperienceOpportunityTarget,
        intent: crate::experimentation::ExperimentIntent,
    },
    CausalInvestigation(CausalHypothesisId),
    FederationReproduction(FederatedObjectRef),
    CapabilityCurriculum(ToolId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityOutcome {
    MaterialLearning,
    StrengthenedExistingEvidence,
    ContradictedExistingEvidence,
    NoMaterialChange,
    Inconclusive,
    FailedToExecute,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceOpportunityResult {
    pub opportunity: ExperienceOpportunityId,
    pub outcome: OpportunityOutcome,
    pub learning_outcomes: Vec<LearningOutcome>,
    pub actual_cost: ActualExperimentCost,
    pub evidence_refs: Vec<String>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LearningYield {
    pub material_updates: usize,
    pub decisions_changed: usize,
    pub gaps_closed: usize,
    pub contradictions_resolved: usize,
    pub failures_mitigated: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperienceAcquisitionSummary {
    pub open_opportunities: usize,
    pub critical_opportunities: usize,
    pub high_opportunities: usize,
    pub last_portfolio: Option<ExperiencePortfolioId>,
    pub trials_consumed: usize,
    pub material_outcomes: usize,
    pub critical_gaps_closed: usize,
    pub early_stop_trial_savings: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceDebtItem {
    pub target: ExperienceOpportunityTarget,
    pub severity: Severity,
    pub exposure: ExposureBand,
    pub age: Duration,
    pub reason: OpportunityReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopDecision {
    Continue,
    StopSatisfied,
    StopSaturated,
    StopContradicted,
    StopUnsafe,
    StopBudget,
}
