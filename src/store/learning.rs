// SPDX-License-Identifier: Apache-2.0

use super::Store;
use crate::{
    Error, Result,
    core::{ExperimentId, HypothesisId, LessonId},
    experiment::{
        CounterfactualPlan, Experiment, ExperimentConclusion, ExperimentStatus, TrialResult,
    },
    lesson::{
        ConfidencePolicy, ConfidenceScore, EvidenceRef, EvidenceRelationship, HeuristicConfidence,
        Lesson, LessonStatus,
    },
    reflection::CandidateHypothesis,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;

#[derive(Default)]
pub struct LessonQuery {
    pub status: Option<LessonStatus>,
}
#[derive(Debug, Serialize)]
pub struct LessonSummary {
    pub id: LessonId,
    pub version: u32,
    pub status: LessonStatus,
    pub confidence: ConfidenceScore,
    pub claim: String,
}

pub trait LessonStore {
    fn insert(&self, lesson: &Lesson) -> Result<()>;
    /// Revise descriptive metadata at the next version. Evidence/status updates use finish_experiment.
    fn update(&self, lesson: &Lesson) -> Result<()>;
    fn get(&self, id: &LessonId) -> Result<Option<Lesson>>;
    fn list(&self, query: LessonQuery) -> Result<Vec<LessonSummary>>;
}

fn enum_name(value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::InvalidInput("Expected enum string".into()))
}

fn save_evidence(tx: &Transaction<'_>, lesson: &Lesson) -> Result<()> {
    for evidence in &lesson.evidence {
        let (key, experience, experiment, trial, relationship) = match evidence {
            EvidenceRef::Experience {
                experience_id,
                relationship,
            } => (
                experience_id.to_string(),
                Some(experience_id.to_string()),
                None,
                None,
                relationship,
            ),
            EvidenceRef::Trial {
                experiment_id,
                trial_id,
                relationship,
            } => (
                trial_id.to_string(),
                None,
                Some(experiment_id.to_string()),
                Some(trial_id.to_string()),
                relationship,
            ),
        };
        tx.execute("INSERT INTO lesson_evidence(lesson_id,evidence_key,experience_id,experiment_id,trial_id,relationship) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(lesson_id,evidence_key) DO NOTHING", params![lesson.id.to_string(), key, experience, experiment, trial, enum_name(relationship)?])?;
    }
    tx.execute(
        "INSERT INTO lesson_versions(lesson_id,version,data) VALUES(?1,?2,?3)",
        params![
            lesson.id.to_string(),
            lesson.version,
            serde_json::to_string(lesson)?
        ],
    )?;
    Ok(())
}

fn load_lesson(connection: &Connection, id: &LessonId) -> Result<Option<Lesson>> {
    let data: Option<String> = connection
        .query_row(
            "SELECT data FROM lessons WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    data.map(|data| Ok(serde_json::from_str(&data)?))
        .transpose()
}

fn update_lesson(tx: &Transaction<'_>, lesson: &Lesson) -> Result<()> {
    let previous = load_lesson(tx, &lesson.id)?
        .ok_or_else(|| Error::NotFound(format!("Lesson {} not found", lesson.id)))?;
    if previous.version.checked_add(1) != Some(lesson.version) {
        return Err(Error::Intervention(
            "Lesson version conflict; reload before updating".into(),
        ));
    }
    if lesson.created_at != previous.created_at
        || lesson.source_experience != previous.source_experience
        || lesson.hypothesis_id != previous.hypothesis_id
        || !previous
            .evidence
            .iter()
            .all(|e| lesson.evidence.contains(e))
    {
        return Err(Error::InvalidInput(
            "Lesson identity, creation time and historical evidence cannot be changed".into(),
        ));
    }
    if lesson.status == LessonStatus::Validated {
        return Err(Error::Intervention(
            "Validated promotion is not implemented in V0.1".into(),
        ));
    }
    tx.execute(
        "UPDATE lessons SET version=?2,data=?3 WHERE id=?1 AND version=?4",
        params![
            lesson.id.to_string(),
            lesson.version,
            serde_json::to_string(lesson)?,
            previous.version
        ],
    )?;
    save_evidence(tx, lesson)
}

impl LessonStore for Store {
    fn insert(&self, lesson: &Lesson) -> Result<()> {
        let h = self.hypothesis(&lesson.hypothesis_id)?;
        if lesson.version != 1
            || lesson.status != LessonStatus::Candidate
            || h.source_experience != lesson.source_experience
            || lesson.claim != h.claim
            || lesson.context_match != h.context_match
            || lesson.avoid.as_ref() != Some(&h.avoid)
            || lesson.prefer.as_ref() != Some(&h.prefer)
            || lesson.confidence != HeuristicConfidence.initial()
            || lesson.evidence
                != vec![EvidenceRef::Experience {
                    experience_id: h.source_experience.clone(),
                    relationship: EvidenceRelationship::Origin,
                }]
            || !lesson
                .context_match
                .matches(&self.experience(&lesson.source_experience)?.context)
        {
            return Err(Error::InvalidInput(
                "A new Lesson must be a scoped Candidate at version 1 with matching provenance"
                    .into(),
            ));
        }
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO lessons(id,source_experience,hypothesis_id,version,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)", params![lesson.id.to_string(),lesson.source_experience.to_string(),lesson.hypothesis_id.to_string(),lesson.version,lesson.created_at.to_rfc3339(),serde_json::to_string(lesson)?])?;
        save_evidence(&tx, lesson)?;
        tx.commit()?;
        Ok(())
    }
    fn update(&self, lesson: &Lesson) -> Result<()> {
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let previous = load_lesson(&tx, &lesson.id)?
            .ok_or_else(|| Error::NotFound(format!("Lesson {} not found", lesson.id)))?;
        if lesson.status != previous.status
            || lesson.confidence != previous.confidence
            || lesson.evidence != previous.evidence
        {
            return Err(Error::InvalidInput("Lesson status, confidence and evidence may only change through a completed experiment".into()));
        }
        if lesson.claim != previous.claim
            || lesson.context_match != previous.context_match
            || lesson.avoid != previous.avoid
            || lesson.prefer != previous.prefer
        {
            return Err(Error::InvalidInput(
                "Propose a new hypothesis to change the tested claim, scope or actions".into(),
            ));
        }
        update_lesson(&tx, lesson)?;
        tx.commit()?;
        Ok(())
    }
    fn get(&self, id: &LessonId) -> Result<Option<Lesson>> {
        load_lesson(&self.connection, id)
    }
    fn list(&self, query: LessonQuery) -> Result<Vec<LessonSummary>> {
        let lessons: Vec<Lesson> =
            Store::list(self, "SELECT data FROM lessons ORDER BY created_at,id")?;
        Ok(lessons
            .into_iter()
            .filter(|l| query.status.is_none_or(|s| s == l.status))
            .map(|l| LessonSummary {
                id: l.id,
                version: l.version,
                status: l.status,
                confidence: l.confidence,
                claim: l.claim,
            })
            .collect())
    }
}

impl Store {
    pub fn insert_hypothesis(&self, h: &CandidateHypothesis) -> Result<()> {
        if !h
            .context_match
            .matches(&self.experience(&h.source_experience)?.context)
        {
            return Err(Error::InvalidInput(
                "Hypothesis context does not match its source Experience".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO hypotheses(id,source_experience,data) VALUES(?1,?2,?3)",
            params![
                h.id.to_string(),
                h.source_experience.to_string(),
                serde_json::to_string(h)?
            ],
        )?;
        Ok(())
    }
    pub fn hypothesis(&self, id: &HypothesisId) -> Result<CandidateHypothesis> {
        self.get("SELECT data FROM hypotheses WHERE id=?1", &id.to_string())
    }
    pub fn lesson(&self, id: &LessonId) -> Result<Lesson> {
        LessonStore::get(self, id)?.ok_or_else(|| Error::NotFound(format!("Lesson {id} not found")))
    }
    pub fn lesson_versions(&self, id: &LessonId) -> Result<Vec<Lesson>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM lesson_versions WHERE lesson_id=?1 ORDER BY version")?;
        stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    pub fn insert_experiment(&self, experiment: &Experiment) -> Result<()> {
        let lesson = self.lesson(&experiment.lesson_id)?;
        let source = self.experience(&lesson.source_experience)?;
        let expected = CounterfactualPlan::from_lesson(&source, &lesson)?;
        let mut plan = experiment.plan.clone();
        for (trial, expected) in plan.trials.iter_mut().zip(&expected.trials) {
            trial.id = expected.id.clone();
        }
        if lesson.source_experience != experiment.source_experience
            || lesson.hypothesis_id != experiment.hypothesis_id
            || experiment.status != ExperimentStatus::Running
            || !experiment.trials.is_empty()
            || experiment.starting_state != source.starting_state
            || plan != expected
            || experiment.conclusion != ExperimentConclusion::Inconclusive
        {
            return Err(Error::InvalidInput(
                "Invalid experiment provenance or initial state".into(),
            ));
        }
        self.connection.execute("INSERT INTO experiments(id,source_experience,lesson_id,hypothesis_id,created_at,status,data) VALUES(?1,?2,?3,?4,?5,'running',?6)", params![experiment.id.to_string(),experiment.source_experience.to_string(),experiment.lesson_id.to_string(),experiment.hypothesis_id.to_string(),experiment.created_at.to_rfc3339(),serde_json::to_string(experiment)?])?;
        Ok(())
    }
    pub fn insert_trial(&self, experiment: &Experiment, trial: &TrialResult) -> Result<()> {
        let stored = self.experiment(&experiment.id)?;
        if stored.plan != experiment.plan {
            return Err(Error::InvalidInput("Experiment plan is immutable".into()));
        }
        let exp = self.experience(&trial.experience_id)?;
        let position = experiment
            .plan
            .trials
            .iter()
            .position(|s| s == &trial.spec)
            .ok_or_else(|| Error::InvalidInput("Trial does not match experiment plan".into()))?;
        if exp.reality_id != trial.reality_id
            || exp.execution_id != trial.execution.id
            || exp.evaluation.id != trial.evaluation.id
            || exp.starting_state != experiment.starting_state
            || exp.outcome != trial.outcome
            || exp.context.environment.fingerprint != experiment.plan.environment_fingerprint
            || trial.starting_state != exp.starting_state
            || trial.environment_fingerprint != exp.context.environment.fingerprint
            || trial.evaluation.spec != trial.spec.evaluation
            || !exp
                .replay
                .as_ref()
                .is_some_and(|r| r.script == trial.spec.command)
            || serde_json::to_value(&trial.execution)?
                != serde_json::to_value(self.execution(&exp.execution_id)?)?
            || serde_json::to_value(&trial.evaluation)? != serde_json::to_value(&exp.evaluation)?
            || serde_json::to_value(&trial.artifacts)?
                != serde_json::to_value(&exp.evidence.artifacts)?
        {
            return Err(Error::InvalidInput(
                "Trial does not match persisted Experience or equivalent starting state".into(),
            ));
        }
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let status: String = tx.query_row(
            "SELECT status FROM experiments WHERE id=?1",
            [experiment.id.to_string()],
            |r| r.get(0),
        )?;
        if status != "running" {
            return Err(Error::InvalidInput(
                "Cannot append trials to a finished experiment".into(),
            ));
        }
        tx.execute("INSERT INTO trials(id,experiment_id,position,experience_id,reality_id,execution_id,evaluation_id,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![trial.spec.id.to_string(),experiment.id.to_string(),position as i64,trial.experience_id.to_string(),trial.reality_id.to_string(),trial.execution.id.to_string(),trial.evaluation.id.to_string(),serde_json::to_string(trial)?])?;
        for a in &trial.artifacts {
            tx.execute(
                "INSERT INTO trial_artifacts(trial_id,experience_id,path) VALUES(?1,?2,?3)",
                params![
                    trial.spec.id.to_string(),
                    trial.experience_id.to_string(),
                    a.path.to_string_lossy()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn experiment(&self, id: &ExperimentId) -> Result<Experiment> {
        let mut experiment: Experiment =
            self.get("SELECT data FROM experiments WHERE id=?1", &id.to_string())?;
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM trials WHERE experiment_id=?1 ORDER BY position")?;
        experiment.trials = stmt
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(experiment)
    }
    pub fn experiments(&self) -> Result<Vec<Experiment>> {
        let mut stmt = self
            .connection
            .prepare("SELECT id FROM experiments ORDER BY created_at,id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|row| self.experiment(&row?.parse()?))
            .collect()
    }
    /// Commit the terminal experiment and the latest Lesson revision atomically.
    pub fn finish_experiment(&self, experiment: &Experiment) -> Result<()> {
        if experiment.status == ExperimentStatus::Running {
            return Err(Error::InvalidInput("Experiment is not finished".into()));
        }
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let stored = self.experiment(&experiment.id)?;
        if stored.plan != experiment.plan
            || stored.source_experience != experiment.source_experience
            || stored.lesson_id != experiment.lesson_id
            || stored.hypothesis_id != experiment.hypothesis_id
            || stored.starting_state != experiment.starting_state
            || stored.created_at != experiment.created_at
            || serde_json::to_value(&stored.trials)? != serde_json::to_value(&experiment.trials)?
            || (experiment.status != ExperimentStatus::Completed
                && experiment.conclusion != ExperimentConclusion::Inconclusive)
        {
            return Err(Error::InvalidInput(
                "Terminal experiment must use its persisted plan and trial evidence".into(),
            ));
        }
        let changed = tx.execute(
            "UPDATE experiments SET status=?2,data=?3 WHERE id=?1 AND status='running'",
            params![
                experiment.id.to_string(),
                enum_name(&experiment.status)?,
                serde_json::to_string(experiment)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention(
                "Experiment already finished or missing".into(),
            ));
        }
        if experiment.status == ExperimentStatus::Completed {
            let mut lesson = load_lesson(&tx, &experiment.lesson_id)?
                .ok_or_else(|| Error::NotFound("Experiment Lesson missing".into()))?;
            lesson.apply_experiment(experiment, &HeuristicConfidence)?;
            update_lesson(&tx, &lesson)?;
        }
        tx.commit()?;
        Ok(())
    }
}
