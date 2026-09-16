// SPDX-License-Identifier: Apache-2.0
use super::{EpistemicStore, Store};
use crate::{Error, Result, core::*, runtime::RuntimeDecisionContext, team::*};
use chrono::Utc;
use rusqlite::{Transaction, TransactionBehavior, params};
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
impl Store {
    /// Record a local, reference-only handoff. Does not deliver bytes over a network,
    /// transfer credentials, import claims as observations or create empirical evidence.
    pub fn create_agent_handoff(
        &self,
        input: &AgentHandoffRequest,
        context: &RuntimeDecisionContext,
    ) -> Result<AgentHandoff> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let review = self.team_review(&input.review)?;
        let team = self.agent_team(&review.team)?;
        let now = Utc::now();
        if matches!(
            input.classification,
            HandoffClassification::Sensitive | HandoffClassification::Restricted
        ) {
            return Err(invalid(
                "Sensitive handoff requires a separate explicit disclosure adapter",
            ));
        }
        let count = input.payload.claims.len()
            + input.payload.observations.len()
            + input.payload.contributions.len();
        if count == 0 || count > 128 {
            return Err(invalid("Handoff requires 1..128 structured references"));
        }
        if review.team_revision != team.revision
            || review.expires_at <= now
            || review.target.action_hash != review_action_hash(context)?
        {
            return Err(invalid(
                "Handoff review is stale or bound to another action",
            ));
        }
        let contributions = self.team_contributions(&review.id)?;
        let source = contributions
            .iter()
            .find(|c| c.id == input.from_contribution)
            .ok_or_else(|| invalid("Unknown source contribution"))?;
        let from = team
            .role_assignments
            .iter()
            .find(|a| a.id == source.assignment && a.member == source.member)
            .ok_or_else(|| invalid("Source assignment changed"))?;
        let to = team
            .role_assignments
            .iter()
            .find(|a| a.id == input.to_assignment)
            .ok_or_else(|| invalid("Unknown recipient assignment"))?;
        if !team.members.iter().any(|m| {
            m.id == source.member && m.session == context.session_id && m.agent == context.agent
        }) {
            return Err(invalid("Handoff sender is not authenticated"));
        }
        let scope = self.team_scope_context(context)?;
        for a in [from, to] {
            let m = team
                .members
                .iter()
                .find(|m| m.id == a.member)
                .ok_or_else(|| invalid("Unknown handoff member"))?;
            team.authorize_assignment(
                &a.id,
                &m.id,
                &m.session,
                &TeamAuthorityRequest {
                    action: RoleActionClass::Observe,
                    context: scope.clone(),
                    runtime_grant: [RoleActionClass::Observe].into(),
                    external_grant: [RoleActionClass::Observe].into(),
                    now,
                },
            )?;
        }
        let mut available = source.evidence_paths.clone();
        for id in &input.payload.contributions {
            let c = contributions
                .iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| invalid("Handoff contribution belongs to another review"))?;
            available.extend(c.evidence_paths.iter().cloned());
        }
        if !input.payload.observations.is_subset(&available) {
            return Err(invalid(
                "Handoff observations must retain a cited contribution's evidence paths",
            ));
        }
        for id in &input.payload.observations {
            if self.evidence_path(id)?.claim.id != review.target.claim {
                return Err(invalid("Handoff evidence is outside the reviewed claim"));
            }
        }
        if input
            .payload
            .claims
            .iter()
            .any(|id| id != &review.target.claim)
        {
            return Err(invalid("Handoff claim is outside review scope"));
        }
        if let Some(id) = &input.knowledge_snapshot {
            let snapshot = self.knowledge_snapshot(id)?;
            if !context.operational_knowledge.as_ref().is_some_and(|k| {
                k.snapshot.id == *id && k.snapshot.fingerprint == snapshot.fingerprint
            }) {
                return Err(invalid(
                    "Handoff knowledge snapshot is not bound to sender context",
                ));
            }
        }
        let mut handoff = AgentHandoff {
            request: input.clone(),
            team: team.id.clone(),
            team_revision: team.revision,
            from: from.member.clone(),
            to: to.member.clone(),
            from_role: from.role.clone(),
            to_role: to.role.clone(),
            created_at: now,
            expires_at: review.expires_at.min(from.valid_until).min(to.valid_until),
            content_hash: String::new(),
        };
        handoff.content_hash = handoff.hash()?;
        let data = serde_json::to_string(&handoff)?;
        tx.execute(
            "INSERT INTO agent_handoffs(id,team,review,data) VALUES(?1,?2,?3,?4)",
            params![
                input.id.to_string(),
                team.id.to_string(),
                review.id.to_string(),
                data
            ],
        )?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES(?1,'structured_handoff_recorded',?2)",
            params![team.id.to_string(), data],
        )?;
        tx.commit()?;
        Ok(handoff)
    }
    /// Local administrative history access. Delivery uses `receive_agent_handoff`.
    pub fn agent_handoff(&self, id: &AgentHandoffId) -> Result<AgentHandoff> {
        let data: String = self.connection.query_row(
            "SELECT data FROM agent_handoffs WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let h: AgentHandoff = serde_json::from_str(&data)?;
        h.verify()?;
        Ok(h)
    }
    pub fn receive_agent_handoff(
        &self,
        id: &AgentHandoffId,
        context: &RuntimeDecisionContext,
    ) -> Result<AgentHandoff> {
        let h = self.agent_handoff(id)?;
        let team = self.agent_team(&h.team)?;
        let review = self.team_review(&h.request.review)?;
        let now = Utc::now();
        if team.revision != h.team_revision
            || h.expires_at <= now
            || h.created_at > now
            || review.target.action_hash != review_action_hash(context)?
        {
            return Err(invalid(
                "Handoff expired, changed revision or targets another action",
            ));
        }
        if !team
            .members
            .iter()
            .any(|m| m.id == h.to && m.session == context.session_id && m.agent == context.agent)
        {
            return Err(invalid("Handoff recipient is not authenticated"));
        }
        team.authorize_assignment(
            &h.request.to_assignment,
            &h.to,
            &context.session_id,
            &TeamAuthorityRequest {
                action: RoleActionClass::Observe,
                context: self.team_scope_context(context)?,
                runtime_grant: [RoleActionClass::Observe].into(),
                external_grant: [RoleActionClass::Observe].into(),
                now,
            },
        )?;
        // Returning references is not an observation, delegation, approval or effect receipt.
        Ok(h)
    }
}
