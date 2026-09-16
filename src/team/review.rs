// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, epistemic::*, runtime::RuntimeDecisionContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Exact action identity, including plan run/revision/step and composition revision.
/// Assessments are excluded to avoid a circular dependency on review results.
pub fn review_action_hash(context: &RuntimeDecisionContext) -> Result<String> {
    let payload = serde_json::json!({
        "action":context.proposed_action,"effect":context.proposed_effect,
        "plan":context.plan.as_ref().map(|p| (&p.run,&p.plan,p.revision,&p.next_step)),
        "composition":context.composition.as_ref().map(|c| (&c.composition,c.revision,&c.step)),
    });
    Ok(blake3::hash(&serde_json::to_vec(&payload)?)
        .to_hex()
        .to_string())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewTarget {
    pub claim: ClaimId,
    pub action_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamReview {
    pub id: TeamReviewId,
    pub team: AgentTeamId,
    pub team_revision: u64,
    pub target: ReviewTarget,
    pub proposer: TeamMemberId,
    pub executor: TeamMemberId,
    pub required_roles: BTreeSet<AgentRoleId>,
    pub minimum_diversity: DiversityClass,
    pub max_evidence_age_seconds: u32,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionType {
    Proposal,
    Observation,
    Hypothesis,
    Challenge,
    ExperimentResult,
    Review,
    Execution,
    Recovery,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentContribution {
    pub id: AgentContributionId,
    pub review: TeamReviewId,
    pub member: TeamMemberId,
    pub assignment: RoleAssignmentId,
    pub contribution_type: ContributionType,
    pub statement: String,
    /// References to canonical evidence, never new paths manufactured by a handoff.
    pub evidence_paths: BTreeSet<EvidencePathId>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFindingKind {
    UnsupportedAssumption,
    MissingEvidence,
    Contradiction,
    ConstraintViolation,
    ScopeMismatch,
    AuthorityMismatch,
    RecoveryGap,
    EpistemicDependency,
    NoIssueFound,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: ReviewFindingId,
    pub contribution: AgentContributionId,
    pub kind: ReviewFindingKind,
    pub statement: String,
    pub evidence_paths: BTreeSet<EvidencePathId>,
}
/// Explicit local administrative disposition, not a reviewer-created approval.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewFindingResolution {
    pub finding: ReviewFindingId,
    pub reason: String,
    pub evidence_paths: BTreeSet<EvidencePathId>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewGateStatus {
    Satisfied,
    ReviewRequired,
    EvidenceRequired,
    Blocked,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewGateAssessment {
    pub review: TeamReviewId,
    pub status: ReviewGateStatus,
    pub reasons: Vec<String>,
    pub evidence: BTreeSet<EvidencePathId>,
    pub diversity: EvidenceDiversityAssessment,
    pub fused_status: FusedEvidenceStatus,
    /// Binds the immutable review, contributions, findings, dispositions and paths.
    pub evidence_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamEvidenceAssessment {
    pub review: TeamReviewId,
    pub contributions: Vec<AgentContributionId>,
    pub fused: FusedEvidenceAssessment,
    pub fault_domains: Vec<EpistemicFaultDomain>,
    pub echo: EvidenceEchoAssessment,
}
