// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

use super::Store;
use crate::{
    Error, Result,
    core::{ClaimId, EvidencePathId, EvidenceSessionId, LessonId},
    epistemic::{
        Claim, EpistemicReport, EvidencePath, EvidenceSession, ExperienceActivationState,
        ExperienceBlastRadius, ExperienceInfluence, ExperienceQuarantineEvent,
        FusedEvidenceAssessment, InfluenceOutcome, LessonImpactAssessment, build_report,
        context_fingerprint, dependency_graph, dependency_values,
    },
    experience::{Experience, Outcome},
};

pub trait EpistemicStore {
    fn insert_claim(&self, claim: &Claim) -> Result<()>;
    fn claim(&self, id: &ClaimId) -> Result<Claim>;
    fn claims(&self) -> Result<Vec<Claim>>;
    fn insert_evidence_path(&self, path: &EvidencePath) -> Result<EvidencePath>;
    fn evidence_path(&self, id: &EvidencePathId) -> Result<EvidencePath>;
    fn evidence_paths(&self, claim: &ClaimId) -> Result<Vec<EvidencePath>>;
    fn insert_evidence_session(&self, session: &EvidenceSession) -> Result<()>;
    fn evidence_session(&self, id: &EvidenceSessionId) -> Result<EvidenceSession>;
    fn record_fused_assessment(&self, assessment: &FusedEvidenceAssessment) -> Result<()>;
    fn latest_fused_assessment(&self, claim: &ClaimId) -> Result<Option<FusedEvidenceAssessment>>;
    fn epistemic_report(&self, claim: &ClaimId) -> Result<EpistemicReport>;
    fn record_experience_influence(&self, influence: &ExperienceInfluence) -> Result<()>;
    fn lesson_impact(&self, lesson: &LessonId) -> Result<LessonImpactAssessment>;
    fn set_lesson_activation(
        &self,
        lesson: &LessonId,
        state: ExperienceActivationState,
        reason: String,
    ) -> Result<ExperienceQuarantineEvent>;
    fn lesson_activation_state(&self, lesson: &LessonId) -> Result<ExperienceActivationState>;
}

fn enum_name(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidInput("Expected a unit enum".into()))
}

fn canonical_hash(claim: &Claim) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(&(
        claim.kind,
        claim.canonical_statement(),
        &claim.scope,
    ))?)
    .to_hex()
    .to_string())
}

impl EpistemicStore for Store {
    fn insert_claim(&self, claim: &Claim) -> Result<()> {
        claim.validate()?;
        self.connection.execute(
            "INSERT INTO claims(id,kind,canonical_hash,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![
                claim.id.to_string(),
                enum_name(&claim.kind)?,
                canonical_hash(claim)?,
                claim.created_at.to_rfc3339(),
                serde_json::to_string(claim)?,
            ],
        ).map_err(|error| {
            if error.to_string().contains("claims.canonical_hash") {
                Error::Intervention("An equivalent canonical Claim already exists; reuse it instead of multiplying Claim IDs".into())
            } else { Error::Sqlite(error) }
        })?;
        Ok(())
    }

    fn claim(&self, id: &ClaimId) -> Result<Claim> {
        self.get("SELECT data FROM claims WHERE id=?1", &id.to_string())
    }

    fn claims(&self) -> Result<Vec<Claim>> {
        self.list("SELECT data FROM claims ORDER BY created_at,id")
    }

    fn insert_evidence_path(&self, path: &EvidencePath) -> Result<EvidencePath> {
        let _claim = EpistemicStore::claim(self, &path.claim.id)?;
        let mut stored = path.clone();
        let expected = context_fingerprint(&stored.dependencies)?;
        if !stored.context.fingerprint.hash.is_empty() && stored.context.fingerprint != expected {
            return Err(Error::InvalidInput(
                "EvidencePath context fingerprint does not match its observable dependencies"
                    .into(),
            ));
        }
        stored.context.fingerprint = expected;
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO evidence_paths(id,claim_id,source_kind,outcome,context_fingerprint,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                stored.id.to_string(), stored.claim.id.to_string(),
                enum_name(&stored.source.kind())?, enum_name(&stored.outcome)?,
                stored.context.fingerprint.hash, stored.created_at.to_rfc3339(),
                serde_json::to_string(&stored)?,
            ],
        )?;
        for dependency in dependency_values(&stored) {
            tx.execute(
                "INSERT INTO epistemic_dependencies(evidence_path_id,kind,value) VALUES(?1,?2,?3)",
                params![
                    stored.id.to_string(),
                    enum_name(&dependency.kind)?,
                    dependency.value
                ],
            )?;
        }
        refresh_edges(&tx, &stored.claim.id)?;
        tx.commit()?;
        Ok(stored)
    }

    fn evidence_path(&self, id: &EvidencePathId) -> Result<EvidencePath> {
        self.get(
            "SELECT data FROM evidence_paths WHERE id=?1",
            &id.to_string(),
        )
    }

    fn evidence_paths(&self, claim: &ClaimId) -> Result<Vec<EvidencePath>> {
        let mut query = self
            .connection
            .prepare("SELECT data FROM evidence_paths WHERE claim_id=?1 ORDER BY created_at,id")?;
        query
            .query_map([claim.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn insert_evidence_session(&self, session: &EvidenceSession) -> Result<()> {
        let _claim = EpistemicStore::claim(self, &session.claim)?;
        self.connection.execute(
            "INSERT INTO evidence_sessions(id,claim_id,created_at,data) VALUES(?1,?2,?3,?4)",
            params![
                session.id.to_string(),
                session.claim.to_string(),
                session.created_at.to_rfc3339(),
                serde_json::to_string(session)?
            ],
        )?;
        Ok(())
    }

    fn evidence_session(&self, id: &EvidenceSessionId) -> Result<EvidenceSession> {
        self.get(
            "SELECT data FROM evidence_sessions WHERE id=?1",
            &id.to_string(),
        )
    }

    fn record_fused_assessment(&self, assessment: &FusedEvidenceAssessment) -> Result<()> {
        let _claim = EpistemicStore::claim(self, &assessment.claim)?;
        self.connection.execute(
            "INSERT INTO fused_evidence_assessments(claim_id,created_at,data) VALUES(?1,?2,?3)",
            params![
                assessment.claim.to_string(),
                Utc::now().to_rfc3339(),
                serde_json::to_string(assessment)?
            ],
        )?;
        if assessment.status == crate::epistemic::FusedEvidenceStatus::Disputed
            && assessment
                .diversity
                .diversity_class
                .satisfies(crate::epistemic::DiversityClass::Moderate)
        {
            for lesson in assessment
                .diversity
                .dependency_overlaps
                .iter()
                .filter(|overlap| {
                    overlap.kind == crate::epistemic::EpistemicDependencyKind::Experience
                })
                .filter_map(|overlap| overlap.shared_value.parse::<LessonId>().ok())
            {
                self.set_lesson_activation(
                    &lesson,
                    ExperienceActivationState::Advisory,
                    format!(
                        "Diverse contradictory evidence disputes a Claim influenced by {lesson}; revalidation required"
                    ),
                )?;
            }
        }
        Ok(())
    }

    fn latest_fused_assessment(&self, claim: &ClaimId) -> Result<Option<FusedEvidenceAssessment>> {
        let data: Option<String> = self.connection.query_row(
            "SELECT data FROM fused_evidence_assessments WHERE claim_id=?1 ORDER BY sequence DESC LIMIT 1",
            [claim.to_string()], |row| row.get(0),
        ).optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }

    fn epistemic_report(&self, claim: &ClaimId) -> Result<EpistemicReport> {
        build_report(
            EpistemicStore::claim(self, claim)?,
            self.evidence_paths(claim)?,
            &Default::default(),
        )
    }

    fn record_experience_influence(&self, influence: &ExperienceInfluence) -> Result<()> {
        let _lesson = self.lesson(&influence.lesson_id)?;
        if influence.session.trim().is_empty() || influence.repository.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Experience influence requires bounded session and repository labels".into(),
            ));
        }
        let agent_key = serde_json::to_string(&influence.agent)?;
        self.connection.execute(
            "INSERT INTO experience_influence(lesson_id,session_id,agent_key,repository,decision_id,outcome,observed_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![influence.lesson_id.to_string(), influence.session, agent_key, influence.repository,
                influence.decision.as_ref().map(ToString::to_string), enum_name(&influence.outcome)?,
                influence.observed_at.to_rfc3339(), serde_json::to_string(influence)?],
        )?;
        Ok(())
    }

    fn lesson_impact(&self, lesson: &LessonId) -> Result<LessonImpactAssessment> {
        let _lesson = self.lesson(lesson)?;
        let mut sessions = BTreeSet::new();
        let mut agents = BTreeSet::new();
        let mut repositories = BTreeSet::new();
        let mut decisions = BTreeSet::new();
        let mut outcomes = Vec::new();

        let mut explicit = self.connection.prepare(
            "SELECT data FROM experience_influence WHERE lesson_id=?1 ORDER BY observed_at,sequence",
        )?;
        for row in explicit.query_map([lesson.to_string()], |row| row.get::<_, String>(0))? {
            let influence: ExperienceInfluence = serde_json::from_str(&row?)?;
            sessions.insert(influence.session);
            agents.insert(serde_json::to_string(&influence.agent)?);
            repositories.insert(influence.repository);
            decisions.extend(influence.decision.map(|id| id.to_string()));
            outcomes.push(influence.outcome);
        }

        // Legacy lesson applications predate explicit session influence. Their
        // immutable Experience is a conservative session surrogate.
        let mut legacy = self.connection.prepare(
            "SELECT a.experience_id,e.data FROM lesson_applications a JOIN experiences e ON e.id=a.experience_id WHERE a.lesson_id=?1 AND json_extract(a.data,'$.delivered')=1 ORDER BY a.created_at,a.id",
        )?;
        for row in legacy.query_map([lesson.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (experience_id, data) = row?;
            let experience: Experience = serde_json::from_str(&data)?;
            sessions.insert(experience_id);
            agents.insert(serde_json::to_string(&experience.agent)?);
            repositories.insert(experience.context.repository.path.display().to_string());
            outcomes.push(match experience.outcome {
                Outcome::Success => InfluenceOutcome::Successful,
                Outcome::Failure => InfluenceOutcome::Failed,
                Outcome::Inconclusive | Outcome::Interrupted | Outcome::TimedOut => {
                    InfluenceOutcome::Inconclusive
                }
            });
        }
        let blast_radius = ExperienceBlastRadius {
            sessions_influenced: sessions.len(),
            agents_influenced: agents.len(),
            repositories_influenced: repositories.len(),
            decisions_influenced: decisions.len(),
            successful: outcomes
                .iter()
                .filter(|outcome| **outcome == InfluenceOutcome::Successful)
                .count(),
            failed: outcomes
                .iter()
                .filter(|outcome| **outcome == InfluenceOutcome::Failed)
                .count(),
            inconclusive: outcomes
                .iter()
                .filter(|outcome| **outcome == InfluenceOutcome::Inconclusive)
                .count(),
        };
        let activation_state = self.lesson_activation_state(lesson)?;
        let revalidation_required = blast_radius.failed > 0
            || matches!(
                activation_state,
                ExperienceActivationState::Advisory
                    | ExperienceActivationState::Quarantined
                    | ExperienceActivationState::Disabled
            );
        let mut reasons = Vec::new();
        if blast_radius.failed > 0 {
            reasons.push(format!(
                "{} influenced outcome(s) failed",
                blast_radius.failed
            ));
        }
        if activation_state == ExperienceActivationState::Quarantined {
            reasons.push("Lesson is quarantined and excluded from automatic retrieval".into());
        }
        if activation_state == ExperienceActivationState::Advisory {
            reasons
                .push("Lesson is advisory pending revalidation after diverse contradiction".into());
        }
        Ok(LessonImpactAssessment {
            lesson_id: lesson.clone(),
            blast_radius,
            activation_state,
            revalidation_required,
            reasons,
        })
    }

    fn set_lesson_activation(
        &self,
        lesson: &LessonId,
        state: ExperienceActivationState,
        reason: String,
    ) -> Result<ExperienceQuarantineEvent> {
        let _lesson = self.lesson(lesson)?;
        if reason.trim().is_empty() || reason.len() > 2048 {
            return Err(Error::InvalidInput(
                "Activation-state change requires a nonempty reason of at most 2048 bytes".into(),
            ));
        }
        let event = ExperienceQuarantineEvent {
            lesson_id: lesson.clone(),
            state,
            reason,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO experience_quarantines(lesson_id,state,reason,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![lesson.to_string(), enum_name(&state)?, event.reason, event.created_at.to_rfc3339(), serde_json::to_string(&event)?],
        )?;
        Ok(event)
    }

    fn lesson_activation_state(&self, lesson: &LessonId) -> Result<ExperienceActivationState> {
        let data: Option<String> = self.connection.query_row(
            "SELECT data FROM experience_quarantines WHERE lesson_id=?1 ORDER BY sequence DESC LIMIT 1",
            [lesson.to_string()], |row| row.get(0),
        ).optional()?;
        data.map(|data| Ok(serde_json::from_str::<ExperienceQuarantineEvent>(&data)?.state))
            .transpose()
            .map(|state| state.unwrap_or_default())
    }
}

fn refresh_edges(tx: &Transaction<'_>, claim: &ClaimId) -> Result<()> {
    let mut query =
        tx.prepare("SELECT data FROM evidence_paths WHERE claim_id=?1 ORDER BY created_at,id")?;
    let paths = query
        .query_map([claim.to_string()], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str::<EvidencePath>(&row?)?))
        .collect::<Result<Vec<_>>>()?;
    let graph = dependency_graph(&paths);
    for edge in graph.edges {
        tx.execute(
            "INSERT OR IGNORE INTO epistemic_dependency_edges(claim_id,from_node,to_node,kind) VALUES(?1,?2,?3,?4)",
            params![claim.to_string(), edge.from, edge.to, enum_name(&edge.kind)?],
        )?;
    }
    Ok(())
}
