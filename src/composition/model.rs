// SPDX-License-Identifier: Apache-2.0
use crate::{
    assurance::{BehavioralCondition, ForbiddenOutcome},
    budget::ExperienceBudget,
    capability::{CapabilityManifest, ExecutionCapability},
    core::*,
    curriculum::Severity,
    epistemic::EvidenceRef,
    experimentation::{ExperimentQuality, StartingStateProof},
    hierarchy::{KnowledgeArtifactRef, KnowledgeScope, ScopeValue},
    runtime::FailureSignatureRef,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Reuse executable contract subjects; knowledge is never an executable step.
pub type ComposableArtifactRef = crate::assurance::ContractSubject;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRevision {
    pub component: ComposableArtifactRef,
    pub revision: String,
    pub content_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composition {
    pub id: CompositionId,
    pub revision: u64,
    pub name: String,
    pub scope: KnowledgeScope,
    pub steps: Vec<CompositionStep>,
    pub relations: Vec<CompositionRelation>,
    pub contract: CompositionContract,
    pub maturity: CompositionMaturity,
    pub evidence: Vec<CompositionEvidenceId>,
    pub assumptions: Vec<OperationalAssumption>,
    pub cross_step_preconditions: Vec<CrossStepPrecondition>,
    pub recoveries: Vec<CompositionRecoveryPlan>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionStep {
    pub id: CompositionStepId,
    pub component: ComponentRevision,
    pub input_bindings: Vec<StateBinding>,
    pub expected_outputs: Vec<StateClaim>,
    pub preconditions: Vec<KnowledgeArtifactRef>,
    pub invariants: Vec<KnowledgeArtifactRef>,
    pub local_recovery: Option<RecoveryId>,
    pub capabilities: CapabilityManifest,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateBinding {
    pub key: String,
    pub from: Option<CompositionStepId>,
    pub source_key: String,
    pub resource: CapabilityFlowResource,
    pub allowed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionRelationKind {
    Before,
    RequiresSuccessOf,
    ConsumesOutputOf,
    EstablishesPreconditionFor,
    InvalidatesAssumptionOf,
    RecoveryFor,
    Compensates,
    AlternativeTo,
}
impl CompositionRelationKind {
    pub fn orders(self) -> bool {
        matches!(
            self,
            Self::Before
                | Self::RequiresSuccessOf
                | Self::ConsumesOutputOf
                | Self::EstablishesPreconditionFor
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRelation {
    pub from: CompositionStepId,
    pub to: CompositionStepId,
    pub kind: CompositionRelationKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionMaturity {
    Candidate,
    Testable,
    Supported,
    Validated,
    Hardened,
    Degraded,
    Stale,
    Retired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionContract {
    pub preconditions: Vec<BehavioralCondition>,
    pub postconditions: Vec<BehavioralCondition>,
    pub sequence_invariants: Vec<SequenceInvariant>,
    pub forbidden_outcomes: Vec<ForbiddenOutcome>,
    pub capability_policy: CompositionCapabilityPolicy,
    pub effect_policy: CompositionEffectPlan,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCapabilityPolicy {
    pub persistent_capabilities: CapabilityManifest,
    pub allow_sensitive_handoffs: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossStepPrecondition {
    pub required_by: CompositionStepId,
    pub established_by: Option<CompositionStepId>,
    pub condition: BehavioralCondition,
    pub freshness: StateFreshnessRequirement,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalAssumption {
    pub id: OperationalAssumptionId,
    pub owner: ComposableArtifactRef,
    pub condition: BehavioralCondition,
    pub scope: KnowledgeScope,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateClaim {
    pub key: String,
    pub value: ScopeValue,
    pub source: StateClaimSource,
    pub freshness: StateFreshness,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateClaimSource {
    StepOutput,
    RuntimeObservation,
    EffectReceipt,
    ToolAttestation,
    UserProvided,
    AgentReported,
}
impl StateClaimSource {
    pub fn trust(self) -> crate::knowledge_runtime::ContextValueSource {
        use crate::knowledge_runtime::ContextValueSource as S;
        match self {
            Self::StepOutput => S::AdapterObserved,
            Self::RuntimeObservation => S::RuntimeObserved,
            Self::EffectReceipt => S::EffectAdapterObserved,
            Self::ToolAttestation => S::ToolAttestation,
            Self::UserProvided => S::UserProvided,
            Self::AgentReported => S::AgentReported,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateFreshness {
    pub observed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub step: Option<CompositionStepId>,
    pub external_version: Option<ExternalStateVersion>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStateVersion {
    pub resource: String,
    pub version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StateFreshnessRequirement {
    None,
    SameStep,
    BeforeNextMutation,
    MaxAge(std::time::Duration),
    AuthoritativeRefreshRequired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEvidenceProvenance {
    pub evidence: Vec<EvidenceRef>,
    pub evaluator: String,
    pub environment: String,
    pub intervention: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateHandoff {
    pub id: StateHandoffId,
    pub from: CompositionStepId,
    pub to: CompositionStepId,
    pub facts: Vec<StateClaim>,
    pub artifacts: Vec<ArtifactRef>,
    pub external_state_versions: Vec<ExternalStateVersion>,
    pub provenance: CompositionEvidenceProvenance,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceInvariant {
    pub id: SequenceInvariantId,
    pub scope: SequenceScope,
    pub condition: BehavioralCondition,
    pub severity: Severity,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SequenceScope {
    EntireComposition,
    Between {
        from: CompositionStepId,
        to: CompositionStepId,
    },
    Before {
        step: CompositionStepId,
    },
    After {
        step: CompositionStepId,
    },
    UntilCommitPoint {
        commit_point: CompositionCommitPointId,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceAntiPattern {
    pub pattern: Vec<CompositionStepId>,
    pub scope: KnowledgeScope,
    pub known_failure: FailureSignatureRef,
    pub evidence: Vec<EvidenceRef>,
    pub knowledge: KnowledgeArtifactRef,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionCompatibilityStatus {
    Compatible,
    CompatibleWithConditions,
    Conflict,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionCompatibilityFindingKind {
    UnsatisfiedPrecondition,
    InvalidatedAssumption,
    ConstraintConflict,
    CapabilityConflict,
    EffectConflict,
    RecoveryConflict,
    StateHandoffRisk,
    OrderingRequirement,
    UnknownCompatibility,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCompatibilityFinding {
    pub kind: CompositionCompatibilityFindingKind,
    pub steps: Vec<CompositionStepId>,
    pub severity: Severity,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionPreflightReport {
    pub composition: CompositionId,
    pub revision: u64,
    pub status: CompositionCompatibilityStatus,
    pub findings: Vec<CompositionCompatibilityFinding>,
    pub untested_pairs: Vec<(CompositionStepId, CompositionStepId)>,
    pub capability_plan: CompositionCapabilityPlan,
    pub empirical_validation_required: bool,
}
pub type CompositionCompatibilityAssessment = CompositionPreflightReport;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCapabilityPlan {
    pub steps: BTreeMap<CompositionStepId, CapabilityManifest>,
    pub persistent_capabilities: CapabilityManifest,
    pub findings: Vec<CapabilityFlow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityFlow {
    pub source_step: CompositionStepId,
    pub target_step: CompositionStepId,
    pub resource: CapabilityFlowResource,
    pub classification: CapabilityFlowClassification,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFlowResource {
    Secret,
    Credential,
    File,
    NetworkReachability,
    EffectAuthority,
    ExternalStateHandle,
    Custom(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFlowClassification {
    Allowed,
    Sensitive,
    Forbidden,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEffect {
    pub step: CompositionStepId,
    pub effect: EffectId,
    pub adapter: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCommitPoint {
    pub id: CompositionCommitPointId,
    pub after_step: CompositionStepId,
    pub irreversible_effects: Vec<EffectId>,
    pub rollback_capabilities_required: Vec<ExecutionCapability>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompensationRelation {
    pub committed: CompositionStepId,
    pub compensating: CompositionStepId,
    pub preserves_original_receipt: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEffectPlan {
    pub effects: Vec<CompositionEffect>,
    pub commit_points: Vec<CompositionCommitPoint>,
    pub compensation_edges: Vec<CompensationRelation>,
    pub atomicity: CompositionAtomicity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionAtomicity {
    None,
    PerStep,
    AdapterLocalGroups,
    FullyAtomic,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRecoveryPlan {
    pub failure_point: CompositionStepId,
    pub recoveries: Vec<CompositionRecoveryStep>,
    pub invariants: Vec<SequenceInvariant>,
    pub effect_reconciliation: Vec<EffectId>,
    pub status: CompositionRecoveryStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRecoveryStep {
    pub recovery: RecoveryId,
    pub revision: u64,
    pub requires: Vec<BehavioralCondition>,
    pub establishes: Vec<BehavioralCondition>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionRecoveryStatus {
    Candidate,
    Supported,
    Validated,
    Partial,
    Unsafe,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionRecoveryOutcome {
    FullyRecovered,
    LocallyRecovered,
    Compensated,
    Degraded,
    Failed,
    Inconclusive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionExperimentRequest {
    pub initial_state: Vec<StateClaim>,
    pub step_inputs: BTreeMap<CompositionStepId, serde_json::Value>,
    pub composition: CompositionId,
    pub revision: u64,
    pub starting_state: StartingStateProof,
    pub failure_injections: Vec<CompositionFailureInjection>,
    pub evaluation: CompositionEvaluationSpec,
    pub budget: ExperienceBudget,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionFailureInjection {
    pub before_step: CompositionStepId,
    pub perturbation: String,
    pub facts: Vec<StateClaim>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEvaluationSpec {
    pub evaluator: String,
    pub checks: Vec<BehavioralCondition>,
    pub required_failure_points: Vec<CompositionStepId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionEvidence {
    pub failure_points: Vec<CompositionStepId>,
    pub test_input_hash: String,
    pub id: CompositionEvidenceId,
    pub composition: CompositionId,
    pub composition_revision: u64,
    pub starting_state: StartingStateProof,
    pub step_results: Vec<CompositionStepResult>,
    pub invariant_results: Vec<SequenceInvariantEvaluation>,
    pub effect_results: Vec<EffectId>,
    pub recovery_result: Option<CompositionRecoveryOutcome>,
    pub outcome: CompositionOutcome,
    pub experiment_quality: ExperimentQuality,
    pub provenance: CompositionEvidenceProvenance,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionStepResult {
    pub step: CompositionStepId,
    pub component: ComponentRevision,
    pub outcome: CompositionOutcome,
    pub observations: Vec<StateClaim>,
    pub attestation: Option<ExecutionAttestationId>,
    pub runtime_decision: Option<RuntimeDecisionId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceInvariantEvaluation {
    pub invariant: SequenceInvariantId,
    pub step: CompositionStepId,
    pub phase: SequenceEvaluationPhase,
    pub satisfied: Option<bool>,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceEvaluationPhase {
    BeforeStep,
    AfterStep,
    CommitPoint,
    Completion,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionOutcome {
    Pass,
    Degraded,
    Fail,
    Inconclusive,
    Invalid,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionFailure {
    pub id: InteractionFailureId,
    pub composition: CompositionId,
    pub origin_steps: Vec<CompositionStepId>,
    pub manifestation_step: CompositionStepId,
    pub kind: InteractionFailureKind,
    pub failure_signature: FailureSignatureRef,
    pub violated_invariants: Vec<SequenceInvariantId>,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionFailureKind {
    StateInvalidation,
    AssumptionInvalidation,
    OrderingFailure,
    CapabilityFlow,
    ConstraintConflict,
    RecoveryConflict,
    EffectConflict,
    ResourceContention,
    DelayedConsequence,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderingConstraint {
    pub before: ComposableArtifactRef,
    pub after: ComposableArtifactRef,
    pub scope: KnowledgeScope,
    pub evidence: Vec<EvidenceRef>,
    pub maturity: crate::hierarchy::KnowledgeMaturity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionCandidate {
    pub steps: Vec<CompositionStepId>,
    pub reason: String,
    pub kind: InteractionFailureKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentHealth {
    pub component: ComponentRevision,
    pub status: CompositionHealthStatus,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionHealthStatus {
    Healthy,
    RevalidationRequired,
    Degraded,
    Broken,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionDependencyHealth {
    pub components: Vec<ComponentHealth>,
    pub status: CompositionHealthStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeSkill {
    pub id: CompositeSkillId,
    pub composition: CompositionId,
    pub revision: u64,
    pub contract: CompositionContract,
    pub maturity: CompositionMaturity,
    pub operating_envelope: KnowledgeScope,
    pub evidence_manifest: crate::assurance::EvidenceManifest,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionCommitState {
    pub reached: Vec<CompositionCommitPointId>,
    pub committed_effects: Vec<EffectId>,
    pub compensated_effects: Vec<EffectId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRuntimeContext {
    pub composition: CompositionId,
    pub revision: u64,
    pub step: CompositionStepId,
    pub completed_steps: Vec<CompositionStepId>,
    pub current_handoffs: Vec<StateHandoff>,
    pub active_sequence_invariants: Vec<SequenceInvariantId>,
    pub commit_state: CompositionCommitState,
    pub state: Vec<StateClaim>,
    pub external_versions: BTreeMap<String, String>,
    pub assessed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionNextStepAssessment {
    pub composition: CompositionId,
    pub revision: u64,
    pub step: CompositionStepId,
    pub findings: Vec<String>,
    pub unknown: Vec<String>,
    pub active_invariants: Vec<SequenceInvariantId>,
    pub component_health: CompositionHealthStatus,
}
