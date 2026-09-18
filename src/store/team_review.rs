// SPDX-License-Identifier: Apache-2.0
use super::{EpistemicStore, Store};
use crate::{
    Error, Result, core::*, epistemic::*, hierarchy::*, runtime::RuntimeDecisionContext, team::*,
};
use chrono::{Duration, Utc};
use rusqlite::{Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
impl Store {
    /// Local administrative declaration. A review is a restriction, never an effect approval.
    pub fn create_team_review(&self, input: &TeamReview) -> Result<TeamReview> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let t = self.agent_team(&input.team)?;
        t.validate()?;
        let now = Utc::now();
        if input.max_evidence_age_seconds == 0
            || input.max_evidence_age_seconds > 86400
            || input.team_revision != t.revision
            || input.expires_at <= now
            || input.expires_at > now + Duration::hours(24)
            || input.required_roles.is_empty()
            || input.required_roles.len() > 32
            || !t.members.iter().any(|m| m.id == input.proposer)
            || !t.members.iter().any(|m| m.id == input.executor)
            || input.target.action_hash.len() != 64
            || !input
                .target
                .action_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid(
                "Review requires current team, known members, bounded lifetime and exact action hash",
            ));
        }
        self.claim(&input.target.claim)?;
        for id in &input.required_roles {
            if !t
                .roles
                .iter()
                .any(|r| &r.id == id && r.authority().contains(&RoleActionClass::Challenge))
            {
                return Err(invalid("Required review role must permit challenge"));
            }
        }
        let mut review = input.clone();
        review.created_at = now;
        let data = serde_json::to_string(&review)?;
        tx.execute(
            "INSERT INTO team_reviews(id,team,action_hash,data) VALUES(?1,?2,?3,?4)",
            params![
                review.id.to_string(),
                t.id.to_string(),
                review.target.action_hash,
                data
            ],
        )?;
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES(?1,'review_created',?2)",
            params![t.id.to_string(), data],
        )?;
        tx.commit()?;
        Ok(review)
    }
    pub fn team_review(&self, id: &TeamReviewId) -> Result<TeamReview> {
        let data: String = self.connection.query_row(
            "SELECT data FROM team_reviews WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn team_contributions(&self, id: &TeamReviewId) -> Result<Vec<AgentContribution>> {
        self.connection
            .prepare("SELECT data FROM agent_contributions WHERE review=?1 ORDER BY id")?
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn review_findings(&self, id: &TeamReviewId) -> Result<Vec<ReviewFinding>> {
        self.connection
            .prepare("SELECT data FROM review_findings WHERE review=?1 ORDER BY id")?
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub(crate) fn team_scope_context(
        &self,
        context: &RuntimeDecisionContext,
    ) -> Result<KnowledgeContext> {
        let mut observed = context.clone();
        for (key, values) in self.runtime_context_observations(&context.session_id)? {
            observed
                .context_observations
                .entry(key)
                .or_default()
                .extend(values);
        }
        Ok(crate::knowledge_runtime::DefaultKnowledgeContextBuilder
            .trusted(&observed)
            .context)
    }
    /// `context` must be bound by the local runtime/adapter to the authenticated session.
    /// Referencing someone else's evidence preserves that path's source; it creates no new path.
    pub fn record_team_contribution(
        &self,
        input: &AgentContribution,
        findings: &[ReviewFinding],
        context: &RuntimeDecisionContext,
    ) -> Result<AgentContribution> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let review = self.team_review(&input.review)?;
        let team = self.agent_team(&review.team)?;
        let now = Utc::now();
        if review.team_revision != team.revision
            || review.created_at > now
            || review.expires_at <= now
            || review.target.action_hash != review_action_hash(context)?
        {
            return Err(invalid(
                "Contribution targets stale review or a different action",
            ));
        }
        if input.statement.trim().is_empty()
            || input.statement.len() > 4096
            || input.evidence_paths.len() > 128
            || findings.len() > 32
        {
            return Err(invalid(
                "Contribution exceeds bounded structured output limits",
            ));
        }
        if !team.members.iter().any(|m| {
            m.id == input.member && m.session == context.session_id && m.agent == context.agent
        }) {
            return Err(invalid("Contribution identity is not authenticated"));
        }
        let action = match input.contribution_type {
            ContributionType::Proposal | ContributionType::Hypothesis => RoleActionClass::Propose,
            ContributionType::Observation => RoleActionClass::Observe,
            ContributionType::Challenge | ContributionType::Review => RoleActionClass::Challenge,
            ContributionType::ExperimentResult => RoleActionClass::Experiment,
            ContributionType::Execution => RoleActionClass::Execute,
            ContributionType::Recovery => RoleActionClass::Recover,
        };
        team.authorize_assignment(
            &input.assignment,
            &input.member,
            &context.session_id,
            &TeamAuthorityRequest {
                action,
                context: self.team_scope_context(context)?,
                runtime_grant: [action].into(),
                external_grant: [action].into(),
                now,
            },
        )?;
        if input.contribution_type == ContributionType::Proposal && input.member != review.proposer
        {
            return Err(invalid("Proposal must come from the designated proposer"));
        }
        if !findings.is_empty()
            && !matches!(
                input.contribution_type,
                ContributionType::Challenge | ContributionType::Review
            )
        {
            return Err(invalid(
                "Only reviewer/challenger contributions may record findings",
            ));
        }
        for id in &input.evidence_paths {
            let path = self.evidence_path(id)?;
            if path.claim.id != review.target.claim || path.created_at > now {
                return Err(invalid(
                    "Contribution evidence belongs to another claim or the future",
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for f in findings {
            if !ids.insert(&f.id)
                || f.contribution != input.id
                || f.statement.trim().is_empty()
                || f.statement.len() > 4096
                || !f.evidence_paths.is_subset(&input.evidence_paths)
            {
                return Err(invalid("Malformed or unbound review finding"));
            }
            if f.kind == ReviewFindingKind::NoIssueFound && f.evidence_paths.is_empty() {
                return Err(invalid("No-issue finding requires inspectable evidence"));
            }
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM agent_contributions WHERE review=?1",
            [review.id.to_string()],
            |r| r.get(0),
        )?;
        if count >= 512 {
            return Err(invalid("Review has reached its bounded contribution limit"));
        }
        let mut c = input.clone();
        c.created_at = now;
        let data = serde_json::to_string(&c)?;
        tx.execute(
            "INSERT INTO agent_contributions(id,review,data) VALUES(?1,?2,?3)",
            params![c.id.to_string(), review.id.to_string(), data],
        )?;
        for f in findings {
            tx.execute(
                "INSERT INTO review_findings(id,review,contribution,data) VALUES(?1,?2,?3,?4)",
                params![
                    f.id.to_string(),
                    review.id.to_string(),
                    c.id.to_string(),
                    serde_json::to_string(f)?
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO team_events(team,kind,data) VALUES(?1,'contribution_recorded',?2)",
            params![team.id.to_string(), data],
        )?;
        tx.commit()?;
        Ok(c)
    }
    /// Explicit local user disposition. Not exposed to the agent Bridge and not external approval.
    /// Retains both finding and disposition for audit. Evidence disagreement still blocks the gate.
    pub fn resolve_review_finding(
        &self,
        input: &ReviewFindingResolution,
    ) -> Result<ReviewFindingResolution> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let (review_id, data): (String, String) = tx.query_row(
            "SELECT review,data FROM review_findings WHERE id=?1",
            [input.finding.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let finding: ReviewFinding = serde_json::from_str(&data)?;
        let review = self.team_review(&review_id.parse()?)?;
        if finding.kind == ReviewFindingKind::NoIssueFound
            || input.reason.trim().is_empty()
            || input.reason.len() > 4096
            || input.evidence_paths.is_empty()
            || input.evidence_paths.len() > 128
        {
            return Err(invalid(
                "Finding disposition requires bounded reason and evidence",
            ));
        }
        for id in &input.evidence_paths {
            if self.evidence_path(id)?.claim.id != review.target.claim {
                return Err(invalid("Disposition evidence concerns another claim"));
            }
        }
        let mut resolution = input.clone();
        resolution.created_at = Utc::now();
        let data = serde_json::to_string(&resolution)?;
        tx.execute(
            "INSERT INTO review_finding_resolutions(finding,data) VALUES(?1,?2)",
            params![resolution.finding.to_string(), data],
        )?;
        tx.execute("INSERT INTO team_events(team,kind,data) VALUES(?1,'finding_resolved_by_local_user',?2)",params![review.team.to_string(),data])?;
        tx.commit()?;
        Ok(resolution)
    }
    pub fn team_evidence(&self, id: &TeamReviewId) -> Result<TeamEvidenceAssessment> {
        let review = self.team_review(id)?;
        let contributions = self.team_contributions(id)?;
        let mut ids: BTreeSet<_> = contributions
            .iter()
            .filter(|c| {
                !matches!(
                    c.contribution_type,
                    ContributionType::Proposal | ContributionType::Hypothesis
                )
            })
            .flat_map(|c| c.evidence_paths.iter().cloned())
            .collect();
        // Known contradictions cannot be hidden by leaving them out of a contribution.
        ids.extend(
            self.evidence_paths(&review.target.claim)?
                .into_iter()
                .filter(|p| p.outcome == EvidenceOutcome::Contradicts)
                .map(|p| p.id),
        );
        let paths: Vec<_> = ids
            .iter()
            .map(|id| self.evidence_path(id))
            .collect::<Result<_>>()?;
        let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
        let fused = DeterministicEvidenceFusionPolicy.fuse(
            &self.claim(&review.target.claim)?,
            &paths,
            &diversity,
        )?;
        Ok(TeamEvidenceAssessment {
            review: id.clone(),
            contributions: contributions.iter().map(|c| c.id.clone()).collect(),
            fused,
            fault_domains: fault_domains(&paths),
            echo: evidence_echo_assessment(&paths),
        })
    }
    pub fn assess_team_review(
        &self,
        id: &TeamReviewId,
        context: &RuntimeDecisionContext,
    ) -> Result<ReviewGateAssessment> {
        self.assess_team_review_at(id, context, Utc::now())
    }
    /// Evaluate current records at a supplied clock for simulation, not historical replay.
    /// Runtime always uses the current clock.
    pub fn assess_team_review_at(
        &self,
        id: &TeamReviewId,
        context: &RuntimeDecisionContext,
        now: chrono::DateTime<Utc>,
    ) -> Result<ReviewGateAssessment> {
        let review = self.team_review(id)?;
        let team = self.agent_team(&review.team)?;
        let contributions = self.team_contributions(id)?;
        let findings = self.review_findings(id)?;
        let evidence = self.team_evidence(id)?;
        let high = self.team_requires_separation(&team.id, team.revision, context.risk.severity)?;
        let mut reasons = vec![];
        let mut status = ReviewGateStatus::Satisfied;
        let binding = context
            .team
            .as_ref()
            .ok_or_else(|| invalid("Review requires team binding"))?;
        if binding.team != team.id
            || binding.revision != team.revision
            || review.team_revision != team.revision
            || binding.member != review.executor
            || review.created_at > now
            || contributions.iter().any(|c| c.created_at > now)
            || review.expires_at <= now
            || review.target.action_hash != review_action_hash(context)?
        {
            status = ReviewGateStatus::Blocked;
            reasons.push(
                "Review identity, revision, executor, expiry or action does not match".into(),
            );
        }
        if high && review.proposer == review.executor {
            status = ReviewGateStatus::Blocked;
            reasons.push("High-risk proposer and executor must be distinct".into());
        }
        if !contributions.iter().any(|c| {
            c.member == review.proposer && c.contribution_type == ContributionType::Proposal
        }) {
            if status != ReviewGateStatus::Blocked {
                status = ReviewGateStatus::ReviewRequired;
            }
            reasons.push("Designated proposer has not recorded a proposal".into());
        }
        let scope = self.team_scope_context(context)?;
        let mut covered = BTreeSet::new();
        for c in &contributions {
            if !matches!(
                c.contribution_type,
                ContributionType::Review | ContributionType::Challenge
            ) {
                continue;
            }
            let Some(a) = team
                .role_assignments
                .iter()
                .find(|a| a.id == c.assignment && a.member == c.member)
            else {
                continue;
            };
            if team.assignment_authority(&a.id, now).is_err()
                || DeterministicApplicabilityEvaluator
                    .evaluate(&a.scope, &scope)
                    .status
                    != ApplicabilityStatus::Applicable
            {
                continue;
            }
            if high && (c.member == review.proposer || c.member == review.executor) {
                continue;
            }
            if findings
                .iter()
                .any(|f| f.contribution == c.id && f.kind == ReviewFindingKind::NoIssueFound)
            {
                covered.insert(a.role.clone());
            }
        }
        if !review.required_roles.is_subset(&covered) {
            if status != ReviewGateStatus::Blocked {
                status = ReviewGateStatus::ReviewRequired;
            }
            reasons.push(
                "Required scoped, distinct reviewers have not supplied evidence-backed findings"
                    .into(),
            );
        }
        // A second review must not erase a known finding about the same exact action.
        let all_findings:Vec<(String,String,Option<String>)>=self.connection.prepare("SELECT f.id,f.data,x.data FROM review_findings f JOIN team_reviews r ON r.id=f.review LEFT JOIN review_finding_resolutions x ON x.finding=f.id WHERE r.team=?1 AND r.action_hash=?2 ORDER BY f.id")?.query_map(params![team.id.to_string(),review.target.action_hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?;
        for (_, data, resolution) in &all_findings {
            let f: ReviewFinding = serde_json::from_str(data)?;
            if f.kind != ReviewFindingKind::NoIssueFound && resolution.is_none() {
                status = ReviewGateStatus::Blocked;
                reasons.push(format!("Unresolved {:?} finding {}", f.kind, f.id));
            }
        }
        let minimum = if high {
            review.minimum_diversity.max(DiversityClass::Moderate)
        } else {
            review.minimum_diversity
        };
        if !evidence.fused.contradiction_paths.is_empty() {
            status = ReviewGateStatus::Blocked;
            reasons.push("Known evidence contradicts the reviewed claim".into());
        } else if !evidence.fused.diversity.diversity_class.satisfies(minimum)
            || !matches!(
                evidence.fused.status,
                FusedEvidenceStatus::Supported | FusedEvidenceStatus::DiverseSupport
            )
        {
            if status == ReviewGateStatus::Satisfied {
                status = ReviewGateStatus::EvidenceRequired;
            }
            reasons.push("Evidence support or diversity is insufficient".into());
        }
        let common_mode = evidence
            .fused
            .diversity
            .dependency_overlaps
            .iter()
            .any(|o| {
                o.paths.len() == evidence.fused.diversity.path_count
                    && matches!(
                        o.kind,
                        EpistemicDependencyKind::Experience
                            | EpistemicDependencyKind::Evaluator
                            | EpistemicDependencyKind::ExternalEvidence
                    )
            });
        let stale_support = evidence.fused.support_paths.iter().any(|id| {
            self.evidence_path(id).is_ok_and(|p| {
                now - p.created_at >= Duration::seconds(i64::from(review.max_evidence_age_seconds))
            })
        });
        if stale_support
            || (high && (common_mode || evidence.echo.status == EvidenceEchoStatus::Strong))
        {
            if status == ReviewGateStatus::Satisfied {
                status = ReviewGateStatus::EvidenceRequired;
            }
            reasons.push(
                "Supporting evidence is stale or shares an unresolved common-mode dependency"
                    .into(),
            );
        }
        let paths: BTreeSet<_> = evidence
            .fused
            .support_paths
            .iter()
            .chain(&evidence.fused.contradiction_paths)
            .chain(&evidence.fused.inconclusive_paths)
            .cloned()
            .collect();
        let evidence_hash = blake3::hash(&serde_json::to_vec(&(
            &review,
            &contributions,
            &all_findings,
            &evidence,
        ))?)
        .to_hex()
        .to_string();
        Ok(ReviewGateAssessment {
            review: id.clone(),
            status,
            reasons,
            evidence: paths,
            diversity: evidence.fused.diversity,
            fused_status: evidence.fused.status,
            evidence_hash,
        })
    }
}

impl Store {
    pub(crate) fn team_action_has_review(
        &self,
        team: &AgentTeamId,
        context: &RuntimeDecisionContext,
    ) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM team_reviews WHERE team=?1 AND action_hash=?2)",
            params![team.to_string(), review_action_hash(context)?],
            |r| r.get(0),
        )?)
    }
}
