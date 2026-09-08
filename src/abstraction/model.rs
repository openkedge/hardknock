// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    budget::ExperienceBudget,
    core::*,
    epistemic::{DiversityClass, EvidenceRef},
    experimentation::ExperimentQuality,
    lesson::{ActionPattern, ContextSelector},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeArtifactKind {
    Lesson,
    Skill,
    Constraint,
    AntiPattern,
    Recovery,
    CausalMechanism,
    FailureTrajectory,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KnowledgeArtifactRef {
    pub kind: KnowledgeArtifactKind,
    pub id: String,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperiencePatternKind {
    Lesson,
    Skill,
    Constraint,
    AntiPattern,
    Recovery,
    CausalMechanism,
    FailureTrajectory,
}

impl From<KnowledgeArtifactKind> for ExperiencePatternKind {
    fn from(value: KnowledgeArtifactKind) -> Self {
        match value {
            KnowledgeArtifactKind::Lesson => Self::Lesson,
            KnowledgeArtifactKind::Skill => Self::Skill,
            KnowledgeArtifactKind::Constraint => Self::Constraint,
            KnowledgeArtifactKind::AntiPattern => Self::AntiPattern,
            KnowledgeArtifactKind::Recovery => Self::Recovery,
            KnowledgeArtifactKind::CausalMechanism => Self::CausalMechanism,
            KnowledgeArtifactKind::FailureTrajectory => Self::FailureTrajectory,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperiencePatternStatus {
    Candidate,
    TransferTestable,
    Supported,
    Validated,
    Contradicted,
    Overgeneralized,
    Stale,
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum VariableValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
    Set(Vec<String>),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextVariableKind {
    Environment,
    Resource,
    SoftwareVersion,
    DependencyVersion,
    ActionSemantics,
    Idempotency,
    Reversibility,
    Externality,
    Concurrency,
    Tool,
    AgentRuntime,
    FailureMode,
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRelevance {
    Required,
    Suspected,
    Varies,
    Irrelevant,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContextVariable {
    pub name: String,
    pub kind: ContextVariableKind,
    pub value: VariableValue,
    pub relevance: ContextRelevance,
}

pub type ContextVariableRef = ContextVariable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateOperator {
    Equals,
    NotEquals,
    Present,
    Absent,
    Contains,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PatternPredicate {
    pub variable: String,
    pub operator: PredicateOperator,
    pub value: Option<VariableValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomePattern {
    pub classification: String,
    pub observable: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CausalHypothesisRef {
    pub id: CausalHypothesisId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternStructure {
    pub trigger: Option<PatternPredicate>,
    pub context_variables: Vec<ContextVariableRef>,
    pub action_pattern: Option<ActionPattern>,
    pub outcome_pattern: Option<OutcomePattern>,
    pub causal_mechanisms: Vec<CausalHypothesisRef>,
    pub required_conditions: Vec<PatternPredicate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeArtifact {
    pub artifact: KnowledgeArtifactRef,
    pub statement: String,
    pub scope: ContextSelector,
    pub structure: PatternStructure,
    pub evidence: Vec<EvidenceRef>,
    /// Root origins are used to detect many descendants of one belief.
    pub root_origins: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperiencePattern {
    pub id: ExperiencePatternId,
    pub name: String,
    pub kind: ExperiencePatternKind,
    pub members: Vec<KnowledgeArtifactRef>,
    pub structure: PatternStructure,
    pub scope: ContextSelector,
    pub status: ExperiencePatternStatus,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionContext {
    pub minimum_source_contexts: usize,
    pub minimum_members: usize,
    pub now: DateTime<Utc>,
}

impl Default for AbstractionContext {
    fn default() -> Self {
        Self {
            minimum_source_contexts: 2,
            minimum_members: 2,
            now: Utc::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstractKnowledgeKind {
    AbstractLesson,
    AbstractSkill,
    AbstractConstraint,
    AbstractAntiPattern,
    AbstractRecovery,
}

impl AbstractKnowledgeKind {
    pub fn from_pattern(kind: ExperiencePatternKind) -> Option<Self> {
        match kind {
            ExperiencePatternKind::Lesson | ExperiencePatternKind::CausalMechanism => {
                Some(Self::AbstractLesson)
            }
            ExperiencePatternKind::Skill => Some(Self::AbstractSkill),
            ExperiencePatternKind::Constraint => Some(Self::AbstractConstraint),
            ExperiencePatternKind::AntiPattern | ExperiencePatternKind::FailureTrajectory => {
                Some(Self::AbstractAntiPattern)
            }
            ExperiencePatternKind::Recovery => Some(Self::AbstractRecovery),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeMaturity {
    Candidate,
    TransferTestable,
    Supported,
    Validated,
    Contradicted,
    Overgeneralized,
    Stale,
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbstractKnowledgeRef {
    pub id: AbstractKnowledgeId,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ApplicabilityClause {
    pub variable: ContextVariableKind,
    pub name: String,
    pub operator: PredicateOperator,
    pub value: Option<VariableValue>,
    pub rationale: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicabilityPredicate {
    pub all_of: Vec<ApplicabilityClause>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralizationBoundary {
    pub included: Vec<ApplicabilityClause>,
    pub excluded: Vec<ApplicabilityClause>,
    pub unknown: Vec<ApplicabilityClause>,
    pub evidence: Vec<TransferEvidenceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneralizationRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeOrigin {
    Local,
    FederatedAdvisory,
    CandidateProvider,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeProvenance {
    pub source_artifacts: Vec<KnowledgeArtifactRef>,
    pub source_contexts: Vec<ContextSelector>,
    pub evidence: Vec<EvidenceRef>,
    pub root_origins: Vec<String>,
    pub origin: KnowledgeOrigin,
    pub generator: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractKnowledge {
    pub id: AbstractKnowledgeId,
    pub revision: u64,
    pub kind: AbstractKnowledgeKind,
    pub statement: String,
    pub applicability: ApplicabilityPredicate,
    pub generalization_boundary: GeneralizationBoundary,
    pub supporting_patterns: Vec<ExperiencePatternId>,
    pub transfer_evidence: Vec<TransferEvidenceRef>,
    pub specializations: Vec<AbstractKnowledgeRef>,
    pub exceptions: Vec<KnowledgeExceptionRef>,
    pub maturity: KnowledgeMaturity,
    pub risk: GeneralizationRisk,
    pub provenance: KnowledgeProvenance,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateAbstraction {
    pub pattern: ExperiencePattern,
    pub knowledge: AbstractKnowledge,
    pub varying_dimensions: Vec<ContextVariableKind>,
    pub rationale: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferHypothesisStatus {
    Candidate,
    Testable,
    Supported,
    Contradicted,
    Inconclusive,
    Untestable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TransferExpectation {
    PreventFailure(String),
    ImproveOutcome,
    PreserveInvariant(String),
    EnableRecovery(String),
    ReduceRepeatedFailure,
    ReduceCapabilityRequirement,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferHypothesis {
    pub id: TransferHypothesisId,
    pub abstract_knowledge: AbstractKnowledgeRef,
    pub source_contexts: Vec<ContextSelector>,
    pub target_context: ContextSelector,
    pub expected_behavior: TransferExpectation,
    pub status: TransferHypothesisStatus,
    pub evidence: Vec<TransferEvidenceRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferEvaluationSet {
    pub hypothesis: TransferHypothesisId,
    pub source_contexts: Vec<ContextSelector>,
    pub held_out_contexts: Vec<ContextSelector>,
    pub negative_controls: Vec<ContextSelector>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransferEvidenceRef {
    pub id: TransferEvidenceId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbstractionTrialRef {
    pub experiment_id: ExperimentId,
    pub trial_id: TrialId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferEvidenceOutcome {
    Supports,
    Contradicts,
    NarrowsScope,
    Inconclusive,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferContextRole {
    Source,
    HeldOut,
    NegativeControl,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDifferenceSummary {
    pub changed_dimensions: Vec<ContextVariableKind>,
    pub preserved_dimensions: Vec<ContextVariableKind>,
    pub unknown_dimensions: Vec<ContextVariableKind>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferQuality {
    pub context_distance: ContextDifferenceSummary,
    pub analogy_mapping_complete: bool,
    pub experiment_quality: ExperimentQuality,
    pub evidence_diversity: DiversityClass,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferEvidence {
    pub id: TransferEvidenceId,
    pub hypothesis: TransferHypothesisId,
    pub source_artifact: AbstractKnowledgeRef,
    pub target_context: ContextSelector,
    pub context_role: TransferContextRole,
    pub expected_applicable: bool,
    pub application_triggered: bool,
    pub baseline_trial: AbstractionTrialRef,
    pub transfer_trial: AbstractionTrialRef,
    pub outcome: TransferEvidenceOutcome,
    pub quality: TransferQuality,
    pub observable_behavior: String,
    pub boundary_clause: Option<ApplicabilityClause>,
    pub local: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KnowledgeExceptionRef {
    pub id: KnowledgeExceptionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExceptionReason {
    ContradictoryTransfer,
    AlternativeMechanism,
    IdempotentSemantics,
    VersionSpecificBehavior,
    DifferentAuthorityModel,
    DifferentConsistencyModel,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeException {
    pub id: KnowledgeExceptionId,
    pub parent: AbstractKnowledgeRef,
    pub context: ContextSelector,
    pub applicability: ApplicabilityPredicate,
    pub reason: ExceptionReason,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeSpecialization {
    pub parent: AbstractKnowledgeRef,
    pub child: AbstractKnowledgeRef,
    pub additional_scope: ApplicabilityPredicate,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
}

/// Links a generic AbstractSkill contract to optional stronger, domain-specific
/// contracts without forcing every implementation into one state schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractSkillContractBinding {
    pub abstract_skill: AbstractKnowledgeRef,
    pub generic_contract: crate::assurance::BehavioralContractRef,
    pub specialized_contracts: Vec<SpecializedContractBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecializedContractBinding {
    pub specialization: AbstractKnowledgeRef,
    pub contract: crate::assurance::BehavioralContractRef,
    pub strengthened_conditions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeDistillation {
    pub id: KnowledgeDistillationId,
    pub inputs: Vec<KnowledgeArtifactRef>,
    pub outputs: Vec<AbstractKnowledgeRef>,
    pub preserved_exceptions: Vec<KnowledgeExceptionId>,
    pub evidence_manifest: EvidenceManifestId,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "abstract_id", rename_all = "snake_case")]
pub enum KnowledgeRepresentationState {
    DirectActive,
    RepresentedByAbstract(AbstractKnowledgeId),
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeRepresentation {
    pub artifact: KnowledgeArtifactRef,
    pub state: KnowledgeRepresentationState,
    pub changed_at: DateTime<Utc>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegativeTransferOutcome {
    FalseConstraint,
    HarmfulSkillTransfer,
    FailedRecoveryTransfer,
    MisleadingLesson,
    UnnecessaryReplan,
    IncorrectAbstention,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NegativeTransferEvent {
    pub knowledge: AbstractKnowledgeId,
    pub context: ContextSelector,
    pub outcome: NegativeTransferOutcome,
    pub evidence: EvidenceRef,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OvergeneralizationReport {
    pub candidate: AbstractKnowledgeId,
    pub failing_contexts: Vec<ContextSelector>,
    pub false_positive_contexts: Vec<ContextSelector>,
    pub recommended_boundary_revision: Option<GeneralizationBoundary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionDecision {
    Promote,
    MoreEvidenceRequired,
    NarrowScope,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionAssessment {
    pub decision: PromotionDecision,
    pub reasons: Vec<String>,
    pub held_out_support: usize,
    pub negative_controls: usize,
    pub false_constraint_applications: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextVariableMapping {
    pub source: ContextVariableRef,
    pub target: ContextVariableRef,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalogyMapping {
    pub id: AnalogyMappingId,
    pub source_context: ContextSelector,
    pub target_context: ContextSelector,
    pub mapped_variables: Vec<ContextVariableMapping>,
    pub unmapped_variables: Vec<ContextVariableRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractCausalMechanism {
    pub cause_pattern: PatternPredicate,
    pub effect_pattern: PatternPredicate,
    pub scope: ApplicabilityPredicate,
    pub supporting_models: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferContext {
    pub selector: ContextSelector,
    pub variables: Vec<ContextVariable>,
    pub role: TransferContextRole,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferTestPlan {
    pub abstraction: AbstractKnowledgeId,
    pub contexts: Vec<TransferContext>,
    pub budget: ExperienceBudget,
    pub intent: crate::experimentation::ExperimentIntent,
    pub requires_equivalent_start: bool,
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberKnowledgeHealth {
    pub artifact: KnowledgeArtifactRef,
    pub maturity: KnowledgeMaturity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferEvidenceHealth {
    pub evidence: TransferEvidenceRef,
    pub stale: bool,
    pub contradicted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstractionFreshnessStatus {
    Fresh,
    PartiallyStale,
    Stale,
    Contradicted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionFreshness {
    pub member_health: Vec<MemberKnowledgeHealth>,
    pub transfer_health: Vec<TransferEvidenceHealth>,
    pub status: AbstractionFreshnessStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionLevel {
    Abstract,
    Specialization,
    Specific,
    Exception,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeCandidateRef {
    pub reference: String,
    pub abstract_parent: Option<AbstractKnowledgeId>,
    pub level: ResolutionLevel,
    pub statement: String,
    pub scope: ContextSelector,
    pub applicability: ApplicabilityPredicate,
    pub boundary: GeneralizationBoundary,
    pub maturity: KnowledgeMaturity,
    pub quality: ExperimentQuality,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedKnowledge {
    pub reference: String,
    pub level: ResolutionLevel,
    pub statement: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeResolution {
    pub abstract_matches: Vec<String>,
    pub specialization_matches: Vec<String>,
    pub specific_matches: Vec<String>,
    pub exceptions: Vec<String>,
    pub selected: Vec<ResolvedKnowledge>,
    pub unknown_boundary_conditions: Vec<ApplicabilityClause>,
    pub suppressed_members: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbstractionDevelopmentSummary {
    pub candidate_patterns: usize,
    pub validated_abstractions: usize,
    pub active_specializations: usize,
    pub exceptions: usize,
    pub negative_transfer_events: usize,
    pub unknown_boundaries: usize,
    pub represented_specific_artifacts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionEvent {
    pub sequence: u64,
    pub subject: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbstractionMetrics {
    pub evaluated_transfers: usize,
    pub supported_transfers: usize,
    pub negative_transfers: usize,
    pub false_abstract_constraints: usize,
    pub constraint_applications: usize,
    pub specific_artifacts_before: usize,
    pub runtime_items_after: usize,
    pub specializations: usize,
    pub exceptions: usize,
    pub unknown_contexts: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbstractionImpact {
    pub runtime_uses: usize,
    pub skills_influenced: Vec<String>,
    pub constraints_influenced: Vec<String>,
    pub recoveries_influenced: Vec<String>,
    pub guard_candidates: Vec<String>,
    pub certifications: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbstractionFixtureCatalog {
    pub families: BTreeMap<String, String>,
}
