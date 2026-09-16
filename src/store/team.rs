// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{Error, Result, core::*, team::*};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
impl Store {
    /// Local administrative operation. Never exposed as an agent self-assignment API.
    pub fn save_agent_team(&self, team: &AgentTeam) -> Result<()> {
        team.validate()?;
        let revision = i64::try_from(team.revision)
            .map_err(|_| Error::InvalidInput("Team revision overflow".into()))?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let previous: Option<i64> = tx
            .query_row(
                "SELECT revision FROM agent_teams WHERE id=?1",
                [team.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if previous.unwrap_or(0).checked_add(1) != Some(revision) {
            return Err(Error::InvalidInput(
                "Team revisions must advance exactly once".into(),
            ));
        }
        let data = serde_json::to_string(team)?;
        tx.execute(
            "INSERT INTO agent_team_revisions(id,revision,data) VALUES (?1,?2,?3)",
            params![team.id.to_string(), revision, data],
        )?;
        tx.execute("INSERT INTO agent_teams(id,revision,data) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,data=excluded.data", params![team.id.to_string(),revision,data])?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES (?1,'team_revision_recorded',?2)",
            params![team.id.to_string(), data],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn agent_team(&self, id: &AgentTeamId) -> Result<AgentTeam> {
        let data: String = self.connection.query_row(
            "SELECT data FROM agent_teams WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn agent_teams(&self) -> Result<Vec<AgentTeam>> {
        self.connection
            .prepare("SELECT data FROM agent_teams ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn team_delegations(
        &self,
        team: &AgentTeamId,
    ) -> Result<BTreeMap<DelegationId, Delegation>> {
        self.connection
            .prepare("SELECT data FROM agent_delegations WHERE team=?1 ORDER BY id")?
            .query_map([team.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| {
                let d: Delegation = serde_json::from_str(&r?)?;
                Ok((d.id.clone(), d))
            })
            .collect()
    }
    fn revoked_delegations(&self) -> Result<BTreeSet<DelegationId>> {
        self.connection
            .prepare("SELECT id FROM delegation_revocations")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| r?.parse())
            .collect()
    }
    pub fn record_delegation(&self, delegation: &Delegation) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let team = self.agent_team(&delegation.team)?;
        let mut all = self.team_delegations(&team.id)?;
        if all.contains_key(&delegation.id) {
            return Err(Error::InvalidInput("Delegation already exists".into()));
        }
        all.insert(delegation.id.clone(), delegation.clone());
        team.delegated_authority(
            &delegation.id,
            &all,
            &self.revoked_delegations()?,
            Utc::now(),
        )?;
        self.validate_delegation_plan(delegation)?;
        let data = serde_json::to_string(delegation)?;
        tx.execute(
            "INSERT INTO agent_delegations(id,team,data) VALUES (?1,?2,?3)",
            params![delegation.id.to_string(), team.id.to_string(), data],
        )?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES (?1,'delegation_recorded',?2)",
            params![team.id.to_string(), data],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn validate_delegation(
        &self,
        team: &AgentTeamId,
        id: &DelegationId,
        now: DateTime<Utc>,
    ) -> Result<RoleAuthority> {
        let all = self.team_delegations(team)?;
        let rights = self.agent_team(team)?.delegated_authority(
            id,
            &all,
            &self.revoked_delegations()?,
            now,
        )?;
        self.validate_delegation_plan(&all[id])?;
        Ok(rights)
    }
    pub fn revoke_delegation(&self, id: &DelegationId, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return Err(Error::InvalidInput("Revocation requires a reason".into()));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let team: String = tx.query_row(
            "SELECT team FROM agent_delegations WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO delegation_revocations(id,reason,created_at) VALUES (?1,?2,?3)",
            params![id.to_string(), reason, Utc::now().to_rfc3339()],
        )?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES (?1,'delegation_revoked',?2)",
            params![
                team,
                serde_json::to_string(&serde_json::json!({"delegation":id,"reason":reason}))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn team_history(&self, team: &AgentTeamId) -> Result<Vec<serde_json::Value>> {
        self.connection.prepare("SELECT kind,data,created_at FROM team_events WHERE team=?1 ORDER BY id")?.query_map([team.to_string()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.map(|r| {let (kind,data,created_at)=r?; Ok(serde_json::json!({"kind":kind,"data":serde_json::from_str::<serde_json::Value>(&data)?,"created_at":created_at}))}).collect()
    }
}

impl Store {
    /// Resolve grants from local records on every decision and publication. Caller claims
    /// about assessment are discarded. This adds restrictions; capability/effect policy
    /// still validates concrete resources and external authorization independently.
    pub fn attach_team_authority(
        &self,
        context: &mut crate::runtime::RuntimeDecisionContext,
    ) -> Result<()> {
        let Some(binding) = context.team.clone() else {
            if self
                .agent_teams()?
                .iter()
                .any(|t| t.members.iter().any(|m| m.session == context.session_id))
            {
                return Err(Error::InvalidInput(
                    "Registered team session requires explicit role binding".into(),
                ));
            }
            return Ok(());
        };
        use crate::bridge::protocol::NormalizedAction;
        let action = if context.proposed_effect.is_some() {
            RoleActionClass::Commit
        } else {
            match context.proposed_action {
                Some(NormalizedAction::FileRead { .. }) => RoleActionClass::Observe,
                _ => RoleActionClass::Execute,
            }
        };
        let mut review_assessment = None;
        let result = (|| -> Result<()> {
            let team = self.agent_team(&binding.team)?;
            if team.revision != binding.revision {
                return Err(Error::InvalidInput("Stale team revision".into()));
            }
            if !team.members.iter().any(|m| {
                m.id == binding.member
                    && m.session == context.session_id
                    && m.agent == context.agent
            }) {
                return Err(Error::InvalidInput(
                    "Runtime identity does not match team member".into(),
                ));
            }
            let scope_context = self.team_scope_context(context)?;
            let assignment = team
                .role_assignments
                .iter()
                .find(|a| a.id == binding.assignment)
                .ok_or_else(|| Error::InvalidInput("Unknown role assignment".into()))?;
            let rights = if let Some(id) = &binding.delegation {
                let all = self.team_delegations(&team.id)?;
                let rights =
                    team.delegated_authority(id, &all, &self.revoked_delegations()?, Utc::now())?;
                let d = &all[id];
                self.validate_delegation_plan(d)?;
                if let Some(pin) = &d.plan
                    && !context
                        .plan
                        .as_ref()
                        .is_some_and(|p| p.plan == pin.plan && p.revision == pin.revision)
                {
                    return Err(Error::InvalidInput(
                        "Delegation is bound to another plan revision".into(),
                    ));
                }
                if d.delegate != binding.member
                    || assignment.member != binding.member
                    || d.role != assignment.role
                {
                    return Err(Error::InvalidInput(
                        "Delegation receiver or role mismatch".into(),
                    ));
                }
                if crate::hierarchy::KnowledgeApplicabilityEvaluator::evaluate(
                    &crate::hierarchy::DeterministicApplicabilityEvaluator,
                    &d.task_scope,
                    &scope_context,
                )
                .status
                    != crate::hierarchy::ApplicabilityStatus::Applicable
                {
                    return Err(Error::InvalidInput(
                        "Delegated scope not established".into(),
                    ));
                }
                rights
                    .intersection(&team.assignment_authority(&binding.assignment, Utc::now())?)
                    .copied()
                    .collect()
            } else {
                team.assignment_authority(&binding.assignment, Utc::now())?
            };
            let mut outer = rights.clone();
            if !context.capability_context.commit_authority {
                outer.remove(&RoleActionClass::Commit);
            }
            if !context.capability_context.required_available
                || context.capability_context.governance.hard_policy_blocked
            {
                outer.clear();
            }
            let request = TeamAuthorityRequest {
                action,
                context: scope_context,
                runtime_grant: outer.clone(),
                external_grant: outer,
                now: Utc::now(),
            };
            team.authorize_assignment(
                &binding.assignment,
                &binding.member,
                &context.session_id,
                &request,
            )?;
            intersect_request(&rights, &request)?;
            if let Some(id) = &binding.review {
                let assessment = self.assess_team_review(id, context)?;
                let satisfied = assessment.status == ReviewGateStatus::Satisfied;
                review_assessment = Some(assessment);
                if !satisfied {
                    return Err(Error::InvalidInput(
                        "Team review gate is not satisfied".into(),
                    ));
                }
            } else if self.team_action_has_review(&team.id, context)?
                || (context.risk.severity >= crate::curriculum::Severity::High
                    && action != RoleActionClass::Observe)
            {
                return Err(Error::InvalidInput(
                    "This team action requires an evidence-backed review".into(),
                ));
            }
            Ok(())
        })();
        let assessment = TeamAuthorityAssessment {
            review: review_assessment,
            allowed: result.is_ok(),
            action,
            reasons: result
                .err()
                .map(|e| vec![e.to_string()])
                .unwrap_or_default(),
        };
        context.team.as_mut().unwrap().assessment = Some(assessment);
        Ok(())
    }
}

impl Store {
    fn validate_delegation_plan(&self, delegation: &Delegation) -> Result<()> {
        if let Some(pin) = &delegation.plan
            && self.execution_plan(&pin.plan)?.revision != pin.revision
        {
            return Err(Error::InvalidInput(
                "Delegation plan revision is stale".into(),
            ));
        }
        Ok(())
    }
}
