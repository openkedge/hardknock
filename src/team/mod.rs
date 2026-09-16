// SPDX-License-Identifier: Apache-2.0
//! Bounded team governance. Role grants constrain authority; they never authorize effects.
use crate::{Error, Result, core::*, hierarchy::*};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleActionClass {
    Observe,
    Propose,
    Challenge,
    Recommend,
    Experiment,
    Prepare,
    Execute,
    Commit,
    Recover,
}
pub type RoleAuthority = BTreeSet<RoleActionClass>;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInAgentRole {
    Planner,
    Investigator,
    Reviewer,
    Executor,
    Recovery,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRole {
    pub id: AgentRoleId,
    pub name: String,
    pub purpose: String,
    pub allowed_actions: RoleAuthority,
    pub prohibited_actions: RoleAuthority,
}
impl AgentRole {
    pub fn builtin(kind: BuiltInAgentRole) -> Self {
        use RoleActionClass::*;
        let (name, actions) = match kind {
            BuiltInAgentRole::Planner => ("Planner", vec![Observe, Propose]),
            BuiltInAgentRole::Investigator => ("Investigator", vec![Observe, Experiment, Prepare]),
            BuiltInAgentRole::Reviewer => ("Reviewer", vec![Observe, Challenge, Recommend]),
            BuiltInAgentRole::Executor => ("Executor", vec![Observe, Execute, Prepare, Commit]),
            BuiltInAgentRole::Recovery => ("Recovery", vec![Observe, Recover]),
        };
        Self {
            id: AgentRoleId::new(),
            name: name.into(),
            purpose: format!("Bounded {name} responsibilities"),
            allowed_actions: actions.into_iter().collect(),
            prohibited_actions: BTreeSet::new(),
        }
    }
    pub fn authority(&self) -> RoleAuthority {
        self.allowed_actions
            .difference(&self.prohibited_actions)
            .copied()
            .collect()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentTeamMember {
    pub id: TeamMemberId,
    pub agent: AgentIdentity,
    /// Local authenticated session binding, never inferred from model name.
    pub session: HardknockSessionId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub id: RoleAssignmentId,
    pub member: TeamMemberId,
    pub role: AgentRoleId,
    pub scope: KnowledgeScope,
    pub valid_from: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentTeam {
    pub id: AgentTeamId,
    pub revision: u64,
    pub members: Vec<AgentTeamMember>,
    pub roles: Vec<AgentRole>,
    pub role_assignments: Vec<RoleAssignment>,
    pub authority: RoleAuthority,
    pub max_delegation_depth: usize,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delegation {
    pub id: DelegationId,
    pub team: AgentTeamId,
    pub team_revision: u64,
    pub delegator: TeamMemberId,
    pub delegate: TeamMemberId,
    pub source_assignment: RoleAssignmentId,
    pub parent: Option<DelegationId>,
    pub role: AgentRoleId,
    pub task_scope: KnowledgeScope,
    pub authority: RoleAuthority,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
fn narrows(a: &KnowledgeScope, b: &KnowledgeScope) -> bool {
    matches!(
        DeterministicScopeRelationEvaluator.compare(a, b),
        ScopeRelation::Equal | ScopeRelation::Narrower
    )
}
impl AgentTeam {
    pub fn validate(&self) -> Result<()> {
        if self.revision == 0
            || self.members.is_empty()
            || self.members.len() > 32
            || self.roles.is_empty()
            || self.roles.len() > 32
            || self.role_assignments.len() > 128
            || self.max_delegation_depth > 2
        {
            return Err(invalid(
                "Team requires 1..32 members/roles, at most 128 assignments and delegation depth <= 2",
            ));
        }
        let mut members = BTreeSet::new();
        let mut sessions = BTreeSet::new();
        for m in &self.members {
            if !members.insert(&m.id) || !sessions.insert(&m.session) {
                return Err(invalid("Duplicate member or authenticated session"));
            }
        }
        let mut roles = BTreeSet::new();
        for r in &self.roles {
            if !roles.insert(&r.id) || r.name.trim().is_empty() {
                return Err(invalid("Duplicate or unnamed role"));
            }
        }
        let mut assignments = BTreeSet::new();
        for a in &self.role_assignments {
            if !assignments.insert(&a.id)
                || !members.contains(&a.member)
                || !roles.contains(&a.role)
                || a.valid_from >= a.valid_until
            {
                return Err(invalid(
                    "Invalid role assignment identity or validity window",
                ));
            }
            // Unknown/custom containment cannot support an authority proof.
            if !narrows(&a.scope, &a.scope) {
                return Err(invalid("Role scope cannot be evaluated deterministically"));
            }
        }
        Ok(())
    }
    pub fn assignment_authority(
        &self,
        id: &RoleAssignmentId,
        now: DateTime<Utc>,
    ) -> Result<RoleAuthority> {
        self.validate()?;
        let a = self
            .role_assignments
            .iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| invalid("Unknown role assignment"))?;
        if now < a.valid_from || now >= a.valid_until {
            return Err(invalid("Role assignment expired or not yet valid"));
        }
        let role = self
            .roles
            .iter()
            .find(|r| r.id == a.role)
            .ok_or_else(|| invalid("Unknown role"))?;
        Ok(role
            .authority()
            .intersection(&self.authority)
            .copied()
            .collect())
    }
    /// Validate every ancestor at use time. Caller supplies persisted revocations and current revision.
    pub fn delegated_authority(
        &self,
        id: &DelegationId,
        delegations: &BTreeMap<DelegationId, Delegation>,
        revoked: &BTreeSet<DelegationId>,
        now: DateTime<Utc>,
    ) -> Result<RoleAuthority> {
        self.validate()?;
        let mut seen = BTreeSet::new();
        self.delegated_inner(id, delegations, revoked, now, &mut seen)
    }
    fn delegated_inner(
        &self,
        id: &DelegationId,
        all: &BTreeMap<DelegationId, Delegation>,
        revoked: &BTreeSet<DelegationId>,
        now: DateTime<Utc>,
        seen: &mut BTreeSet<DelegationId>,
    ) -> Result<RoleAuthority> {
        if !seen.insert(id.clone())
            || seen.len() > self.max_delegation_depth
            || revoked.contains(id)
        {
            return Err(invalid("Delegation revoked, cyclic, or too deep"));
        }
        let d = all.get(id).ok_or_else(|| invalid("Unknown delegation"))?;
        if &d.id != id
            || d.team != self.id
            || d.team_revision != self.revision
            || d.issued_at > now
            || d.expires_at <= now
            || d.issued_at >= d.expires_at
            || d.delegator == d.delegate
            || !self.members.iter().any(|m| m.id == d.delegate)
        {
            return Err(invalid(
                "Delegation identity, revision, or validity mismatch",
            ));
        }
        let a = self
            .role_assignments
            .iter()
            .find(|a| a.id == d.source_assignment)
            .ok_or_else(|| invalid("Unknown source assignment"))?;
        let source = if let Some(parent) = &d.parent {
            let rights = self.delegated_inner(parent, all, revoked, now, seen)?;
            let p = &all[parent];
            if p.delegate != d.delegator
                || p.source_assignment != d.source_assignment
                || d.expires_at > p.expires_at
                || d.issued_at < p.issued_at
                || !narrows(&d.task_scope, &p.task_scope)
            {
                return Err(invalid("Child delegation expands parent scope or lifetime"));
            }
            rights
        } else {
            if a.member != d.delegator
                || d.expires_at > a.valid_until
                || d.issued_at < a.valid_from
                || !narrows(&d.task_scope, &a.scope)
            {
                return Err(invalid("Delegation expands source assignment"));
            }
            self.assignment_authority(&a.id, now)?
        };
        let target = self
            .roles
            .iter()
            .find(|r| r.id == d.role)
            .ok_or_else(|| invalid("Unknown delegated role"))?
            .authority();
        if !d.authority.is_subset(&source)
            || !d.authority.is_subset(&target)
            || !d.authority.is_subset(&self.authority)
        {
            return Err(invalid("Delegation cannot invent authority"));
        }
        Ok(d.authority.clone())
    }
    /// Runtime and external grants are required independently, including for Commit.
    pub fn authorize_assignment(
        &self,
        id: &RoleAssignmentId,
        member: &TeamMemberId,
        session: &HardknockSessionId,
        request: &TeamAuthorityRequest,
    ) -> Result<RoleAuthority> {
        let rights = self.assignment_authority(id, request.now)?;
        let a = self
            .role_assignments
            .iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| invalid("Unknown assignment"))?;
        if &a.member != member
            || !self
                .members
                .iter()
                .any(|m| &m.id == member && &m.session == session)
        {
            return Err(invalid("Authenticated member mismatch"));
        }
        if DeterministicApplicabilityEvaluator
            .evaluate(&a.scope, &request.context)
            .status
            != ApplicabilityStatus::Applicable
        {
            return Err(invalid("Role scope not established"));
        }
        intersect_request(&rights, request)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamAuthorityRequest {
    pub action: RoleActionClass,
    pub context: KnowledgeContext,
    pub runtime_grant: RoleAuthority,
    pub external_grant: RoleAuthority,
    pub now: DateTime<Utc>,
}
/// Inputs must come from trusted runtime/governance adapters, not an agent claim.
pub fn intersect_request(
    rights: &RoleAuthority,
    request: &TeamAuthorityRequest,
) -> Result<RoleAuthority> {
    let effective: RoleAuthority = rights
        .intersection(&request.runtime_grant)
        .copied()
        .collect::<RoleAuthority>()
        .intersection(&request.external_grant)
        .copied()
        .collect();
    if !effective.contains(&request.action) {
        return Err(invalid("Action not granted by every authority boundary"));
    }
    Ok(effective)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamRuntimeContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<TeamReviewId>,
    pub team: AgentTeamId,
    pub revision: u64,
    pub member: TeamMemberId,
    pub assignment: RoleAssignmentId,
    pub delegation: Option<DelegationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<TeamAuthorityAssessment>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAuthorityAssessment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewGateAssessment>,
    pub allowed: bool,
    pub action: RoleActionClass,
    pub reasons: Vec<String>,
}

mod review;
pub use review::*;
