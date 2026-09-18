// SPDX-License-Identifier: Apache-2.0
use super::{CurriculumStore, EffectStore, EpistemicStore, RuntimeStore, Store};
use crate::{Error, Result, core::*, epistemic::*, runtime::RuntimeDecisionContext, team::*};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
impl Store {
    pub(crate) fn team_record<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        team: &AgentTeamId,
        value: &T,
    ) -> Result<()> {
        let data = serde_json::to_string(value)?;
        if data.len() > 1024 * 1024 {
            return Err(invalid("Team record exceeds 1 MiB"));
        }
        self.connection.execute(
            "INSERT INTO team_records(kind,id,team,data) VALUES(?1,?2,?3,?4)",
            params![kind, id, team.to_string(), data],
        )?;
        self.connection.execute(
            "INSERT INTO team_events(team,kind,data) VALUES(?1,?2,?3)",
            params![team.to_string(), kind, data],
        )?;
        Ok(())
    }
    pub fn team_records<T: DeserializeOwned>(
        &self,
        team: &AgentTeamId,
        kind: &str,
    ) -> Result<Vec<T>> {
        self.connection
            .prepare("SELECT data FROM team_records WHERE team=?1 AND kind=?2 ORDER BY rowid")?
            .query_map(params![team.to_string(), kind], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub(crate) fn team_record_by_id<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<T> {
        let data: String = self.connection.query_row(
            "SELECT data FROM team_records WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    /// Local governance only. One immutable policy per revision; changing it requires a new team revision.
    pub fn save_team_governance(&self, g: &TeamGovernance) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let t = self.agent_team(&g.team)?;
        if t.revision != g.revision
            || g.max_agent_runs > 32
            || g.max_tokens > 10_000_000
            || g.max_latency_ms > 86_400_000
            || g.separation.distinct_from > crate::curriculum::Severity::High
            || g.members.len() != t.members.len()
            || g.role_capabilities.len() != t.roles.len()
            || g.knowledge.len() != t.roles.len()
            || t.members.iter().any(|m| !g.members.contains_key(&m.id))
            || t.roles.iter().any(|r| {
                !g.role_capabilities.contains_key(&r.id) || !g.knowledge.contains_key(&r.id)
            })
        {
            return Err(invalid(
                "Governance requires complete current role/member coverage and bounded budgets",
            ));
        }
        for p in g.members.values() {
            if p.experience_profile.is_empty() || p.experience_profile.len() > 256 {
                return Err(invalid("Member profile must be bounded and named"));
            }
        }
        for p in g.knowledge.values() {
            if p.hidden_artifacts.len() > 128 || p.hidden_artifacts.iter().any(|s| s.len() > 256) {
                return Err(invalid("Knowledge policy exceeds bounds"));
            }
        }
        let data = serde_json::to_string(g)?;
        if data.len() > 262144 {
            return Err(invalid("Governance exceeds 256 KiB"));
        }
        let revision = i64::try_from(g.revision).map_err(|_| invalid("Team revision overflow"))?;
        tx.execute(
            "INSERT INTO team_governance(team,revision,data) VALUES(?1,?2,?3)",
            params![g.team.to_string(), revision, data],
        )?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES(?1,'team_governance_recorded',?2)",
            params![g.team.to_string(), data],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn team_governance(
        &self,
        id: &AgentTeamId,
        revision: u64,
    ) -> Result<Option<TeamGovernance>> {
        let revision = i64::try_from(revision).map_err(|_| invalid("Team revision overflow"))?;
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM team_governance WHERE team=?1 AND revision=?2",
                params![id.to_string(), revision],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
    pub(crate) fn validate_team_capabilities(
        &self,
        context: &RuntimeDecisionContext,
    ) -> Result<()> {
        let b = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Missing team context"))?;
        let t = self.agent_team(&b.team)?;
        let a = t
            .role_assignments
            .iter()
            .find(|a| a.id == b.assignment && a.member == b.member)
            .ok_or_else(|| invalid("Unknown assignment"))?;
        if let Some(g) = self.team_governance(&b.team, b.revision)? {
            let role = &g.role_capabilities[&a.role];
            let role_definition = t
                .roles
                .iter()
                .find(|role| role.id == a.role)
                .ok_or_else(|| invalid("Unknown role definition"))?;
            let member = &g.members[&b.member].capabilities;
            if context.capability_context.available.iter().any(|c| {
                !role.permits(c)
                    || !member.permits(c)
                    || (!role_definition.required_capabilities.allowed.is_empty()
                        && !role_definition.required_capabilities.permits(c))
            }) {
                return Err(invalid("Capability outside role/member envelope"));
            }
        } else {
            // Once configured, a team cannot drop restrictions by advancing its revision.
            let any: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM team_governance WHERE team=?1)",
                [b.team.to_string()],
                |r| r.get(0),
            )?;
            if any {
                return Err(invalid(
                    "New team revision requires governance revalidation",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn team_requires_separation(
        &self,
        id: &AgentTeamId,
        revision: u64,
        severity: crate::curriculum::Severity,
    ) -> Result<bool> {
        let threshold = self
            .team_governance(id, revision)?
            .map(|governance| governance.separation.distinct_from)
            .unwrap_or(crate::curriculum::Severity::High);
        Ok(severity >= threshold)
    }
    pub fn role_knowledge_view(
        &self,
        context: &RuntimeDecisionContext,
    ) -> Result<RoleKnowledgeView> {
        let b = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Knowledge view requires team context"))?;
        let t = self.agent_team(&b.team)?;
        if t.revision != b.revision
            || !t.members.iter().any(|m| {
                m.id == b.member && m.session == context.session_id && m.agent == context.agent
            })
        {
            return Err(invalid("Knowledge recipient identity/revision mismatch"));
        }
        let a = t
            .role_assignments
            .iter()
            .find(|a| a.id == b.assignment && a.member == b.member)
            .ok_or_else(|| invalid("Unknown knowledge recipient"))?;
        t.authorize_assignment(
            &a.id,
            &b.member,
            &context.session_id,
            &TeamAuthorityRequest {
                action: RoleActionClass::Observe,
                context: self.team_scope_context(context)?,
                runtime_grant: [RoleActionClass::Observe].into(),
                external_grant: [RoleActionClass::Observe].into(),
                now: Utc::now(),
            },
        )?;
        let exposure_rank = |mode| match mode {
            KnowledgeExposureMode::Full => 0,
            KnowledgeExposureMode::RoleScoped => 1,
            KnowledgeExposureMode::BlindChallenge => 2,
            KnowledgeExposureMode::Minimal => 3,
        };
        let mut policy = t
            .roles
            .iter()
            .find(|role| role.id == a.role)
            .map(|role| role.knowledge_policy.clone())
            .ok_or_else(|| invalid("Unknown knowledge role"))?;
        if let Some(governance_policy) = self
            .team_governance(&b.team, b.revision)?
            .and_then(|governance| governance.knowledge.get(&a.role).cloned())
        {
            policy
                .hidden_artifacts
                .extend(governance_policy.hidden_artifacts);
            policy.include_local_experience &= governance_policy.include_local_experience;
            policy.include_federated_experience &= governance_policy.include_federated_experience;
            policy.include_candidate_knowledge &= governance_policy.include_candidate_knowledge;
            if exposure_rank(governance_policy.mode) > exposure_rank(policy.mode) {
                policy.mode = governance_policy.mode;
            }
        }
        // Active challenges can only further restrict delivery, never relax role policy.
        for c in self
            .team_records::<ChallengeAssignment>(&b.team, "challenge_assigned")?
            .iter()
            .filter(|c| c.challenger == a.id && c.expires_at > Utc::now())
        {
            let r = self.team_review(&c.review)?;
            if r.team_revision == b.revision && r.target.action_hash == review_action_hash(context)?
            {
                policy
                    .hidden_artifacts
                    .extend(c.knowledge_policy.hidden_artifacts.clone());
                policy.include_local_experience &= c.knowledge_policy.include_local_experience;
                policy.include_federated_experience &=
                    c.knowledge_policy.include_federated_experience;
                policy.include_candidate_knowledge &=
                    c.knowledge_policy.include_candidate_knowledge;
                if exposure_rank(c.knowledge_policy.mode) > exposure_rank(policy.mode) {
                    policy.mode = c.knowledge_policy.mode;
                }
            }
        }
        let mut hidden = policy.hidden_artifacts.clone();
        let mut visible = BTreeSet::new();
        let mut fresh = context.clone();
        self.attach_runtime_knowledge(&mut fresh)?;
        let mut origins = BTreeMap::new();
        if let Some(knowledge) = &fresh.operational_knowledge {
            let snapshot = self.knowledge_snapshot(&knowledge.snapshot.id)?;
            for hierarchy in self.snapshot_hierarchies(&snapshot)? {
                for node in hierarchy.nodes.values() {
                    origins.insert(node.artifact.id.clone(), node.provenance.origin);
                }
            }
        }
        let mut bundle = fresh
            .operational_knowledge
            .as_ref()
            .map(|k| k.bundle.clone());
        let mut keep = |r: &crate::knowledge_runtime::ResolvedKnowledgeRef| {
            let id = &r.knowledge.artifact.id;
            let safety = matches!(
                r.knowledge.artifact.kind,
                crate::hierarchy::KnowledgeArtifactKind::Constraint
                    | crate::hierarchy::KnowledgeArtifactKind::AntiPattern
            );
            let origin_visible = origins.get(id).is_some_and(|origin| match origin {
                crate::abstraction::KnowledgeOrigin::Local => policy.include_local_experience,
                crate::abstraction::KnowledgeOrigin::FederatedAdvisory => {
                    policy.include_federated_experience
                }
                crate::abstraction::KnowledgeOrigin::CandidateProvider => {
                    policy.include_candidate_knowledge
                }
            });
            let retain = safety
                || (origin_visible
                    && policy.mode != KnowledgeExposureMode::Minimal
                    && !policy.hidden_artifacts.contains(id)
                    && !self.causal_artifact_quarantined(id).unwrap_or(true));
            if retain {
                hidden.remove(id);
                visible.insert(id.clone());
            } else {
                hidden.insert(id.clone());
            }
            retain
        };
        if let Some(k) = &mut bundle {
            k.primary_knowledge.retain(&mut keep);
            k.refinements.retain(&mut keep);
            k.exceptions.retain(&mut keep);
            // Never filter hard constraints or anti-pattern warnings, even if explicitly hidden.
            for r in k.constraints.iter().chain(k.antipatterns.iter()) {
                keep(r);
            }
            k.recoveries.retain(|r| keep(&r.knowledge));
            k.provenance = None; // full audit remains local; do not leak excluded bundle metadata.
        }
        let lessons = context
            .relevant_experience
            .lessons
            .iter()
            .filter(|l| {
                let id = l.lesson.id.to_string();
                let retain = policy.include_local_experience
                    && policy.mode != KnowledgeExposureMode::Minimal
                    && !policy.hidden_artifacts.contains(&id)
                    && !self.causal_artifact_quarantined(&id).unwrap_or(true);
                if retain {
                    visible.insert(id);
                } else {
                    hidden.insert(id);
                }
                retain
            })
            .cloned()
            .collect();
        let view = RoleKnowledgeView {
            team: b.team.clone(),
            revision: b.revision,
            member: b.member.clone(),
            role: a.role.clone(),
            mode: policy.mode,
            visible_artifacts: visible,
            hidden_artifacts: hidden,
            bundle,
            lessons,
            snapshot: fresh.operational_knowledge.map(|k| k.snapshot),
        };
        self.team_record(
            "knowledge_exposed",
            &uuid::Uuid::new_v4().to_string(),
            &b.team,
            &view,
        )?;
        Ok(view)
    }
    pub fn assign_responsibility(&self, r: &ResponsibilityAssignment) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let t = self.agent_team(&r.team)?;
        if t.revision != r.revision
            || !t
                .role_assignments
                .iter()
                .any(|a| a.id == r.assignment && a.member == r.owner && a.valid_until > Utc::now())
            || r.evidence.len() > 128
        {
            return Err(invalid("Responsibility owner/revision invalid"));
        }
        match &r.subject {
            ResponsibilitySubject::Plan { plan } => {
                self.plan_revision(&plan.plan, plan.revision)?;
            }
            ResponsibilitySubject::PlanStep { plan, step } => {
                if !self
                    .plan_revision(&plan.plan, plan.revision)?
                    .steps
                    .iter()
                    .any(|s| s.id == *step)
                {
                    return Err(invalid("Unknown plan step"));
                }
            }
            ResponsibilitySubject::CompositionStep {
                composition,
                revision,
                step,
            } => {
                if !self
                    .composition_revision(composition, *revision)?
                    .steps
                    .iter()
                    .any(|s| s.id == *step)
                {
                    return Err(invalid("Unknown composition step"));
                }
            }
            ResponsibilitySubject::Claim { id } => {
                self.claim(id)?;
            }
            ResponsibilitySubject::Experiment { id } => {
                self.experiment(id)?;
            }
            ResponsibilitySubject::Effect { id } => {
                self.effect(id)?;
            }
            ResponsibilitySubject::Recovery { id } => {
                self.recovery(id)?;
            }
            ResponsibilitySubject::Review { id } => {
                if self.team_review(id)?.team != r.team {
                    return Err(invalid("Review belongs to another team"));
                }
            }
        }
        for id in &r.evidence {
            self.evidence_path(id)?;
        }
        // One owner per subject at a revision. Completion is recorded as evidence, not silent reassignment.
        for old in
            self.team_records::<ResponsibilityAssignment>(&r.team, "responsibility_assigned")?
        {
            if old.revision == r.revision
                && serde_json::to_value(old.subject)? == serde_json::to_value(&r.subject)?
            {
                return Err(invalid("Subject already has an owner at this revision"));
            }
        }
        self.team_record("responsibility_assigned", &r.id.to_string(), &r.team, r)?;
        tx.commit()?;
        Ok(())
    }
    pub fn team_epistemic_profile(&self, id: &AgentTeamId) -> Result<TeamEpistemicProfile> {
        let t = self.agent_team(id)?;
        let g = self.team_governance(id, t.revision)?;
        let mut members: BTreeMap<TeamMemberId, BTreeSet<EvidencePathId>> = t
            .members
            .iter()
            .map(|m| (m.id.clone(), BTreeSet::new()))
            .collect();
        let reviews: Vec<TeamReview> = self
            .connection
            .prepare("SELECT data FROM team_reviews WHERE team=?1")?
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        let mut paths = BTreeMap::new();
        for r in reviews.iter().filter(|r| r.team_revision == t.revision) {
            for c in self.team_contributions(&r.id)? {
                if matches!(
                    c.contribution_type,
                    ContributionType::Proposal | ContributionType::Hypothesis
                ) {
                    continue;
                }
                for pid in c.evidence_paths {
                    if let Some(m) = members.get_mut(&c.member) {
                        m.insert(pid.clone());
                    }
                    paths.insert(pid.clone(), self.evidence_path(&pid)?);
                }
            }
        }
        let member_dependencies: BTreeMap<TeamMemberId, EpistemicDependencySet> = g
            .map(|g| {
                g.members
                    .into_iter()
                    .map(|(id, p)| (id, p.dependencies))
                    .collect()
            })
            .unwrap_or_default();
        let mut shared: BTreeMap<(EpistemicDependencyKind, String), BTreeSet<TeamMemberId>> =
            BTreeMap::new();
        for (m, ids) in &members {
            for id in ids {
                for d in dependency_values(&paths[id]) {
                    shared
                        .entry((d.kind, d.value))
                        .or_default()
                        .insert(m.clone());
                }
            }
        }
        // Declared member dependencies are risk metadata, never evidence paths or diversity credit.
        for (member, d) in &member_dependencies {
            let synthetic = EvidencePath {
                id: EvidencePathId::new(),
                claim: ClaimId::new().into(),
                source: EvidenceSource::Agent {
                    identity: t
                        .members
                        .iter()
                        .find(|candidate| candidate.id == *member)
                        .expect("governance member was validated")
                        .agent
                        .clone(),
                },
                context: Default::default(),
                dependencies: d.clone(),
                evidence_refs: vec![],
                outcome: EvidenceOutcome::Inconclusive,
                created_at: Utc::now(),
            };
            for v in dependency_values(&synthetic) {
                shared
                    .entry((v.kind, v.value))
                    .or_default()
                    .insert(member.clone());
            }
        }
        let claims = paths
            .values()
            .map(|p| p.claim.id.clone())
            .collect::<BTreeSet<_>>();
        let common_mode_risks = shared
            .into_iter()
            .filter(|(_, m)| m.len() > 1)
            .map(|((kind, value), m)| TeamCommonModeRisk {
                shared_dependencies: vec![DependencyValue { kind, value }],
                affected_members: m,
                affected_claims: claims.clone(),
                severity: if matches!(
                    kind,
                    EpistemicDependencyKind::Experience
                        | EpistemicDependencyKind::Evaluator
                        | EpistemicDependencyKind::ExternalEvidence
                ) {
                    CommonModeRiskClass::High
                } else {
                    CommonModeRiskClass::Moderate
                },
            })
            .collect();
        let paths = paths.into_values().collect::<Vec<_>>();
        Ok(TeamEpistemicProfile {
            team: id.clone(),
            revision: t.revision,
            member_dependencies,
            member_paths: members,
            diversity: DeterministicEvidenceDiversityPolicy.assess(&paths),
            fault_domains: fault_domains(&paths),
            common_mode_risks,
        })
    }
    /// Evaluate only; this API never spawns an agent or grants capabilities.
    pub fn assess_team_formation(
        &self,
        id: &AgentTeamId,
        roles: &BTreeSet<AgentRoleId>,
        minimum: DiversityClass,
    ) -> Result<TeamFormationAssessment> {
        let t = self.agent_team(id)?;
        let profile = self.team_epistemic_profile(id)?;
        let missing_roles = roles
            .iter()
            .filter(|r| {
                !t.role_assignments.iter().any(|a| {
                    a.role == **r && a.valid_from <= Utc::now() && a.valid_until > Utc::now()
                })
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let g = self.team_governance(id, t.revision)?;
        let (status, recommendations, runs) = if !missing_roles.is_empty() {
            (
                TeamFormationStatus::Unsuitable,
                vec!["Assign missing roles with scoped authority".into()],
                0,
            )
        } else if profile.diversity.diversity_class.satisfies(minimum) {
            (
                TeamFormationStatus::Suitable,
                vec!["Existing evidence is sufficient; do not add redundant agent runs".into()],
                0,
            )
        } else {
            (TeamFormationStatus::AdditionalDiversityRecommended,vec!["Prefer a bounded blind challenge or controlled alternative evaluator over another correlated opinion".into()],usize::from(g.is_some_and(|g|g.max_agent_runs>0)))
        };
        Ok(TeamFormationAssessment {
            team: id.clone(),
            missing_roles,
            profile,
            status,
            recommendations,
            additional_agent_runs: runs,
        })
    }
}
impl Store {
    pub fn assign_team_challenge(
        &self,
        input: &ChallengeAssignment,
    ) -> Result<ChallengeAssignment> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let r = self.team_review(&input.review)?;
        let t = self.agent_team(&r.team)?;
        let g = self
            .team_governance(&r.team, t.revision)?
            .ok_or_else(|| invalid("Challenge requires explicit team budgets and profiles"))?;
        let now = Utc::now();
        let a = t
            .role_assignments
            .iter()
            .find(|a| a.id == input.challenger)
            .ok_or_else(|| invalid("Unknown challenger"))?;
        if t.revision != r.team_revision
            || input.expires_at <= now
            || input.expires_at > r.expires_at
            || input.expires_at > a.valid_until
            || input.max_tokens == 0
            || input.max_tokens > g.max_tokens
            || input.max_latency_ms == 0
            || input.max_latency_ms > g.max_latency_ms
            || !t
                .assignment_authority(&a.id, now)?
                .iter()
                .any(|c| matches!(c, RoleActionClass::Challenge | RoleActionClass::Experiment))
        {
            return Err(invalid(
                "Challenge identity, scope, lifetime or budget invalid",
            ));
        }
        let previous = self.team_records::<ChallengeAssignment>(&r.team, "challenge_assigned")?;
        let current = previous
            .iter()
            .filter(|c| {
                self.team_review(&c.review)
                    .is_ok_and(|r| r.team_revision == t.revision)
            })
            .collect::<Vec<_>>();
        if current.len() >= g.max_agent_runs
            || current
                .iter()
                .map(|c| c.max_tokens)
                .sum::<u64>()
                .saturating_add(input.max_tokens)
                > g.max_tokens
            || current
                .iter()
                .map(|c| c.max_latency_ms)
                .sum::<u64>()
                .saturating_add(input.max_latency_ms)
                > g.max_latency_ms
        {
            return Err(invalid("Team challenge budget exhausted"));
        }
        if input.strategy == ChallengeStrategy::RemoveDominantExperience
            && (input.knowledge_policy.mode != KnowledgeExposureMode::BlindChallenge
                || input.knowledge_policy.hidden_artifacts.is_empty())
        {
            return Err(invalid("Blind challenge must name hidden experience"));
        }
        let mut c = input.clone();
        c.created_at = now;
        c.baseline_paths = self
            .evidence_paths(&r.target.claim)?
            .iter()
            .map(|p| p.id.clone())
            .collect();
        self.team_record("challenge_assigned", &c.id.to_string(), &r.team, &c)?;
        tx.commit()?;
        Ok(c)
    }
    pub fn complete_team_challenge(
        &self,
        id: &ChallengeAssignmentId,
        contribution: &AgentContributionId,
        context: &RuntimeDecisionContext,
        tokens: u64,
        latency_ms: u64,
    ) -> Result<ChallengeCompletion> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let c: ChallengeAssignment =
            self.team_record_by_id("challenge_assigned", &id.to_string())?;
        let r = self.team_review(&c.review)?;
        let t = self.agent_team(&r.team)?;
        let b = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Challenge completion requires authenticated team"))?;
        let v = self
            .team_contributions(&c.review)?
            .into_iter()
            .find(|v| v.id == *contribution)
            .ok_or_else(|| invalid("Unknown challenge contribution"))?;
        if t.revision != r.team_revision
            || c.expires_at <= Utc::now()
            || c.created_at > v.created_at
            || v.assignment != c.challenger
            || b.assignment != c.challenger
            || b.team != r.team
            || b.revision != t.revision
            || b.member != v.member
            || !t.members.iter().any(|m| {
                m.id == v.member && m.session == context.session_id && m.agent == context.agent
            })
            || review_action_hash(context)? != r.target.action_hash
            || tokens > c.max_tokens
            || latency_ms > c.max_latency_ms
            || !matches!(
                v.contribution_type,
                ContributionType::Challenge
                    | ContributionType::ExperimentResult
                    | ContributionType::Review
            )
        {
            return Err(invalid(
                "Challenge completion identity, provenance, lifetime or budget invalid",
            ));
        }
        t.assignment_authority(&c.challenger, Utc::now())?;
        let baseline = c
            .baseline_paths
            .iter()
            .map(|id| self.evidence_path(id))
            .collect::<Result<Vec<_>>>()?;
        let mut paths = Vec::new();
        for id in v.evidence_paths.difference(&c.baseline_paths) {
            let p = self.evidence_path(id)?;
            if p.created_at >= c.created_at && p.claim.id == r.target.claim {
                paths.push(p);
            }
        }
        if paths.is_empty() {
            return Err(invalid(
                "Challenge must add new canonical evidence; opinions and relays do not count",
            ));
        }
        let controlled = paths.iter().any(|p| match &p.source {
            EvidenceSource::Experiment { experiment_id } => self
                .strategy_experiment(experiment_id)
                .is_ok_and(|e| e.result.is_some() && e.request.candidates.len() >= 2),
            _ => false,
        });
        if (c.require_controlled_evidence
            || matches!(
                c.strategy,
                ChallengeStrategy::CounterfactualExperiment
                    | ChallengeStrategy::AdversarialPerturbation
            ))
            && !controlled
        {
            return Err(invalid(
                "Challenge requires a recorded controlled experiment",
            ));
        }
        let base_values = baseline
            .iter()
            .flat_map(dependency_values)
            .map(|d| (d.kind, d.value))
            .collect::<BTreeSet<_>>();
        let new_values = paths
            .iter()
            .flat_map(dependency_values)
            .map(|d| (d.kind, d.value))
            .collect::<BTreeSet<_>>();
        let dimension = match c.strategy {
            ChallengeStrategy::AlternativeAgent => Some(EpistemicDependencyKind::Model),
            ChallengeStrategy::AlternativeEvaluator => Some(EpistemicDependencyKind::Evaluator),
            ChallengeStrategy::AlternativeTool => Some(EpistemicDependencyKind::Tool),
            ChallengeStrategy::AlternativeEnvironment => Some(EpistemicDependencyKind::Environment),
            _ => None,
        };
        if dimension
            .is_some_and(|kind| !new_values.difference(&base_values).any(|(k, _)| *k == kind))
        {
            return Err(invalid(
                "Challenge did not change the requested dependency dimension",
            ));
        }
        if c.strategy == ChallengeStrategy::RemoveDominantExperience {
            let views = self.team_records::<RoleKnowledgeView>(&r.team, "knowledge_exposed")?;
            if !views.iter().any(|v| {
                v.revision == t.revision
                    && v.member == b.member
                    && v.mode == KnowledgeExposureMode::BlindChallenge
                    && c.knowledge_policy
                        .hidden_artifacts
                        .is_subset(&v.hidden_artifacts)
                    && c.knowledge_policy
                        .hidden_artifacts
                        .is_disjoint(&v.visible_artifacts)
            }) || paths.iter().any(|p| {
                p.dependencies
                    .originating_lessons
                    .iter()
                    .any(|l| c.knowledge_policy.hidden_artifacts.contains(&l.to_string()))
            }) {
                return Err(invalid(
                    "Blind challenge has no uncontaminated delivered knowledge view",
                ));
            }
        }
        let old_roots = baseline
            .iter()
            .flat_map(|p| p.context.root_evidence_origins.iter().cloned())
            .collect::<BTreeSet<_>>();
        let roots = paths
            .iter()
            .flat_map(|p| p.context.root_evidence_origins.iter().cloned())
            .collect::<BTreeSet<_>>();
        let result = ChallengeCompletion {
            challenge: id.clone(),
            contribution: contribution.clone(),
            new_paths: paths.iter().map(|p| p.id.clone()).collect(),
            new_roots: roots.difference(&old_roots).cloned().collect(),
            new_evaluators: new_values
                .difference(&base_values)
                .filter(|(k, _)| *k == EpistemicDependencyKind::Evaluator)
                .map(|(_, v)| v.clone())
                .collect(),
            contradictions: paths
                .iter()
                .filter(|p| p.outcome == EvidenceOutcome::Contradicts)
                .count(),
            tokens,
            latency_ms,
            completed_at: Utc::now(),
        };
        self.team_record("challenge_completed", &id.to_string(), &r.team, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn reassign_team_role(&self, r: &RoleReassignment) -> Result<AgentTeam> {
        // Savepoint keeps team revision, governance, and audit atomic through existing transactions.
        self.connection
            .execute_batch("SAVEPOINT team_reassignment")?;
        let result = (|| {
            let mut t = self.agent_team(&r.team)?;
            if t.revision != r.revision
                || r.from == r.to
                || r.evidence.is_empty()
                || r.evidence.len() > 128
                || !t.members.iter().any(|m| m.id == r.to)
            {
                return Err(invalid(
                    "Reassignment needs current revision, distinct member and evidence",
                ));
            }
            let mut g = self
                .team_governance(&t.id, t.revision)?
                .ok_or_else(|| invalid("Reassignment requires explicit capability profiles"))?;
            let snapshot = self.knowledge_snapshot(&r.snapshot.id)?;
            if snapshot.fingerprint != r.snapshot.fingerprint {
                return Err(invalid("Reassignment snapshot mismatch"));
            }
            for id in &r.evidence {
                self.evidence_path(id)?;
            }
            let a = t
                .role_assignments
                .iter_mut()
                .find(|a| a.id == r.assignment && a.member == r.from && a.valid_until > Utc::now())
                .ok_or_else(|| invalid("Reassignment source mismatch"))?;
            let target = &g.members[&r.to].capabilities;
            let role = &g.role_capabilities[&a.role];
            if role.allowed.iter().any(|p| !target.allowed.contains(p)) {
                return Err(invalid("Replacement member lacks role capability envelope"));
            }
            a.member = r.to.clone();
            t.revision += 1;
            g.revision = t.revision;
            // Direct SQL here avoids nested BEGIN from save_agent_team; same validation and revision check.
            t.validate()?;
            let data = serde_json::to_string(&t)?;
            let new_revision =
                i64::try_from(t.revision).map_err(|_| invalid("Team revision overflow"))?;
            let old_revision =
                i64::try_from(r.revision).map_err(|_| invalid("Team revision overflow"))?;
            self.connection.execute(
                "INSERT INTO agent_team_revisions(id,revision,data) VALUES(?1,?2,?3)",
                params![t.id.to_string(), new_revision, data],
            )?;
            let updated = self.connection.execute(
                "UPDATE agent_teams SET revision=?2,data=?3 WHERE id=?1 AND revision=?4",
                params![t.id.to_string(), new_revision, data, old_revision],
            )?;
            if updated != 1 {
                return Err(Error::Intervention(
                    "Team changed during reassignment".into(),
                ));
            }
            self.connection.execute(
                "INSERT INTO team_governance(team,revision,data) VALUES(?1,?2,?3)",
                params![t.id.to_string(), new_revision, serde_json::to_string(&g)?],
            )?;
            self.team_record("role_reassigned", &r.id.to_string(), &r.team, r)?;
            Ok(t)
        })();
        match result {
            Ok(t) => {
                self.connection.execute_batch("RELEASE team_reassignment")?;
                Ok(t)
            }
            Err(e) => {
                self.connection
                    .execute_batch("ROLLBACK TO team_reassignment; RELEASE team_reassignment")?;
                Err(e)
            }
        }
    }
    pub fn record_team_recovery_handoff(
        &self,
        r: &TeamRecoveryHandoff,
        context: &RuntimeDecisionContext,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let h = self.agent_handoff(&r.handoff)?;
        let t = self.agent_team(&h.team)?;
        let b = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Recovery handoff requires team"))?;
        if h.team_revision != t.revision
            || h.expires_at <= Utc::now()
            || b.member != h.from
            || b.team != h.team
            || !t.members.iter().any(|m| {
                m.id == h.from && m.session == context.session_id && m.agent == context.agent
            })
            || r.failure_evidence.is_empty()
            || !r
                .failure_evidence
                .is_subset(&h.request.payload.observations)
            || r.action_hash.len() != 64
        {
            return Err(invalid(
                "Recovery handoff must preserve authenticated failure provenance",
            ));
        }
        let a = t
            .role_assignments
            .iter()
            .find(|a| a.id == h.request.to_assignment)
            .ok_or_else(|| invalid("Missing recovery role"))?;
        if !t
            .assignment_authority(&a.id, Utc::now())?
            .contains(&RoleActionClass::Recover)
        {
            return Err(invalid("Recipient has no recovery authority"));
        }
        if self.recovery(&r.recovery)?.status != crate::resilience::RecoveryStatus::Validated {
            return Err(invalid("Recovery must be validated"));
        }
        let run = self.plan_run(&r.run)?;
        for effect in &r.effects {
            if !run.state.committed_effects.contains(effect) {
                return Err(invalid(
                    "Recovery effect is not in the plan's committed state",
                ));
            }
            self.commit_receipt_for_effect(effect)?
                .ok_or_else(|| invalid("Missing effect receipt"))?;
        }
        self.team_record("recovery_handoff_created", &r.id.to_string(), &h.team, r)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn team_recovery_action(&self, context: &RuntimeDecisionContext) -> Result<bool> {
        let Some(b) = &context.team else {
            return Ok(false);
        };
        for r in self.team_records::<TeamRecoveryHandoff>(&b.team, "recovery_handoff_created")? {
            let h = self.agent_handoff(&r.handoff)?;
            if h.to == b.member
                && h.request.to_assignment == b.assignment
                && h.team_revision == b.revision
                && h.expires_at > Utc::now()
                && r.action_hash == review_action_hash(context)?
                && self.recovery(&r.recovery)?.status
                    == crate::resilience::RecoveryStatus::Validated
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Re-resolve team authority at commit time and preserve the verified actor context.
    pub fn bind_effect_actor(
        &self,
        effect: &crate::effects::Effect,
    ) -> Result<Option<EffectActorContext>> {
        self.effect_actor_from_runtime(effect, true)
    }
    pub(crate) fn historical_effect_actor(
        &self,
        effect: &crate::effects::Effect,
    ) -> Result<Option<EffectActorContext>> {
        self.effect_actor_from_runtime(effect, false)
    }
    fn effect_actor_from_runtime(
        &self,
        effect: &crate::effects::Effect,
        revalidate: bool,
    ) -> Result<Option<EffectActorContext>> {
        let Some(record) = self.runtime_decisions()?.into_iter().rev().find(|r| {
            r.session_id.to_string() == effect.session_id
                && r.context.knowledge_action_id.as_deref() == Some(&effect.source_action.id)
        }) else {
            return Ok(None);
        };
        let Some(binding) = record.context.team.clone() else {
            return Ok(None);
        };
        let mut checked = record.context.clone();
        if revalidate {
            self.attach_team_authority(&mut checked)?;
        }
        if !checked
            .team
            .as_ref()
            .and_then(|team| team.assessment.as_ref())
            .is_some_and(|assessment| assessment.allowed)
        {
            return Err(Error::Intervention(
                "Team actor is not authorized at effect commit".into(),
            ));
        }
        if revalidate
            && serde_json::to_value(&checked.team)? != serde_json::to_value(&record.context.team)?
        {
            return Err(Error::Intervention(
                "Team authority changed before effect commit".into(),
            ));
        }
        let team = self.agent_team(&binding.team)?;
        let assignment = team
            .role_assignments
            .iter()
            .find(|a| a.id == binding.assignment && a.member == binding.member)
            .ok_or_else(|| invalid("Effect actor assignment changed"))?;
        let mut chain = Vec::new();
        let mut cursor = binding.delegation.clone();
        let delegations = self.team_delegations(&binding.team)?;
        while let Some(id) = cursor {
            if chain.len() >= team.max_delegation_depth {
                return Err(invalid("Effect delegation chain exceeds bound"));
            }
            let d = delegations
                .get(&id)
                .ok_or_else(|| invalid("Effect delegation disappeared"))?;
            chain.push(id);
            cursor = d.parent.clone();
        }
        let snapshot = record
            .context
            .operational_knowledge
            .as_ref()
            .map(|k| k.snapshot.clone());
        let actor = EffectActorContext {
            agent: record.context.agent.clone(),
            role: assignment.role.clone(),
            member: binding.member,
            team: binding.team.clone(),
            delegation_chain: chain,
            runtime_decision: record.id.clone(),
            plan: record
                .context
                .plan
                .as_ref()
                .map(|p| crate::plan::PlanRevisionRef {
                    plan: p.plan.clone(),
                    revision: p.revision,
                }),
            step: record.context.plan.as_ref().map(|p| p.next_step.clone()),
            knowledge_snapshot: snapshot,
            review: binding.review,
            authority_source: "local runtime intersection plus external commit authorization"
                .into(),
        };
        Ok(Some(actor))
    }
    pub fn effect_actor(&self, id: &EffectId) -> Result<Option<EffectActorContext>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM effect_actor_contexts WHERE effect_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
    }
    pub(crate) fn validate_composition_responsibility(
        &self,
        context: &RuntimeDecisionContext,
        composition: &crate::composition::CompositionRuntimeContext,
    ) -> Result<()> {
        let Some(binding) = &context.team else {
            let assigned=self.agent_teams()?.into_iter().any(|team|self.team_records::<ResponsibilityAssignment>(&team.id,"responsibility_assigned").is_ok_and(|records|records.into_iter().any(|r|matches!(r.subject,ResponsibilitySubject::CompositionStep{composition:ref id,revision,step:ref subject_step} if id==&composition.composition && revision==composition.revision && subject_step==&composition.step))));
            return if assigned {
                Err(invalid("Composition step has an explicit team owner"))
            } else {
                Ok(())
            };
        };
        for r in
            self.team_records::<ResponsibilityAssignment>(&binding.team, "responsibility_assigned")?
        {
            if r.revision == binding.revision
                && matches!(r.subject,ResponsibilitySubject::CompositionStep{composition:ref id,revision,step:ref subject_step} if id==&composition.composition && revision==composition.revision && subject_step==&composition.step)
            {
                if r.assignment != binding.assignment || r.owner != binding.member {
                    return Err(invalid(
                        "Composition responsibility does not match acting role/member",
                    ));
                }
                return Ok(());
            }
        }
        Ok(())
    }
    pub fn assess_team_assurance(
        &self,
        profile: TeamAssuranceProfile,
        context: &RuntimeDecisionContext,
    ) -> Result<TeamAssuranceAssessment> {
        let mut checked = context.clone();
        self.attach_team_authority(&mut checked)?;
        let binding = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Team assurance requires runtime team context"))?;
        let team = self.agent_team(&binding.team)?;
        let governance = self.team_governance(&team.id, team.revision)?;
        let epistemic = self.team_epistemic_profile(&team.id)?;
        let handoffs: Vec<AgentHandoff> = self
            .connection
            .prepare("SELECT data FROM agent_handoffs WHERE team=?1")?
            .query_map([team.id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        let dependencies = governance.as_ref().is_some_and(|g| {
            g.members
                .values()
                .all(|member| member.dependencies != EpistemicDependencySet::default())
        });
        let capability_scoped = governance.as_ref().is_some_and(|g| {
            g.role_capabilities
                .values()
                .all(|envelope| !envelope.allowed.is_empty())
        });
        let high = self.team_requires_separation(&team.id, team.revision, context.risk.severity)?;
        let review = if high || binding.review.is_some() {
            binding
                .review
                .as_ref()
                .map(|id| self.assess_team_review(id, context))
                .transpose()?
        } else {
            None
        };
        let review_satisfied = if high {
            review
                .as_ref()
                .is_some_and(|assessment| assessment.status == ReviewGateStatus::Satisfied)
        } else {
            review
                .as_ref()
                .is_none_or(|assessment| assessment.status == ReviewGateStatus::Satisfied)
        };
        let runtime_authority = checked
            .team
            .as_ref()
            .and_then(|team| team.assessment.as_ref())
            .is_some_and(|assessment| assessment.allowed);
        let challenge_completed = !self
            .team_records::<ChallengeCompletion>(&team.id, "challenge_completed")?
            .is_empty();
        let mut requirements = BTreeMap::from([
            (
                "current_explicit_role_assignments".into(),
                team.revision == binding.revision
                    && team
                        .role_assignments
                        .iter()
                        .any(|a| a.id == binding.assignment && a.member == binding.member),
            ),
            ("runtime_authority_satisfied".into(), runtime_authority),
            ("scoped_capability_envelopes".into(), capability_scoped),
            ("high_risk_review_separation".into(), review_satisfied),
            (
                "handoff_provenance_integrity".into(),
                handoffs.iter().all(|handoff| handoff.verify().is_ok()),
            ),
            ("epistemic_dependencies_recorded".into(), dependencies),
        ]);
        let mut evidence = review
            .as_ref()
            .map(|a| a.evidence.clone())
            .unwrap_or_default();
        if profile == TeamAssuranceProfile::TeamEpistemicDiversityV1 {
            requirements.insert(
                "moderate_or_higher_evidence_diversity".into(),
                epistemic.diversity.diversity_class >= DiversityClass::Moderate,
            );
            requirements.insert("bounded_challenge_completed".into(), challenge_completed);
            if let Some(a) = review {
                evidence.extend(a.evidence);
            }
        }
        let status = if requirements.values().all(|v| *v) {
            TeamAssuranceStatus::Satisfied
        } else {
            TeamAssuranceStatus::Blocked
        };
        Ok(TeamAssuranceAssessment {
            team: team.id,
            revision: team.revision,
            profile,
            status,
            requirements,
            evidence_paths: evidence,
            scope: format!(
                "runtime action {} at team revision {}",
                review_action_hash(context)?,
                binding.revision
            ),
        })
    }
    /// Repeated violations can suggest a governance change; this never changes OpenKedge or runtime policy.
    pub fn team_guard_recommendations(
        &self,
        id: &AgentTeamId,
    ) -> Result<Vec<TeamGuardRecommendation>> {
        let mut counts: BTreeMap<(RoleAssignmentId, RoleActionClass), usize> = BTreeMap::new();
        for violation in self.team_records::<RoleViolation>(id, "role_violation_attempted")? {
            *counts
                .entry((violation.role, violation.attempted_action))
                .or_default() += 1;
        }
        Ok(counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|((assignment, action), count)| TeamGuardRecommendation {
                team: id.clone(),
                assignment,
                action,
                supporting_violations: count,
                statement: "Consider a versioned external guard after independent validation"
                    .into(),
                automatic_promotion: false,
            })
            .collect())
    }
    pub fn team_learning_opportunity(
        &self,
        id: &AgentTeamId,
        kind: crate::economics::ExperienceOpportunityKind,
        severity: crate::curriculum::Severity,
    ) -> Result<crate::economics::ExperienceOpportunity> {
        use crate::economics::*;
        if !matches!(
            kind,
            ExperienceOpportunityKind::IncreaseTeamEvidenceDiversity
                | ExperienceOpportunityKind::ValidateDelegationBoundary
                | ExperienceOpportunityKind::ChallengeTeamCommonModeRisk
                | ExperienceOpportunityKind::ValidateRoleSeparation
        ) {
            return Err(invalid("Expected a team learning opportunity kind"));
        }
        let team = self.agent_team(id)?;
        let governance = self
            .team_governance(id, team.revision)?
            .ok_or_else(|| invalid("Team economics requires explicit budgets"))?;
        let profile = self.team_epistemic_profile(id)?;
        let useful = kind != ExperienceOpportunityKind::IncreaseTeamEvidenceDiversity
            || profile.diversity.diversity_class < DiversityClass::Moderate;
        let gap = ExperienceGap {
            kind,
            target: ExperienceOpportunityTarget::RuntimeGap(id.to_string()),
            reasons: vec![OpportunityReason::Custom(
                "Bounded team authority or epistemic validation".into(),
            )],
            severity,
            exposure: ExposureBand::Rare,
            mitigation_gap: if useful {
                MitigationGap::Significant
            } else {
                MitigationGap::None
            },
            learning: LearningValueEstimate {
                band: if useful {
                    ValueBand::High
                } else {
                    ValueBand::Low
                },
                possible_outcomes: vec![LearningOutcomeClass::ChangeRuntimeDecision],
                decision_changing_outcomes: usize::from(useful),
                rationale: vec![if useful {
                    "Expected to test a decision-relevant boundary".into()
                } else {
                    "Existing diversity makes another agent run redundant".into()
                }],
            },
            decision_relevance: DecisionRelevance {
                affected_decisions: 1,
                affected_task_families: 1,
                current_runtime_use: RuntimeUseBand::Low,
                likely_decision_change: if useful {
                    ValueBand::High
                } else {
                    ValueBand::Low
                },
            },
            reuse: ReusePotential::Reusable,
            novelty: EvidenceNovelty::ContextExtension,
            evidence: EvidenceSummary::default(),
            estimated_cost: ExperimentCost {
                agent_runs: usize::from(useful).min(governance.max_agent_runs),
                ..Default::default()
            },
            risk: OpportunityRisk {
                trial_safety: crate::curriculum::TrialSafety::RequiresIsolation,
                external_effect_risk: crate::effects::EffectRisk::ReadOnly,
                isolation_required: crate::runtime::ExperimentCapabilitySummary::default()
                    .requirements,
                approval_required: false,
            },
            dependencies: vec![],
        };
        let opportunity = DeterministicExperienceOpportunityGenerator
            .generate(&ExperiencePlanningContext {
                gaps: vec![gap],
                completed_dependencies: Default::default(),
                objective: ExperiencePortfolioObjective::Balanced,
                now: Utc::now(),
            })?
            .pop()
            .ok_or_else(|| invalid("No team opportunity generated"))?;
        self.save_experience_opportunity(&opportunity)?;
        Ok(opportunity)
    }
    pub fn compile_team_curriculum(
        &self,
        id: &AgentTeamId,
        skill: &str,
        requests: &[crate::experimentation::ExperimentRequest],
        kind: crate::curriculum::CurriculumGoalKind,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<crate::curriculum::Curriculum> {
        use crate::curriculum::*;
        if requests.is_empty()
            || !matches!(
                kind,
                CurriculumGoalKind::TestRoleBoundary
                    | CurriculumGoalKind::TestDelegationBoundary
                    | CurriculumGoalKind::TestCrossAgentHandoff
                    | CurriculumGoalKind::ChallengeSharedExperience
                    | CurriculumGoalKind::TestTeamRecovery
            )
        {
            return Err(invalid(
                "Team curriculum requires concrete requests and a team goal",
            ));
        }
        let team = self.agent_team(id)?;
        let governance = self
            .team_governance(id, team.revision)?
            .ok_or_else(|| invalid("Team curriculum requires explicit budgets"))?;
        if requests.len() > governance.max_agent_runs {
            return Err(invalid("Team curriculum exceeds agent-run budget"));
        }
        let skill = self.skill(skill)?;
        let goal = CurriculumGoal {
            id: CurriculumGoalId::new(),
            kind,
            description: "Validate one bounded team boundary under controlled conditions".into(),
            priority: Priority::High,
            score: PriorityScore {
                score: 80,
                priority: Priority::High,
                explanation: "Team authority or common-mode evidence gap".into(),
            },
            evidence_gap: EvidenceGap {
                dimension: "team-role-evidence-authority".into(),
                known_values: vec![],
                unknown_values: vec!["boundary behavior".into()],
                rationale:
                    "Role names or agreement do not establish authority or evidence diversity"
                        .into(),
            },
            status: GoalStatus::Planned,
            decision: CurriculumDecision::Approved,
            reason: "Bounded isolated validation".into(),
            severity: Severity::High,
            safety: TrialSafety::RequiresIsolation,
        };
        let trials = requests
            .iter()
            .map(|request| {
                if request.candidates.iter().any(|candidate| {
                    !matches!(
                        candidate.execution,
                        crate::experimentation::CandidateExecution::Shell { .. }
                    )
                }) {
                    return Err(invalid(
                        "Team curriculum accepts only isolated local command candidates",
                    ));
                }
                let fingerprint = blake3::hash(&serde_json::to_vec(request)?)
                    .to_hex()
                    .to_string();
                Ok(CurriculumTrial {
                    id: CurriculumTrialId::new(),
                    goal_id: goal.id.clone(),
                    skill_id: skill.id.clone(),
                    condition: format!("team:{}:{}", team.id, team.revision),
                    fingerprint,
                    intent: TrialIntent::Revalidation,
                    execution: TrialExecution::Experiment {
                        request: Box::new(request.clone()),
                    },
                    result: None,
                    learning_outcome: None,
                    status: GoalStatus::Planned,
                    estimated_budget: crate::budget::ExperienceUsage {
                        realities: request.candidates.len(),
                        agent_runs: 1,
                        ..Default::default()
                    },
                    expected_value:
                        "Measure boundary enforcement or new evidence without production effects"
                            .into(),
                    required_isolation: RealityCapabilities::default(),
                    round: 1,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let now = Utc::now();
        let curriculum = Curriculum {
            id: CurriculumId::new(),
            target: CurriculumTarget::Skill(skill.id),
            profile: "team-boundaries-v1".into(),
            goals: vec![goal],
            trials,
            budget: budget.clone(),
            usage: Default::default(),
            reserved: Default::default(),
            trials_executed: 0,
            status: CurriculumStatus::Planned,
            created_at: now,
            updated_at: now,
            rounds: 0,
            max_rounds: 1,
            revision: 1,
            before: vec![],
            after: vec![],
            stop_reason: None,
            session_id: None,
            quality: CurriculumQuality::Medium,
        };
        let config = crate::bridge::config::Config::load(&self.home)?;
        crate::curriculum::CurriculumExecutor {
            store: self,
            config: &config,
        }
        .validate(&curriculum)?;
        CurriculumStore::insert(self, &curriculum)?;
        self.team_record(
            "team_curriculum_created",
            &curriculum.id.to_string(),
            id,
            &curriculum.id,
        )?;
        Ok(curriculum)
    }
}
