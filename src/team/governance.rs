// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{assurance::CapabilityEnvelope, epistemic::*, knowledge_runtime::KnowledgeSnapshotRef};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeExposureMode {
    #[default]
    Full,
    RoleScoped,
    BlindChallenge,
    Minimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleKnowledgePolicy {
    pub include_local_experience: bool,
    pub include_federated_experience: bool,
    pub include_candidate_knowledge: bool,
    pub hidden_artifacts: BTreeSet<String>,
    pub mode: KnowledgeExposureMode,
}
impl Default for RoleKnowledgePolicy {
    fn default() -> Self {
        Self {
            include_local_experience: true,
            include_federated_experience: false,
            include_candidate_knowledge: false,
            hidden_artifacts: BTreeSet::new(),
            mode: KnowledgeExposureMode::Full,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamMemberProfile {
    pub experience_profile: String,
    pub dependencies: EpistemicDependencySet,
    /// Envelope has no credential material. Concrete grants still come from runtime.
    pub capabilities: CapabilityEnvelope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleSeparationPolicy {
    /// Can strengthen the default High boundary, never relax Critical/High separation.
    pub distinct_from: crate::curriculum::Severity,
}
impl Default for RoleSeparationPolicy {
    fn default() -> Self {
        Self {
            distinct_from: crate::curriculum::Severity::High,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamGovernance {
    pub team: AgentTeamId,
    pub revision: u64,
    pub members: BTreeMap<TeamMemberId, TeamMemberProfile>,
    pub role_capabilities: BTreeMap<AgentRoleId, CapabilityEnvelope>,
    pub knowledge: BTreeMap<AgentRoleId, RoleKnowledgePolicy>,
    pub separation: RoleSeparationPolicy,
    pub max_agent_runs: usize,
    pub max_tokens: u64,
    pub max_latency_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleKnowledgeView {
    pub team: AgentTeamId,
    pub revision: u64,
    pub member: TeamMemberId,
    pub role: AgentRoleId,
    pub mode: KnowledgeExposureMode,
    pub visible_artifacts: BTreeSet<String>,
    pub hidden_artifacts: BTreeSet<String>,
    pub bundle: Option<crate::knowledge_runtime::HierarchyContextBundle>,
    pub lessons: Vec<crate::retrieval::RetrievedLesson>,
    pub snapshot: Option<KnowledgeSnapshotRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponsibilitySubject {
    Plan {
        plan: crate::plan::PlanRevisionRef,
    },
    PlanStep {
        plan: crate::plan::PlanRevisionRef,
        step: PlanStepId,
    },
    CompositionStep {
        composition: CompositionId,
        revision: u64,
        step: CompositionStepId,
    },
    Claim {
        id: ClaimId,
    },
    Experiment {
        id: ExperimentId,
    },
    Effect {
        id: EffectId,
    },
    Recovery {
        id: RecoveryId,
    },
    Review {
        id: TeamReviewId,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsibilityStatus {
    Assigned,
    Completed,
    Blocked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponsibilityAssignment {
    pub id: ResponsibilityAssignmentId,
    pub team: AgentTeamId,
    pub revision: u64,
    pub subject: ResponsibilitySubject,
    pub assignment: RoleAssignmentId,
    pub owner: TeamMemberId,
    pub status: ResponsibilityStatus,
    pub evidence: BTreeSet<EvidencePathId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamEpistemicProfile {
    pub team: AgentTeamId,
    pub revision: u64,
    pub member_dependencies: BTreeMap<TeamMemberId, EpistemicDependencySet>,
    pub member_paths: BTreeMap<TeamMemberId, BTreeSet<EvidencePathId>>,
    pub diversity: EvidenceDiversityAssessment,
    pub fault_domains: Vec<EpistemicFaultDomain>,
    pub common_mode_risks: Vec<TeamCommonModeRisk>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommonModeRiskClass {
    Low,
    Moderate,
    High,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamCommonModeRisk {
    pub shared_dependencies: Vec<DependencyValue>,
    pub affected_members: BTreeSet<TeamMemberId>,
    pub affected_claims: BTreeSet<ClaimId>,
    pub severity: CommonModeRiskClass,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamFormationStatus {
    Suitable,
    SuitableWithWarnings,
    AdditionalDiversityRecommended,
    AuthorityConflict,
    Unsuitable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamFormationAssessment {
    pub team: AgentTeamId,
    pub missing_roles: BTreeSet<AgentRoleId>,
    pub profile: TeamEpistemicProfile,
    pub status: TeamFormationStatus,
    pub recommendations: Vec<String>,
    pub additional_agent_runs: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChallengeAssignment {
    pub id: ChallengeAssignmentId,
    pub review: TeamReviewId,
    pub challenger: RoleAssignmentId,
    pub strategy: ChallengeStrategy,
    pub knowledge_policy: RoleKnowledgePolicy,
    pub require_controlled_evidence: bool,
    pub baseline_paths: BTreeSet<EvidencePathId>,
    pub max_tokens: u64,
    pub max_latency_ms: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChallengeCompletion {
    pub challenge: ChallengeAssignmentId,
    pub contribution: AgentContributionId,
    pub new_paths: BTreeSet<EvidencePathId>,
    pub new_roots: BTreeSet<String>,
    pub new_evaluators: BTreeSet<String>,
    pub contradictions: usize,
    pub tokens: u64,
    pub latency_ms: u64,
    pub completed_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleReassignmentReason {
    AgentUnavailable,
    CapabilityMismatch,
    ConflictOfInterest,
    DiversityRequirement,
    RecoveryTransition,
    UserRequested,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleReassignment {
    pub id: RoleReassignmentId,
    pub team: AgentTeamId,
    pub revision: u64,
    pub assignment: RoleAssignmentId,
    pub from: TeamMemberId,
    pub to: TeamMemberId,
    pub reason: RoleReassignmentReason,
    pub evidence: BTreeSet<EvidencePathId>,
    pub snapshot: KnowledgeSnapshotRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamRecoveryHandoff {
    pub id: TeamRecoveryHandoffId,
    pub handoff: AgentHandoffId,
    pub recovery: RecoveryId,
    pub run: PlanRunId,
    pub failure_evidence: BTreeSet<EvidencePathId>,
    pub effects: BTreeSet<EffectId>,
    pub action_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectActorContext {
    pub agent: AgentIdentity,
    pub role: AgentRoleId,
    pub member: TeamMemberId,
    pub team: AgentTeamId,
    pub delegation_chain: Vec<DelegationId>,
    pub runtime_decision: RuntimeDecisionId,
    pub plan: Option<crate::plan::PlanRevisionRef>,
    pub step: Option<PlanStepId>,
    pub knowledge_snapshot: Option<KnowledgeSnapshotRef>,
    pub review: Option<TeamReviewId>,
    pub authority_source: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleViolationReason {
    ActionOutsideRole,
    CapabilityOutsideRole,
    DelegationExpired,
    DelegationScopeMismatch,
    SelfApprovalDisallowed,
    MissingReview,
    HandoffPolicyViolation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleViolation {
    pub team: AgentTeamId,
    pub member: TeamMemberId,
    pub role: RoleAssignmentId,
    pub attempted_action: RoleActionClass,
    pub reasons: Vec<String>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeamAssuranceProfile {
    TeamAssuranceBasicV1,
    TeamEpistemicDiversityV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamAssuranceStatus {
    Satisfied,
    Blocked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamAssuranceAssessment {
    pub team: AgentTeamId,
    pub revision: u64,
    pub profile: TeamAssuranceProfile,
    pub status: TeamAssuranceStatus,
    pub requirements: BTreeMap<String, bool>,
    pub evidence_paths: BTreeSet<EvidencePathId>,
    pub scope: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamGuardRecommendation {
    pub team: AgentTeamId,
    pub assignment: RoleAssignmentId,
    pub action: RoleActionClass,
    pub supporting_violations: usize,
    pub statement: String,
    pub automatic_promotion: bool,
}
