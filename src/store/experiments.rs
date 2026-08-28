// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Error, Result,
    core::{ExperimentId, ExperimentRequestId},
    experimentation::*,
};
use rusqlite::{OptionalExtension, params};

pub trait ExperimentStore {
    fn insert(&self, experiment: &StrategyExperiment) -> Result<()>;
    fn get(&self, id: &ExperimentId) -> Result<Option<StrategyExperiment>>;
    fn update_status(&self, experiment: &StrategyExperiment) -> Result<()>;
    fn list(&self, agent: Option<&str>) -> Result<Vec<StrategyExperiment>>;
}

fn status(value: ExperimentStatus) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or_default()
        .into())
}

impl ExperimentStore for Store {
    fn insert(&self, experiment: &StrategyExperiment) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO experiment_requests(id,request_id,session_id,agent,created_at,status,data) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![experiment.id.to_string(), experiment.request.id.to_string(), experiment.request.session_id, experiment.request.requested_by.kind, experiment.request.created_at.to_rfc3339(), status(experiment.status)?, serde_json::to_string(experiment)?])?;
        for (position, candidate) in experiment.request.candidates.iter().enumerate() {
            tx.execute("INSERT INTO experiment_candidates(id,experiment_id,position,data) VALUES (?1,?2,?3,?4)", params![candidate.id.to_string(), experiment.id.to_string(), position as i64, serde_json::to_string(candidate)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    fn get(&self, id: &ExperimentId) -> Result<Option<StrategyExperiment>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experiment_requests WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|d| Ok(serde_json::from_str(&d)?)).transpose()
    }
    fn update_status(&self, experiment: &StrategyExperiment) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE experiment_requests SET status=?2,data=?3 WHERE id=?1",
            params![
                experiment.id.to_string(),
                status(experiment.status)?,
                serde_json::to_string(experiment)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(format!(
                "Experiment {} not found",
                experiment.id
            )));
        }
        Ok(())
    }
    fn list(&self, agent: Option<&str>) -> Result<Vec<StrategyExperiment>> {
        let mut query = self.connection.prepare("SELECT data FROM experiment_requests WHERE (?1 IS NULL OR agent=?1) ORDER BY created_at,id")?;
        query
            .query_map([agent], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
}

impl Store {
    /// Publish the terminal status/result and terminal progress in one transaction.
    pub fn finish_strategy_experiment(
        &self,
        experiment: &StrategyExperiment,
        progress: &ExperimentProgress,
    ) -> Result<()> {
        if !experiment.status.terminal() || progress.experiment_id != experiment.id {
            return Err(Error::InvalidInput(
                "Terminal experiment/progress mismatch".into(),
            ));
        }
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE experiment_requests SET status=?2,data=?3 WHERE id=?1",
            params![
                experiment.id.to_string(),
                status(experiment.status)?,
                serde_json::to_string(experiment)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::NotFound("Experiment not found".into()));
        }
        tx.execute(
            "INSERT INTO experiment_progress(experiment_id,data) VALUES (?1,?2)",
            params![experiment.id.to_string(), serde_json::to_string(progress)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reserved work is cumulative for the session, including cancelled/failed attempts.
    pub fn session_experiment_reservations(
        &self,
        session: &str,
        excluding: &ExperimentId,
    ) -> Result<(usize, usize)> {
        let (realities, agents) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(json_extract(c.value,'$.execution.kind')='agent_task'),0) FROM experiment_requests e, json_each(e.data,'$.request.candidates') c WHERE e.session_id=?1 AND e.id!=?2 AND e.status!='rejected'",
            params![session,excluding.to_string()], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?)),
        )?;
        Ok((realities as usize, agents as usize))
    }

    pub fn strategy_experiment(&self, id: &ExperimentId) -> Result<StrategyExperiment> {
        ExperimentStore::get(self, id)?
            .ok_or_else(|| Error::NotFound(format!("Experiment {id} not found")))
    }
    pub fn experiment_for_request(
        &self,
        id: &ExperimentRequestId,
    ) -> Result<Option<StrategyExperiment>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experiment_requests WHERE request_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|d| Ok(serde_json::from_str(&d)?)).transpose()
    }
    pub fn cancel_experiment(&self, id: &ExperimentId) -> Result<bool> {
        self.strategy_experiment(id)?;
        Ok(self.connection.execute("UPDATE experiment_requests SET cancel_requested=1 WHERE id=?1 AND status IN ('accepted','running')", [id.to_string()])? != 0)
    }
    pub fn experiment_cancel_requested(&self, id: &ExperimentId) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT cancel_requested FROM experiment_requests WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn insert_candidate_result(
        &self,
        id: &ExperimentId,
        result: &CandidateResult,
    ) -> Result<()> {
        let experience = self.experience(&result.experience_id)?;
        let link = experience.experiment.as_ref().ok_or_else(|| {
            Error::InvalidInput("Candidate Experience has no experiment provenance".into())
        })?;
        if &link.experiment_id != id
            || link.candidate_id != result.candidate_id
            || link.starting_fingerprint != result.starting_fingerprint
            || experience.reality_id != result.reality_id
            || experience.agent != result.agent
            || serde_json::to_value(&experience.evaluation)?
                != serde_json::to_value(&result.evaluation)?
            || serde_json::to_value(&experience.evidence.artifacts)?
                != serde_json::to_value(&result.artifacts)?
            || self.execution(&experience.execution_id)?.status != result.execution_status
        {
            return Err(Error::InvalidInput(
                "Candidate provenance does not match immutable Experience".into(),
            ));
        }
        let changed = self.connection.execute("UPDATE experiment_candidates SET result=?3,experience_id=?4,reality_id=?5 WHERE id=?1 AND experiment_id=?2", params![result.candidate_id.to_string(),id.to_string(),serde_json::to_string(result)?,result.experience_id.to_string(),result.reality_id.to_string()])?;
        if changed != 1 {
            return Err(Error::NotFound("Candidate not found".into()));
        }
        Ok(())
    }
    pub fn candidate_results(&self, id: &ExperimentId) -> Result<Vec<CandidateResult>> {
        let mut query = self.connection.prepare("SELECT result FROM experiment_candidates WHERE experiment_id=?1 AND result IS NOT NULL ORDER BY position")?;
        query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn experiment_progress(
        &self,
        id: &ExperimentId,
        after: u64,
    ) -> Result<Vec<(u64, ExperimentProgress)>> {
        let mut query = self.connection.prepare("SELECT sequence,data FROM experiment_progress WHERE experiment_id=?1 AND sequence>?2 ORDER BY sequence LIMIT 128")?;
        query
            .query_map(
                params![id.to_string(), after.min(i64::MAX as u64) as i64],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
            .map(|r| {
                let (n, d) = r?;
                Ok((n as u64, serde_json::from_str(&d)?))
            })
            .collect()
    }
    pub fn append_experiment_progress(&self, progress: &ExperimentProgress) -> Result<()> {
        self.connection.execute(
            "INSERT INTO experiment_progress(experiment_id,data) VALUES (?1,?2)",
            params![
                progress.experiment_id.to_string(),
                serde_json::to_string(progress)?
            ],
        )?;
        Ok(())
    }
    pub fn insert_experiment_variables(
        &self,
        id: &ExperimentId,
        variables: &[ExperimentVariable],
    ) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        for variable in variables {
            tx.execute(
                "INSERT INTO experiment_variables(experiment_id,name,data) VALUES (?1,?2,?3)",
                params![
                    id.to_string(),
                    variable.name,
                    serde_json::to_string(variable)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn insert_experiment_relation(&self, relation: &ExperimentRelation) -> Result<()> {
        self.connection.execute(
            "INSERT INTO experiment_relations(parent,child,relation) VALUES (?1,?2,?3)",
            params![
                relation.parent.to_string(),
                relation.child.to_string(),
                serde_json::to_value(relation.relation)?.as_str()
            ],
        )?;
        Ok(())
    }
    pub fn experiment_relations(&self, id: &ExperimentId) -> Result<Vec<ExperimentRelation>> {
        let mut query = self.connection.prepare("SELECT parent,child,relation FROM experiment_relations WHERE parent=?1 OR child=?1 ORDER BY parent,child")?;
        query
            .query_map([id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .map(|r| {
                let (p, c, t) = r?;
                Ok(ExperimentRelation {
                    parent: p.parse()?,
                    child: c.parse()?,
                    relation: serde_json::from_value(serde_json::Value::String(t))?,
                })
            })
            .collect()
    }
}
