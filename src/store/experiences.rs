// SPDX-License-Identifier: Apache-2.0

use super::Store;
use crate::{
    Error, Result,
    core::{ExperienceId, RealityId},
    experience::{Experience, Outcome},
};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Default)]
pub struct ExperienceQuery {
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Serialize)]
pub struct ExperienceSummary {
    pub id: ExperienceId,
    pub created_at: DateTime<Utc>,
    pub goal: String,
    pub reality_id: RealityId,
    pub outcome: Outcome,
}

pub trait ExperienceStore {
    fn insert(&self, experience: &Experience) -> Result<()>;
    fn get(&self, id: &ExperienceId) -> Result<Option<Experience>>;
    fn list(&self, query: ExperienceQuery) -> Result<Vec<ExperienceSummary>>;
}

impl ExperienceStore for Store {
    fn insert(&self, exp: &Experience) -> Result<()> {
        exp.evaluation.validate()?;
        let execution = self.execution(&exp.execution_id)?;
        if execution.reality_id != exp.reality_id
            || execution.starting_state != exp.starting_state
            || exp.outcome != Outcome::from_evaluation(&exp.evaluation)
        {
            return Err(Error::InvalidInput(
                "Experience does not match its execution/evaluation".into(),
            ));
        }
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO evaluations(id,execution_id,data) VALUES(?1,?2,?3)",
            params![
                exp.evaluation.id.to_string(),
                exp.execution_id.to_string(),
                serde_json::to_string(&exp.evaluation)?
            ],
        )?;
        tx.execute("INSERT INTO experiences(id,created_at,reality_id,execution_id,evaluation_id,outcome,data) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![exp.id.to_string(), exp.created_at.to_rfc3339(), exp.reality_id.to_string(), exp.execution_id.to_string(), exp.evaluation.id.to_string(), serde_json::to_string(&exp.outcome)?, serde_json::to_string(exp)?])?;
        for artifact in &exp.evidence.artifacts {
            tx.execute("INSERT INTO experience_artifacts(experience_id,path,blake3,bytes,kind) VALUES(?1,?2,?3,?4,?5)", params![exp.id.to_string(), artifact.path.to_string_lossy(), artifact.blake3, i64::try_from(artifact.bytes).map_err(|_| Error::InvalidInput("Artifact exceeds SQLite size range".into()))?, serde_json::to_string(&artifact.kind)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get(&self, id: &ExperienceId) -> Result<Option<Experience>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experiences WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }

    fn list(&self, query: ExperienceQuery) -> Result<Vec<ExperienceSummary>> {
        let mut stmt = self.connection.prepare("SELECT id,created_at,json_extract(data,'$.goal'),reality_id,outcome FROM experiences WHERE (?1 IS NULL OR outcome=?1) ORDER BY created_at,id")?;
        let outcome = query
            .outcome
            .map(|s| serde_json::to_string(&s))
            .transpose()?;
        stmt.query_map([outcome], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .map(|row| {
            let (id, created_at, goal, reality_id, outcome) = row?;
            Ok(ExperienceSummary {
                id: id.parse()?,
                created_at: DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|e| Error::InvalidInput(e.to_string()))?
                    .with_timezone(&Utc),
                goal,
                reality_id: reality_id.parse()?,
                outcome: serde_json::from_str(&outcome)?,
            })
        })
        .collect()
    }
}

impl Store {
    pub fn experience(&self, id: &ExperienceId) -> Result<Experience> {
        ExperienceStore::get(self, id)?
            .ok_or_else(|| Error::NotFound(format!("Experience {id} not found")))
    }
}
