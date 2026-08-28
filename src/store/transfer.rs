// SPDX-License-Identifier: Apache-2.0

use super::Store;
use crate::{
    Error, Result,
    application::{
        ApplicationVerification, ExperienceRelation, LessonApplication, LessonInfluence,
    },
    core::{ExperienceId, LessonId},
    experience::Experience,
    lesson::Lesson,
};
use crate::{
    experience::Outcome,
    experiment::{ExperimentConclusion, ExperimentStatus},
    validation::{ApplicationEvidence, DistinctApplicationValidation, LessonEvidenceSummary},
};
use rusqlite::{OptionalExtension, Transaction, params};

impl Store {
    pub fn latest_influenced_experience(&self) -> Result<Experience> {
        let influential:Option<String>=self.connection.query_row("SELECT e.data FROM experiences e WHERE EXISTS(SELECT 1 FROM lesson_applications a WHERE a.experience_id=e.id AND json_extract(a.data,'$.influence')='applied') OR EXISTS(SELECT 1 FROM reflex_matches m WHERE m.experience_id=e.id) ORDER BY e.created_at DESC,e.id DESC LIMIT 1",[],|r|r.get(0)).optional()?;
        let data = if let Some(data) = influential {
            Some(data)
        } else {
            self.connection
                .query_row(
                    "SELECT data FROM experiences ORDER BY created_at DESC,id DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
        };
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound("No Experience has been recorded yet".into())
        })?)?)
    }
    pub fn status_counts(&self) -> Result<serde_json::Value> {
        let mut counts = serde_json::Map::new();
        for table in [
            "experiences",
            "lessons",
            "experiments",
            "lesson_applications",
            "repeated_mistakes",
            "chaos_campaigns",
            "chaos_trials",
            "operating_envelopes",
            "reflexes",
            "recoveries",
            "resilience_tests",
            "skills",
        ] {
            let count: i64 =
                self.connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            counts.insert(table.into(), count.into());
        }
        let mut states = serde_json::Map::new();
        let mut statement=self.connection.prepare("SELECT json_extract(data,'$.status'),COUNT(*) FROM lessons GROUP BY json_extract(data,'$.status')")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (state, count) = row?;
            states.insert(state, count.into());
        }
        counts.insert("lesson_states".into(), states.into());
        Ok(counts.into())
    }
    pub fn lesson_version(&self, id: &LessonId, version: u32) -> Result<Lesson> {
        let data: String = self.connection.query_row(
            "SELECT data FROM lesson_versions WHERE lesson_id=?1 AND version=?2",
            params![id.to_string(), version],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn applications(&self, id: &ExperienceId) -> Result<Vec<LessonApplication>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM lesson_applications WHERE experience_id=?1 ORDER BY created_at,id",
        )?;
        statement
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    pub(super) fn insert_learning(&self, tx: &Transaction<'_>, exp: &Experience) -> Result<()> {
        for application in &exp.lesson_applications {
            let lesson = self.lesson_version(&application.lesson_id, application.lesson_version)?;
            if application.experience_id != exp.id || !lesson.context_match.matches(&exp.context) {
                return Err(Error::InvalidInput(
                    "Application does not match its Experience or Lesson scope".into(),
                ));
            }
            if application.verification == ApplicationVerification::Observed
                && application.influence == LessonInfluence::Applied
                && (!application.delivered
                    || exp.agent.kind != "test-agent"
                    || lesson.prefer != application.resulting_action
                    || !exp.observed_actions.iter().any(|a| {
                        a.observer == "fixture-trace-v2"
                            && Some(&a.action) == application.resulting_action.as_ref()
                    }))
            {
                return Err(Error::InvalidInput(
                    "Observed application lacks matching fixture action evidence".into(),
                ));
            }
            tx.execute("INSERT INTO lesson_applications(id,lesson_id,lesson_version,experience_id,created_at,relevance,influence,verification,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![application.id.to_string(),application.lesson_id.to_string(),application.lesson_version,exp.id.to_string(),application.created_at.to_rfc3339(),f64::from(application.relevance),serde_json::to_string(&application.influence)?,serde_json::to_string(&application.verification)?,serde_json::to_string(application)?])?;
            for artifact in &application.artifacts {
                tx.execute("INSERT INTO application_artifacts(application_id,experience_id,path) VALUES(?1,?2,?3)",params![application.id.to_string(),exp.id.to_string(),artifact.path.to_string_lossy()])?;
            }
        }
        for relation in &exp.relations {
            let target = self.experience(relation.target())?;
            if let ExperienceRelation::TransferFrom(_) = relation {
                let mut supported = false;
                for application in &exp.lesson_applications {
                    if application.influence == LessonInfluence::Applied
                        && application.verification == ApplicationVerification::Observed
                        && self
                            .lesson_version(&application.lesson_id, application.lesson_version)?
                            .source_experience
                            == target.id
                    {
                        supported = true;
                    }
                }
                if !supported {
                    return Err(Error::InvalidInput("Transfer lineage requires an observed application of a Lesson from the target Experience".into()));
                }
            }
            if let ExperienceRelation::RetryOf(_) = relation
                && (exp.starting_state != target.starting_state || exp.goal != target.goal)
            {
                return Err(Error::InvalidInput(
                    "Retry must preserve the original task and starting state".into(),
                ));
            }
            if matches!(
                relation,
                ExperienceRelation::ChaosVariantOf(_) | ExperienceRelation::RecoveryOf(_)
            ) && (exp.starting_state != target.starting_state
                || exp.goal != target.goal
                || exp.evaluation.spec != target.evaluation.spec
                || exp.context.environment.fingerprint != target.context.environment.fingerprint)
            {
                return Err(Error::InvalidInput("Resilience lineage must preserve task, snapshot, environment policy and evaluator".into()));
            }
            if matches!(relation, ExperienceRelation::RecoveryOf(_))
                && (!target
                    .resilience
                    .as_ref()
                    .is_some_and(|r| r.outcome == crate::resilience::ChaosTrialOutcome::Fail)
                    || !exp
                        .resilience
                        .as_ref()
                        .is_some_and(|r| r.recovery_attempt.is_some()))
            {
                return Err(Error::InvalidInput(
                    "Recovery lineage requires a failed trial and a recorded recovery observation"
                        .into(),
                ));
            }
            tx.execute("INSERT INTO experience_relations(source_experience_id,target_experience_id,relation_type) VALUES(?1,?2,?3)",params![exp.id.to_string(),relation.target().to_string(),relation.kind()])?;
        }
        for mistake in &exp.repeated_mistakes {
            if !exp
                .observed_actions
                .iter()
                .any(|a| a.action == mistake.action)
                || !exp
                    .lesson_applications
                    .iter()
                    .any(|a| a.lesson_id == mistake.lesson_id)
            {
                return Err(Error::InvalidInput(
                    "Repeated mistake must link a matched Lesson and an observed action".into(),
                ));
            }
            tx.execute(
                "INSERT INTO repeated_mistakes(experience_id,lesson_id,data) VALUES(?1,?2,?3)",
                params![
                    exp.id.to_string(),
                    mistake.lesson_id.to_string(),
                    serde_json::to_string(mistake)?
                ],
            )?;
        }
        for application in &exp.lesson_applications {
            if application.influence != LessonInfluence::Applied
                || application.verification != ApplicationVerification::Observed
            {
                continue;
            }
            let mut lesson = self.lesson(&application.lesson_id)?;
            let previous = lesson.version;
            let summary = self.lesson_evidence_summary(&lesson.id)?;
            lesson.apply_application(exp, &summary, &DistinctApplicationValidation)?;
            if lesson.version != previous {
                super::learning::update_lesson(tx, &lesson)?;
                tx.execute("INSERT INTO lesson_validations(lesson_id,lesson_version,policy,data) VALUES(?1,?2,?3,?4)",params![lesson.id.to_string(),lesson.version,"distinct-application-v1",serde_json::to_string(&serde_json::json!({"decision":lesson.validation,"evidence":summary}))?])?;
            }
        }
        Ok(())
    }

    pub fn lesson_evidence_summary(&self, id: &LessonId) -> Result<LessonEvidenceSummary> {
        let lesson = self.lesson(id)?;
        let source = self.experience(&lesson.source_experience)?;
        let mut summary = LessonEvidenceSummary::default();
        for experiment in self
            .experiments()?
            .into_iter()
            .filter(|e| e.lesson_id == *id && e.status == ExperimentStatus::Completed)
        {
            match experiment.conclusion {
                ExperimentConclusion::SupportsHypothesis => {
                    summary.controlled_supports.push(experiment.id)
                }
                ExperimentConclusion::ContradictsHypothesis => {
                    summary.controlled_contradictions.push(experiment.id)
                }
                _ => {}
            }
        }
        let mut statement = self.connection.prepare(
            "SELECT data FROM lesson_applications WHERE lesson_id=?1 ORDER BY created_at,id",
        )?;
        let applications = statement
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str::<LessonApplication>(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        for application in applications
            .into_iter()
            .filter(|a| a.influence == LessonInfluence::Applied)
        {
            let exp = self.experience(&application.experience_id)?;
            summary.applications.push(ApplicationEvidence {
                application_id: application.id,
                experience_id: exp.id,
                agent: exp.agent,
                context_key: format!(
                    "{}:{}",
                    exp.starting_state.tree_hash, exp.context.environment.fingerprint
                ),
                distinct: exp.starting_state.tree_hash != source.starting_state.tree_hash,
                observed: application.verification == ApplicationVerification::Observed
                    && application.delivered,
                success: exp.outcome == Outcome::Success,
                relevant: f64::from(application.relevance) >= 0.7
                    && lesson.context_match.matches(&exp.context),
            });
        }
        Ok(summary)
    }

    pub fn retire_lesson(&self, id: &LessonId, reason: Option<String>) -> Result<Lesson> {
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let mut lesson = self.lesson(id)?;
        let previous = lesson.version;
        lesson.retire(reason)?;
        if lesson.version != previous {
            super::learning::update_lesson(&tx, &lesson)?;
        }
        tx.commit()?;
        Ok(lesson)
    }
}
