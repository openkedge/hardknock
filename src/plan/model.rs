// SPDX-License-Identifier: Apache-2.0
use crate::{
    assurance::BehavioralCondition,
    composition::{ComponentRevision, StateClaim, StateClaimSource, StateFreshnessRequirement},
    core::*,
    curriculum::Severity,
    epistemic::EvidenceRef,
    hierarchy::FreshnessStatus,
    knowledge_runtime::KnowledgeRevisionRef,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlan {
    pub id: ExecutionPlanId,
    pub goal: PlanGoal,
    pub revision: u64,
    pub steps: Vec<PlanStep>,
    pub assumptions: Vec<PlanAssumption>,
    pub invariants: Vec<PlanInvariant>,
    pub checkpoints: Vec<PlanCheckpoint>,
    pub commitment_points: Vec<PlanCommitmentPoint>,
    pub commitment_gates: Vec<CommitmentGate>,
    pub knowledge_dependencies: Vec<PlanKnowledgeDependency>,
    pub freshness_policy: PlanFreshnessPolicy,
    #[serde(default)]
    pub component_revisions: BTreeMap<PlanStepId, PlanComponentRevision>,
    pub status: PlanStatus,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanGoal {
    pub description: String,
    pub family: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: PlanStepId,
    pub kind: PlanStepKind,
    pub dependencies: Vec<PlanStepId>,
    pub required_assumptions: Vec<PlanAssumptionId>,
    pub required_invariants: Vec<PlanInvariantId>,
    pub expected_observations: Vec<StateClaim>,
    pub status: PlanStepStatus,
    pub severity: Severity,
    #[serde(default)]
    pub required_capabilities: Vec<crate::capability::ExecutionCapability>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PlanStepKind {
    Skill(SkillId),
    Composition(CompositionId),
    Tool(ToolId),
    Effect(EffectPlanId),
    Observe(ObservationSpec),
    Experiment(ExperimentTemplateRef),
    Approval(ApprovalRequirement),
    Custom(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanComponentRevision {
    Executable {
        revision: ComponentRevision,
    },
    Composition {
        id: CompositionId,
        revision: u64,
        hash: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Proposed,
    Active,
    Valid,
    NeedsVerification,
    NeedsReplan,
    Recovering,
    Completed,
    Abandoned,
    Invalid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    Proposed,
    Completed,
    Failed,
    Skipped,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservationSpec {
    pub key: String,
    pub condition: BehavioralCondition,
    pub freshness: StateFreshnessRequirement,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentTemplateRef {
    pub id: String,
    pub request: Option<Box<crate::experimentation::ExperimentRequest>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequirement {
    pub id: String,
    pub effects: Vec<EffectId>,
    pub max_age: Option<Duration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanAssumption {
    pub id: PlanAssumptionId,
    pub statement: String,
    pub predicate: BehavioralCondition,
    pub source: AssumptionSource,
    pub required_by: Vec<PlanStepId>,
    pub validity: AssumptionValidity,
    pub freshness: AssumptionFreshness,
    pub severity: Severity,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionSource {
    InitialObservation,
    AgentReported,
    ToolAttestation,
    EffectAdapter,
    UserProvided,
    KnowledgeArtifact,
    CompositionContract,
    Derived,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionValidity {
    Supported,
    Contradicted,
    Unknown,
    Stale,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssumptionFreshness {
    pub observed_at: DateTime<Utc>,
    pub requirement: StateFreshnessRequirement,
    pub status: FreshnessStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanInvariant {
    pub id: PlanInvariantId,
    pub condition: BehavioralCondition,
    pub scope: PlanInvariantScope,
    pub severity: Severity,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PlanInvariantScope {
    EntirePlan,
    Between { start: PlanStepId, end: PlanStepId },
    UntilCheckpoint(PlanCheckpointId),
    UntilCommitmentPoint(PlanCommitmentPointId),
    AfterCommitmentPoint(PlanCommitmentPointId),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanCheckpoint {
    pub id: PlanCheckpointId,
    pub after_step: Option<PlanStepId>,
    pub required_observations: Vec<ObservationSpec>,
    pub assumptions_to_revalidate: Vec<PlanAssumptionId>,
    pub invariants_to_verify: Vec<PlanInvariantId>,
    pub decision_required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanCommitmentPoint {
    pub id: PlanCommitmentPointId,
    pub after_step: PlanStepId,
    pub consequences: Vec<CommittedConsequence>,
    pub assumptions_invalidated: Vec<PlanAssumptionId>,
    pub recoveries_lost: Vec<RecoveryId>,
    pub new_recovery_requirements: Vec<RecoveryId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommittedConsequence {
    pub effect: EffectId,
    pub description: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitmentGate {
    pub commitment_point: PlanCommitmentPointId,
    pub required_assumptions: Vec<PlanAssumptionId>,
    pub required_invariants: Vec<PlanInvariantId>,
    pub required_approvals: Vec<ApprovalRequirement>,
    pub required_recoveries: Vec<RecoveryId>,
    pub diversity_claim: Option<ClaimId>,
    pub minimum_diversity: Option<crate::epistemic::DiversityClass>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentGateStatus {
    Open,
    Blocked,
    VerificationRequired,
    ApprovalRequired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitmentGateEvaluation {
    pub point: PlanCommitmentPointId,
    pub status: CommitmentGateStatus,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanFreshnessPolicy {
    pub default_max_age: Option<Duration>,
    pub critical_max_age: Option<Duration>,
    pub commitment_point_max_age: Option<Duration>,
}
impl Default for PlanFreshnessPolicy {
    fn default() -> Self {
        Self {
            default_max_age: Some(Duration::from_secs(1800)),
            critical_max_age: Some(Duration::from_secs(300)),
            commitment_point_max_age: Some(Duration::from_secs(60)),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanKnowledgeDependency {
    pub knowledge: KnowledgeRevisionRef,
    pub dependent_steps: Vec<PlanStepId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanState {
    pub plan: ExecutionPlanId,
    pub revision: u64,
    pub completed_steps: Vec<PlanStepId>,
    pub current_step: Option<PlanStepId>,
    pub observations: Vec<StateClaim>,
    pub assumption_states: BTreeMap<PlanAssumptionId, AssumptionValidity>,
    pub invariant_states: BTreeMap<PlanInvariantId, InvariantEvaluation>,
    pub crossed_commitment_points: Vec<PlanCommitmentPointId>,
    #[serde(default)]
    pub committed_effects: Vec<EffectId>,
    #[serde(default)]
    pub external_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub mutation_epoch: u64,
    #[serde(default)]
    pub observation_epochs: BTreeMap<String, u64>,
    #[serde(default)]
    pub reached_checkpoints: Vec<PlanCheckpointId>,
    #[serde(default)]
    pub authorizations: Vec<CommitAuthorizationId>,
    #[serde(default)]
    pub nested_commitments: BTreeMap<CompositionId, crate::composition::CompositionCommitState>,
}
impl PlanState {
    pub fn initial(p: &ExecutionPlan) -> Self {
        Self {
            plan: p.id.clone(),
            revision: p.revision,
            completed_steps: vec![],
            current_step: p
                .steps
                .iter()
                .find(|s| s.dependencies.is_empty())
                .map(|s| s.id.clone()),
            observations: vec![],
            assumption_states: BTreeMap::new(),
            invariant_states: BTreeMap::new(),
            crossed_commitment_points: vec![],
            committed_effects: vec![],
            external_versions: BTreeMap::new(),
            mutation_epoch: 0,
            observation_epochs: BTreeMap::new(),
            reached_checkpoints: vec![],
            authorizations: vec![],
            nested_commitments: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanValidityAssessment {
    pub id: PlanAssessmentId,
    pub plan: ExecutionPlanId,
    pub revision: u64,
    pub status: PlanValidityStatus,
    pub assumptions: Vec<AssumptionAssessment>,
    pub invariants: Vec<InvariantEvaluation>,
    pub checkpoints: Vec<CheckpointEvaluation>,
    pub gates: Vec<CommitmentGateEvaluation>,
    pub next_step: Option<PlanStepId>,
    pub blockers: Vec<PlanValidityBlocker>,
    pub recommendations: Vec<PlanValidityRecommendation>,
    pub reasons: Vec<String>,
    pub state_hash: String,
    pub assessed_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanValidityStatus {
    Valid,
    ValidWithWarnings,
    VerificationRequired,
    ReplanRequired,
    RecoveryRequired,
    Invalid,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanValidityBlocker {
    ContradictedAssumption,
    StaleCriticalAssumption,
    UnknownCriticalAssumption,
    ViolatedInvariant,
    MissingCapability,
    MissingRecovery,
    InvalidatedKnowledge,
    CommitmentPointConflict,
    ExternalStateDrift,
    KnowledgeConflict,
    MissingDependency,
    ApprovalRequired,
    InsufficientDiversity,
    UnsupportedStep,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PlanValidityRecommendation {
    Continue,
    Verify(ObservationSpec),
    Replan,
    Recover(RecoveryId),
    RunExperiment(ExperimentTemplateRef),
    RequireApproval,
    Abstain,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssumptionAssessment {
    pub assumption: PlanAssumptionId,
    pub validity: AssumptionValidity,
    pub required_for_next: bool,
    pub affected_steps: Vec<PlanStepId>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvariantEvaluation {
    pub invariant: PlanInvariantId,
    pub satisfied: Option<bool>,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Satisfied,
    VerificationRequired,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointEvaluation {
    pub checkpoint: PlanCheckpointId,
    pub status: CheckpointStatus,
    pub missing: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssumptionDrift {
    pub assumption: PlanAssumptionId,
    pub previous: StateClaim,
    pub current: Option<StateClaim>,
    pub drift: AssumptionDriftKind,
    pub detected_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionDriftKind {
    ValueChanged,
    EvidenceStale,
    SourceChanged,
    ConfidenceReduced,
    Contradicted,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssumptionImpact {
    pub assumption: PlanAssumptionId,
    pub affected_steps: Vec<PlanStepId>,
    pub affected_invariants: Vec<PlanInvariantId>,
    pub affected_recoveries: Vec<RecoveryId>,
    pub severity: Severity,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlanRevalidationSet {
    pub assumptions: Vec<PlanAssumptionId>,
    pub steps: Vec<PlanStepId>,
    pub invariants: Vec<PlanInvariantId>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlanDependencyIndex {
    pub assumptions_by_step: BTreeMap<PlanStepId, Vec<PlanAssumptionId>>,
    pub steps_by_assumption: BTreeMap<PlanAssumptionId, Vec<PlanStepId>>,
    pub invariants_by_step: BTreeMap<PlanStepId, Vec<PlanInvariantId>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanObservation {
    pub id: PlanObservationId,
    pub source: StateClaimSource,
    pub claims: Vec<StateClaim>,
    pub captured_at: DateTime<Utc>,
    pub attestation: Option<ExecutionAttestationId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevisionRef {
    pub plan: ExecutionPlanId,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRevisionRelation {
    pub parent: PlanRevisionRef,
    pub child: PlanRevisionRef,
    pub reason: PlanRevisionReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRevisionReason {
    AssumptionDrift,
    InvariantViolation,
    KnowledgeChange,
    FailedStep,
    Forecast,
    UserChange,
    ApprovalChange,
    ExternalStateDrift,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanReplanEvent {
    pub from: PlanRevisionRef,
    pub to: PlanRevisionRef,
    pub trigger: PlanRevisionReason,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRuntimeContext {
    pub run: PlanRunId,
    pub plan: ExecutionPlanId,
    pub revision: u64,
    pub next_step: PlanStepId,
    pub validity: Option<PlanValidityAssessment>,
    pub crossed_commitments: Vec<PlanCommitmentPointId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanCheckpointSnapshot {
    pub id: PlanCheckpointSnapshotId,
    pub checkpoint: PlanCheckpointId,
    pub plan_revision: u64,
    pub state_claims: Vec<StateClaim>,
    pub knowledge_snapshot: Option<KnowledgeSnapshotId>,
    pub effect_state: Vec<EffectId>,
    pub state: PlanState,
    pub captured_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRecoveryContext {
    pub plan_state: PlanState,
    pub failure_step: Option<PlanStepId>,
    pub committed_effects: Vec<EffectId>,
    pub crossed_commitments: Vec<PlanCommitmentPointId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRecoveryOutcome {
    RestoredToSafeState,
    ReplannedFromCurrentState,
    Compensated,
    DegradedButStable,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRun {
    #[serde(default)]
    pub trajectory: Option<TrajectoryId>,
    pub id: PlanRunId,
    pub plan: PlanRevisionRef,
    pub state: PlanState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: Option<PlanRunOutcome>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRunOutcome {
    Success,
    Failure,
    AppropriateReplan,
    UnnecessaryReplan,
    Abandoned,
    Inconclusive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStepRun {
    #[serde(default)]
    pub composition_evidence: Option<CompositionEvidenceId>,
    #[serde(default)]
    pub experiment: Option<ExperimentId>,
    pub run: PlanRunId,
    pub step: PlanStepId,
    pub decision: RuntimeDecisionId,
    pub attestation: Option<ExecutionAttestationId>,
    pub receipts: Vec<CommitReceiptId>,
    pub completed_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Default)]
pub struct PlanEvaluationInputs {
    pub now: Option<DateTime<Utc>>,
    pub valid_approvals: BTreeSet<String>,
    pub available_recoveries: BTreeSet<RecoveryId>,
    pub changed_knowledge: BTreeSet<PlanStepId>,
    pub invalid_components: BTreeSet<PlanStepId>,
    pub low_diversity: BTreeSet<PlanCommitmentPointId>,
    pub nested_blockers: BTreeMap<PlanStepId, Vec<String>>,
    pub nested_commitment_steps: BTreeSet<PlanStepId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanExperimentBudget {
    pub max_runs: usize,
    pub max_steps: usize,
    pub max_duration: Duration,
    pub max_external_effect_risk: crate::effects::EffectRisk,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanDriftExperiment {
    pub plan: PlanRevisionRef,
    pub drift: Option<AssumptionDrift>,
    pub request: crate::experimentation::ExperimentRequest,
}
