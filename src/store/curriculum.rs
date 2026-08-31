// SPDX-License-Identifier: Apache-2.0
use super::{AssuranceStore, Store};
use crate::{Error, Result, core::*, curriculum::*};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
fn json(v: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}
pub trait CurriculumStore {
    fn insert(&self, curriculum: &Curriculum) -> Result<()>;
    fn update(&self, curriculum: &Curriculum) -> Result<()>;
    fn get(&self, id: &CurriculumId) -> Result<Option<Curriculum>>;
    fn list(&self, query: CurriculumQuery) -> Result<Vec<Curriculum>>;
}
impl CurriculumStore for Store {
    fn insert(&self, c: &Curriculum) -> Result<()> {
        if c.status != CurriculumStatus::Planned || c.revision != 1 {
            return Err(Error::InvalidInput(
                "Curriculum must start planned at revision 1".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO curricula(id,created_at,session_id,status,revision,data) VALUES(?1,?2,?3,'planned',1,?4)",params![c.id.to_string(),c.created_at.to_rfc3339(),c.session_id,json(c)?])?;
        children(&tx, c)?;
        event(
            &tx,
            c,
            "curriculum_planned",
            None,
            "Plan persisted; explicit invocation is required",
        )?;
        tx.commit()?;
        Ok(())
    }
    fn update(&self, c: &Curriculum) -> Result<()> {
        let old = self.curriculum(&c.id)?;
        if c.revision != old.revision + 1 || old.status.terminal() {
            return Err(Error::Intervention(
                "Curriculum changed concurrently or is terminal".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE curricula SET status=?2,revision=?3,data=?4 WHERE id=?1 AND revision=?5",
            params![
                c.id.to_string(),
                json(&c.status)?.trim_matches('"'),
                c.revision,
                json(c)?,
                old.revision
            ],
        )?;
        if n != 1 {
            return Err(Error::Intervention("Concurrent curriculum revision".into()));
        }
        children(&tx, c)?;
        if old.status != c.status {
            event(
                &tx,
                c,
                match c.status {
                    CurriculumStatus::Running => "curriculum_started",
                    CurriculumStatus::Cancelled => "curriculum_cancelled",
                    _ => "curriculum_completed",
                },
                None,
                &format!("{:?}", c.status),
            )?;
        }
        for t in &c.trials {
            let previous = old.trials.iter().find(|p| p.id == t.id);
            if previous.is_none_or(|p| p.status != t.status)
                && matches!(
                    t.status,
                    GoalStatus::Running | GoalStatus::Completed | GoalStatus::Inconclusive
                )
            {
                event(
                    &tx,
                    c,
                    if t.status == GoalStatus::Running {
                        "curriculum_trial_started"
                    } else {
                        "curriculum_trial_completed"
                    },
                    Some(&t.id),
                    &format!(
                        "{}: {:?}",
                        t.condition,
                        t.result.as_ref().and_then(|r| r.outcome)
                    ),
                )?;
                if t.status == GoalStatus::Completed
                    && t.result
                        .as_ref()
                        .and_then(|r| r.outcome)
                        .is_some_and(|o| o != crate::resilience::ChaosTrialOutcome::Inconclusive)
                {
                    event(
                        &tx,
                        c,
                        "evidence_gap_closed",
                        Some(&t.id),
                        &format!(
                            "{} observed; a failed condition is known failure, not safety",
                            t.condition
                        ),
                    )?;
                }
            }
        }
        if c.status.terminal() {
            for p in &c.after {
                if let Some(b) = c.before.iter().find(|b| b.skill == p.skill)
                    && b.maturity != p.maturity
                {
                    event(
                        &tx,
                        c,
                        "skill_maturity_changed",
                        None,
                        &format!("{}: {:?} -> {:?}", p.skill, b.maturity, p.maturity),
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn get(&self, id: &CurriculumId) -> Result<Option<Curriculum>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM curricula WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|d| Ok(serde_json::from_str(&d)?)).transpose()
    }
    fn list(&self, q: CurriculumQuery) -> Result<Vec<Curriculum>> {
        let mut stmt = self.connection.prepare(
            "SELECT data FROM curricula WHERE (?1 IS NULL OR session_id=?1) ORDER BY created_at,id",
        )?;
        stmt.query_map([q.session_id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
}
fn children(tx: &Transaction<'_>, c: &Curriculum) -> Result<()> {
    for g in &c.goals {
        tx.execute("INSERT INTO curriculum_goals(id,curriculum_id,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![g.id.to_string(),c.id.to_string(),json(g)?])?;
        tx.execute(
            "INSERT INTO evidence_gaps(goal_id,data) VALUES(?1,?2) ON CONFLICT DO NOTHING",
            params![g.id.to_string(), json(&g.evidence_gap)?],
        )?;
    }
    for t in &c.trials {
        if !c.goals.iter().any(|g| g.id == t.goal_id) {
            return Err(Error::InvalidInput(
                "Trial references a foreign goal".into(),
            ));
        }
        tx.execute("INSERT INTO curriculum_trials(id,curriculum_id,goal_id,skill_id,fingerprint,data) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![t.id.to_string(),c.id.to_string(),t.goal_id.to_string(),t.skill_id.to_string(),t.fingerprint,json(t)?])?;
    }
    Ok(())
}
fn event(
    tx: &Transaction<'_>,
    c: &Curriculum,
    name: &str,
    trial: Option<&CurriculumTrialId>,
    message: &str,
) -> Result<()> {
    let e = CurriculumEvent {
        event: name.into(),
        curriculum_id: c.id.clone(),
        trial_id: trial.cloned(),
        message: message.chars().take(1024).collect(),
        created_at: chrono::Utc::now(),
    };
    tx.execute(
        "INSERT INTO curriculum_events(curriculum_id,data) VALUES(?1,?2)",
        params![c.id.to_string(), json(&e)?],
    )?;
    Ok(())
}
impl Store {
    pub fn link_curriculum_engine(
        &self,
        trial: &CurriculumTrialId,
        kind: &str,
        id: &str,
    ) -> Result<()> {
        match kind {
            "experiment" => {
                self.strategy_experiment(&id.parse()?)?;
            }
            "chaos" => {
                self.campaign(&id.parse()?)?;
            }
            "resilience_test" => {
                let parsed: ResilienceTestId = id.parse()?;
                if !self.resilience_tests()?.iter().any(|t| t.id == parsed) {
                    return Err(Error::NotFound(
                        "Referenced resilience test does not exist".into(),
                    ));
                }
            }
            _ => {
                return Err(Error::InvalidInput(
                    "Unsupported curriculum engine kind".into(),
                ));
            }
        }
        self.connection.execute(
            "INSERT INTO curriculum_engine_links(trial_id,kind,record_id) VALUES(?1,?2,?3)",
            params![trial.to_string(), kind, id],
        )?;
        Ok(())
    }
    pub fn curriculum_engine_link(
        &self,
        trial: &CurriculumTrialId,
    ) -> Result<Option<(String, String)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT kind,record_id FROM curriculum_engine_links WHERE trial_id=?1",
                [trial.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn curriculum(&self, id: &CurriculumId) -> Result<Curriculum> {
        CurriculumStore::get(self, id)?
            .ok_or_else(|| Error::NotFound(format!("Curriculum {id} not found")))
    }
    pub fn curriculum_events(
        &self,
        id: &CurriculumId,
        after: u64,
    ) -> Result<Vec<(u64, CurriculumEvent)>> {
        let mut stmt=self.connection.prepare("SELECT sequence,data FROM curriculum_events WHERE curriculum_id=?1 AND sequence>?2 ORDER BY sequence LIMIT 64")?;
        stmt.query_map(
            params![id.to_string(), after.min(i64::MAX as u64) as i64],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .map(|r| {
            let (s, d) = r?;
            Ok((s as u64, serde_json::from_str(&d)?))
        })
        .collect()
    }
    pub fn cancel_curriculum(&self, id: &CurriculumId) -> Result<bool> {
        // Serialize cancellation with executor admission. A plan with no worker must
        // reach a terminal state without requiring a later `run` just to cancel it.
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let data: String = tx.query_row(
            "SELECT data FROM curricula WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let mut c: Curriculum = serde_json::from_str(&data)?;
        if c.status.terminal() {
            return Ok(false);
        }
        if c.status == CurriculumStatus::Planned {
            c.status = CurriculumStatus::Cancelled;
            c.revision += 1;
            c.updated_at = chrono::Utc::now();
            c.stop_reason = Some("Cancelled before execution; no Reality was created".into());
            c.after = c.before.clone();
            tx.execute(
                "UPDATE curricula SET cancel_requested=1,status='cancelled',revision=?2,data=?3 WHERE id=?1",
                params![id.to_string(), c.revision, json(&c)?],
            )?;
            event(
                &tx,
                &c,
                "curriculum_cancelled",
                None,
                "Cancelled before execution",
            )?;
        } else {
            tx.execute(
                "UPDATE curricula SET cancel_requested=1 WHERE id=?1",
                [id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
    pub fn curriculum_cancel_requested(&self, id: &CurriculumId) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT cancel_requested FROM curricula WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn task_families(&self) -> Result<Vec<TaskFamily>> {
        self.list("SELECT data FROM task_families ORDER BY name,id")
    }
    pub fn task_family(&self, name: &str) -> Result<TaskFamily> {
        self.get(
            "SELECT data FROM task_families WHERE id=?1 OR name=?1",
            name,
        )
    }
    pub fn register_task_family(
        &self,
        name: &str,
        examples: Vec<ExperienceId>,
    ) -> Result<TaskFamily> {
        if name.is_empty() || name.len() > 120 || examples.is_empty() || examples.len() > 32 {
            return Err(Error::InvalidInput(
                "Task family needs a name and 1..32 examples".into(),
            ));
        }
        let source = self.experience(&examples[0])?;
        let selector = crate::lesson::ContextSelector::from_context(&source.context);
        for id in &examples {
            if !selector.matches(&self.experience(id)?.context) {
                return Err(Error::InvalidInput(
                    "Task family examples must match the explicit first-example context selector"
                        .into(),
                ));
            }
        }
        let family = TaskFamily {
            id: TaskFamilyId::new(),
            name: name.into(),
            selector,
            examples,
        };
        self.connection.execute(
            "INSERT INTO task_families(id,name,data) VALUES(?1,?2,?3)",
            params![family.id.to_string(), name, json(&family)?],
        )?;
        Ok(family)
    }
    pub fn save_skill_package(&self, p: &ExperiencePackage) -> Result<()> {
        let profile = p
            .coverage
            .profile
            .as_deref()
            .ok_or_else(|| Error::InvalidInput("Package has no profile".into()))?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO skill_coverage(skill_id,profile,data) VALUES(?1,?2,?3) ON CONFLICT(skill_id,profile) DO UPDATE SET data=excluded.data",params![p.skill.to_string(),profile,json(&p.coverage)?])?;
        tx.execute("INSERT INTO skill_usage(skill_id,data) VALUES(?1,?2) ON CONFLICT(skill_id) DO UPDATE SET data=excluded.data",params![p.skill.to_string(),json(&p.evidence.usage)?])?;
        tx.execute(
            "INSERT INTO experience_packages(skill_id,profile,created_at,data) VALUES(?1,?2,?3,?4)",
            params![
                p.skill.to_string(),
                profile,
                p.generated_at.to_rfc3339(),
                json(p)?
            ],
        )?;
        tx.commit()?;
        self.save_package_revision(p)?;
        Ok(())
    }
    pub(super) fn enrich_skill(
        &self,
        mut skill: crate::resilience::Skill,
    ) -> Result<crate::resilience::Skill> {
        let revisions = self.skill_revisions(&skill.id)?;
        if let Some(r) = revisions.last() {
            skill.procedure = r.procedure.clone();
            skill.context = r.context.clone();
            skill.evidence = r.evidence.clone();
            skill.source_experience = r.source_experience.clone();
            if r.revision > 1 {
                skill.maturity = crate::curriculum::SkillMaturity::Supported;
            }
        }
        skill.behavioral_contract = self.skill_contract_binding(&skill.id)?;
        let data:Option<String>=self.connection.query_row("SELECT data FROM experience_packages WHERE skill_id=?1 ORDER BY created_at DESC LIMIT 1",[skill.id.to_string()],|r|r.get(0)).optional()?;
        if let Some(data) = data {
            let p: ExperiencePackage = serde_json::from_str(&data)?;
            if revisions
                .last()
                .is_none_or(|r| r.revision == 1 || p.generated_at >= r.created_at)
            {
                skill.maturity = p.maturity;
                skill.coverage = p.coverage;
                skill.operating_envelope = p.operating_envelope;
            }
        }
        Ok(skill)
    }
    pub fn mark_curriculum_review(
        &self,
        lesson: &LessonId,
        trial: &CurriculumTrialId,
        reason: &str,
    ) -> Result<()> {
        self.connection.execute("INSERT INTO curriculum_reviews(lesson_id,trial_id,reason) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![lesson.to_string(),trial.to_string(),reason])?;
        Ok(())
    }
    pub fn curriculum_reviews(&self) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self
            .connection
            .prepare("SELECT lesson_id,trial_id,reason FROM curriculum_reviews ORDER BY rowid")?;
        Ok(stmt.query_map([],|r|Ok(serde_json::json!({"lesson_id":r.get::<_,String>(0)?,"trial_id":r.get::<_,String>(1)?,"reason":r.get::<_,String>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?)
    }
}
