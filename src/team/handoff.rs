// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, core::*};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffClassification {
    Public,
    Operational,
    Sensitive,
    Restricted,
}
/// References only: no raw prompts, scratchpads, credential values or arbitrary blobs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredHandoff {
    pub claims: BTreeSet<ClaimId>,
    pub observations: BTreeSet<EvidencePathId>,
    pub contributions: BTreeSet<AgentContributionId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHandoffRequest {
    pub id: AgentHandoffId,
    pub review: TeamReviewId,
    pub from_contribution: AgentContributionId,
    pub to_assignment: RoleAssignmentId,
    pub payload: StructuredHandoff,
    pub classification: HandoffClassification,
    pub knowledge_snapshot: Option<KnowledgeSnapshotId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHandoff {
    pub request: AgentHandoffRequest,
    pub team: AgentTeamId,
    pub team_revision: u64,
    pub from: TeamMemberId,
    pub to: TeamMemberId,
    pub from_role: AgentRoleId,
    pub to_role: AgentRoleId,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub content_hash: String,
}
impl AgentHandoff {
    pub fn hash(&self) -> Result<String> {
        let mut copy = self.clone();
        copy.content_hash.clear();
        Ok(blake3::hash(&serde_json::to_vec(&copy)?)
            .to_hex()
            .to_string())
    }
    pub fn verify(&self) -> Result<()> {
        if self.content_hash != self.hash()? {
            return Err(Error::InvalidInput("Handoff integrity mismatch".into()));
        }
        Ok(())
    }
}
