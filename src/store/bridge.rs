// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Result,
    bridge::engine::{RunRecord, Session},
    core::LessonId,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

impl Store {
    pub fn bridge_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM bridge_sessions ORDER BY id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn save_bridge_session(&self, session: &Session) -> Result<()> {
        self.connection.execute("INSERT INTO bridge_sessions(id,revision,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,data=excluded.data WHERE excluded.revision > bridge_sessions.revision",
            params![session.id, session.revision.min(i64::MAX as u64) as i64, serde_json::to_string(session)?])?;
        Ok(())
    }
    pub fn bridge_event(&self, session: &str, kind: &str, data: &Value) -> Result<()> {
        self.connection.execute(
            "INSERT INTO bridge_events(session_id,kind,data) VALUES(?1,?2,?3)",
            params![session, kind, serde_json::to_string(data)?],
        )?;
        Ok(())
    }
    pub fn bridge_events(&self, after: u64) -> Result<Value> {
        let mut stmt = self.connection.prepare("SELECT sequence,created_at,session_id,kind,data FROM bridge_events WHERE sequence > ?1 ORDER BY sequence LIMIT 100")?;
        let rows = stmt.query_map([after.min(i64::MAX as u64) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (sequence, created_at, session, kind, data) = row?;
            events.push(json!({"sequence":sequence,"created_at":created_at,"session_id":session,"kind":kind,"data":serde_json::from_str::<Value>(&data)?}));
        }
        Ok(json!({"events":events}))
    }
    pub fn save_bridge_run(&self, session: &str, run: &RunRecord) -> Result<()> {
        self.connection.execute("INSERT INTO bridge_runs(session_id,run_id,experience_id,data) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,run_id) DO UPDATE SET experience_id=excluded.experience_id,data=excluded.data",
            params![session,run.run_id, if run.status == "completed" { Some(&run.experience_id) } else { None },serde_json::to_string(run)?])?;
        Ok(())
    }
    pub fn bridge_runs(&self, session: &str) -> Result<Vec<RunRecord>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM bridge_runs WHERE session_id=?1")?;
        stmt.query_map([session], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn bridge_feedback(
        &self,
        session: &str,
        agent: &str,
        id: &LessonId,
        reason: &str,
    ) -> Result<()> {
        self.connection.execute("INSERT OR IGNORE INTO lesson_agent_feedback(lesson_id,session_id,agent,reason) VALUES(?1,?2,?3,?4)",params![id.to_string(),session,agent,reason])?;
        let count: u32 = self.connection.query_row(
            "SELECT COUNT(*) FROM lesson_agent_feedback WHERE lesson_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if self.lesson(id)?.status == crate::lesson::LessonStatus::Validated
            && (count >= 2 || reason == "environment_changed")
        {
            self.connection.execute("INSERT INTO lesson_review_flags(lesson_id,needs_revalidation,reason) VALUES(?1,1,?2) ON CONFLICT(lesson_id) DO UPDATE SET needs_revalidation=1,reason=excluded.reason",params![id.to_string(),"Repeated rejection or environment change; scope remains intact"])?;
        }
        Ok(())
    }
    pub fn lesson_agent_provenance(&self, id: &LessonId) -> Result<Value> {
        let lesson = self.lesson(id)?;
        use crate::lesson::{AgentEvidenceContribution, EvidenceRelationship};
        let mut contributions = vec![AgentEvidenceContribution {
            agent: self.experience(&lesson.source_experience)?.agent,
            experience_id: lesson.source_experience.clone(),
            relationship: EvidenceRelationship::Origin,
            role: "discovery".into(),
        }];
        for experiment in self
            .experiments()?
            .into_iter()
            .filter(|e| e.lesson_id == *id)
        {
            for trial in experiment.trials {
                let exp = self.experience(&trial.experience_id)?;
                contributions.push(AgentEvidenceContribution {
                    agent: exp.agent,
                    experience_id: exp.id,
                    relationship: match experiment.conclusion {
                        crate::experiment::ExperimentConclusion::SupportsHypothesis => {
                            EvidenceRelationship::Supports
                        }
                        crate::experiment::ExperimentConclusion::ContradictsHypothesis => {
                            EvidenceRelationship::Contradicts
                        }
                        _ => EvidenceRelationship::Inconclusive,
                    },
                    role: "counterfactual".into(),
                });
            }
        }
        let evidence = self.lesson_evidence_summary(id)?;
        for e in evidence.applications {
            contributions.push(AgentEvidenceContribution {
                agent: e.agent,
                experience_id: e.experience_id,
                relationship: if e.success && e.observed {
                    EvidenceRelationship::Supports
                } else {
                    EvidenceRelationship::Inconclusive
                },
                role: if e.distinct && e.success && e.observed {
                    "successful_transfer"
                } else {
                    "application"
                }
                .into(),
            });
        }
        let review: Option<String> = self.connection.query_row("SELECT reason FROM lesson_review_flags WHERE lesson_id=?1 AND needs_revalidation=1",[id.to_string()],|r|r.get(0)).optional()?;
        Ok(
            json!({"contributions":contributions,"independence_established":false,"needs_revalidation":review.is_some(),"review_reason":review}),
        )
    }
}

pub(super) fn valid_observation(
    exp: &crate::experience::Experience,
    observation: &crate::application::ObservedAction,
) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(&observation.artifact.path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 32 * 1024 * 1024
    {
        return false;
    }
    let Ok(bytes) = std::fs::read(&observation.artifact.path) else {
        return false;
    };
    if blake3::hash(&bytes).to_hex().as_str() != observation.artifact.blake3 {
        return false;
    }
    let Ok(actions) = serde_json::from_slice::<Vec<crate::bridge::engine::RecordedAction>>(&bytes)
    else {
        return false;
    };
    actions.iter().any(|a| {
        a.result.as_ref().is_some_and(|r| r.success)
            && match &a.action {
                crate::bridge::protocol::NormalizedAction::Shell { command, cwd } => {
                    observation.action.matches_shell(command)
                        && std::path::Path::new(cwd) == exp.context.environment.cwd
                }
                _ => false,
            }
    })
}

impl Store {
    pub(crate) fn persist_bridge_experience(
        &self,
        reality: &crate::core::Reality,
        execution: &crate::core::ExecutionRecord,
        experience: &crate::experience::Experience,
        session: &str,
        run: &RunRecord,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.insert_reality(reality)?;
        self.insert_execution(execution)?;
        self.insert_experience_in_transaction(&tx, experience)?;
        let mut run = run.clone();
        run.status = "completed".into();
        run.outcome = serde_json::to_value(experience.outcome)?
            .as_str()
            .map(str::to_owned);
        self.save_bridge_run(session, &run)?;
        tx.commit()?;
        Ok(())
    }
}
