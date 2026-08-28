// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Error, Result,
    application::ExperienceRelation,
    core::*,
    experience::{Experience, Outcome},
    lesson::{ActionPattern, ContextSelector, EvidenceRef, EvidenceRelationship},
    perturbation::Perturbation,
    resilience::*,
};
use chrono::Utc;
use rusqlite::{Transaction, TransactionBehavior, params};
use serde::Serialize;

fn json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn transaction(store: &Store) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        &store.connection,
        TransactionBehavior::Immediate,
    )?)
}

impl Store {
    pub fn perturbation(&self, id: &PerturbationId) -> Result<Perturbation> {
        self.get(
            "SELECT data FROM perturbations WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn campaigns(&self) -> Result<Vec<ChaosCampaign>> {
        let campaigns: Vec<ChaosCampaign> =
            self.list("SELECT data FROM chaos_campaigns ORDER BY created_at,id")?;
        campaigns.iter().map(|c| self.campaign(&c.id)).collect()
    }
    pub fn campaign(&self, id: &ChaosCampaignId) -> Result<ChaosCampaign> {
        let mut campaign: ChaosCampaign = self.get(
            "SELECT data FROM chaos_campaigns WHERE id=?1",
            &id.to_string(),
        )?;
        let mut query = self
            .connection
            .prepare("SELECT id FROM chaos_trials WHERE campaign_id=?1 ORDER BY trial_index")?;
        let ids = query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        campaign.control = None;
        campaign.trials.clear();
        for id in ids {
            let trial = self.chaos_trial(&id.parse()?)?;
            if trial.is_control {
                campaign.control = Some(trial)
            } else {
                campaign.trials.push(trial)
            }
        }
        Ok(campaign)
    }
    pub fn chaos_trial(&self, id: &ChaosTrialId) -> Result<ChaosTrial> {
        let mut trial: ChaosTrial =
            self.get("SELECT data FROM chaos_trials WHERE id=?1", &id.to_string())?;
        let mut query = self.connection.prepare(
            "SELECT lesson_id FROM chaos_trial_lessons WHERE trial_id=?1 ORDER BY lesson_id",
        )?;
        trial.lessons = query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| r?.parse())
            .collect::<Result<_>>()?;
        trial.reflexes = self
            .reflexes()?
            .into_iter()
            .filter(|r| r.source_trial == *id)
            .map(|r| r.id)
            .collect();
        trial.recoveries = self
            .recoveries()?
            .into_iter()
            .filter(|r| r.source_trial == *id)
            .map(|r| r.id)
            .collect();
        Ok(trial)
    }
    pub fn envelopes(&self) -> Result<Vec<OperatingEnvelope>> {
        self.list("SELECT data FROM operating_envelopes ORDER BY rowid")
    }
    pub fn envelope(&self, id: &OperatingEnvelopeId) -> Result<OperatingEnvelope> {
        self.get(
            "SELECT data FROM operating_envelopes WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn reflexes(&self) -> Result<Vec<Reflex>> {
        self.list("SELECT data FROM reflexes ORDER BY rowid")
    }
    pub fn reflex(&self, id: &ReflexId) -> Result<Reflex> {
        self.get("SELECT data FROM reflexes WHERE id=?1", &id.to_string())
    }
    pub fn recoveries(&self) -> Result<Vec<Recovery>> {
        self.list("SELECT data FROM recoveries ORDER BY rowid")
    }
    pub fn recovery(&self, id: &RecoveryId) -> Result<Recovery> {
        self.get("SELECT data FROM recoveries WHERE id=?1", &id.to_string())
    }
    pub fn resilience_tests(&self) -> Result<Vec<ResilienceTest>> {
        self.list("SELECT data FROM resilience_tests ORDER BY created_at,id")
    }
    pub fn skills(&self) -> Result<Vec<Skill>> {
        self.list("SELECT data FROM skills ORDER BY name,id")
    }
    pub fn skill(&self, name: &str) -> Result<Skill> {
        self.get("SELECT data FROM skills WHERE id=?1 OR name=?1", name)
    }
    pub fn register_skill(&self, name: &str, source: &ExperienceId) -> Result<Skill> {
        if name.trim().is_empty() || name.len() > 120 || name.starts_with("skill-") {
            return Err(Error::InvalidInput(
                "Choose a nonempty Skill name of at most 120 bytes (not an ID)".into(),
            ));
        }
        let exp = self.experience(source)?;
        if exp.outcome != Outcome::Success
            || exp.resilience.as_ref().is_some_and(|r| {
                r.outcome != ChaosTrialOutcome::Pass
                    || !r.perturbation_ids.is_empty()
                    || !r.reflex_matches.is_empty()
                    || r.recovery_attempt.is_some()
            })
        {
            return Err(Error::InvalidInput(
                "A Skill requires an observed successful procedure".into(),
            ));
        }
        let replay = exp.replay.as_ref().ok_or_else(|| {
            Error::InvalidInput("Skill registration requires an explicit replay script".into())
        })?;
        let mut scope = ContextSelector::from_context(&exp.context);
        scope.tags = exp.context.tags.clone();
        let skill = Skill { id: SkillId::new(), name: name.into(), description: "Manually registered procedure with one local successful observation; replication remains untested".into(), context: scope, procedure: vec![ActionPattern::shell(&replay.script)], evidence: vec![EvidenceRef::Experience { experience_id: exp.id.clone(), relationship: EvidenceRelationship::Supports }], status: SkillStatus::Supported, operating_envelope: None, source_experience: exp.id };
        self.connection.execute(
            "INSERT INTO skills(id,name,source_experience_id,data) VALUES(?1,?2,?3,?4)",
            params![
                skill.id.to_string(),
                name,
                source.to_string(),
                json(&skill)?
            ],
        )?;
        Ok(skill)
    }
    pub(crate) fn register_perturbations(&self, values: &[Perturbation]) -> Result<()> {
        let tx = transaction(self)?;
        for p in values {
            p.validate()?;
            tx.execute(
                "INSERT INTO perturbations(id,data) VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",
                params![p.id.to_string(), json(p)?],
            )?;
            if self.perturbation(&p.id)? != *p {
                return Err(Error::InvalidInput(
                    "Perturbation ID already identifies different parameters".into(),
                ));
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn insert_campaign(&self, campaign: &ChaosCampaign) -> Result<()> {
        let tx = transaction(self)?;
        let skill = match &campaign.plan.target {
            ChaosTarget::Skill(id) => Some(id.to_string()),
            _ => None,
        };
        tx.execute("INSERT INTO chaos_campaigns(id,created_at,skill_id,status,data) VALUES(?1,?2,?3,'running',?4)",params![campaign.id.to_string(),campaign.created_at.to_rfc3339(),skill,json(campaign)?])?;
        for (i, conditions) in campaign.plan.perturbations.iter().enumerate() {
            for p in conditions {
                p.validate()?;
                tx.execute(
                    "INSERT INTO perturbations(id,data) VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",
                    params![p.id.to_string(), json(p)?],
                )?;
                if self.perturbation(&p.id)? != *p {
                    return Err(Error::InvalidInput("Perturbation ID conflict".into()));
                }
                tx.execute("INSERT INTO campaign_perturbations(campaign_id,perturbation_id,trial_index) VALUES(?1,?2,?3)",params![campaign.id.to_string(),p.id.to_string(),(i+1) as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn update_campaign(
        &self,
        campaign: &ChaosCampaign,
        envelope: Option<&OperatingEnvelope>,
    ) -> Result<()> {
        let tx = transaction(self)?;
        if let Some(envelope) = envelope {
            tx.execute(
                "INSERT INTO operating_envelopes(id,campaign_id,version,data) VALUES(?1,?2,?3,?4)",
                params![
                    envelope.id.to_string(),
                    campaign.id.to_string(),
                    envelope.version,
                    json(envelope)?
                ],
            )?;
            tx.execute("INSERT INTO operating_envelope_versions(envelope_id,version,data) VALUES(?1,?2,?3)",params![envelope.id.to_string(),envelope.version,json(envelope)?])?;
            for condition in &envelope.tested_conditions {
                let trial = self.chaos_trial(&condition.trial_id)?;
                if trial.campaign_id != campaign.id
                    || trial.experience_id != condition.experience_id
                    || trial.outcome != condition.outcome
                {
                    return Err(Error::InvalidInput(
                        "Envelope observation does not match trial evidence".into(),
                    ));
                }
                tx.execute("INSERT INTO operating_envelope_observations(envelope_id,trial_id,experience_id,outcome,data) VALUES(?1,?2,?3,?4,?5)",params![envelope.id.to_string(),condition.trial_id.to_string(),condition.experience_id.to_string(),json(&condition.outcome)?,json(condition)?])?;
            }
        }
        let changed = tx.execute(
            "UPDATE chaos_campaigns SET status=?2,data=?3 WHERE id=?1 AND status='running'",
            params![
                campaign.id.to_string(),
                json(&campaign.result)?.trim_matches('"'),
                json(campaign)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention("Campaign is already terminal".into()));
        }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn link_chaos_lesson(&self, trial: &ChaosTrialId, lesson: &LessonId) -> Result<()> {
        let t = self.chaos_trial(trial)?;
        let l = self.lesson(lesson)?;
        if l.source_experience != t.experience_id {
            return Err(Error::InvalidInput(
                "Lesson source must be this chaos trial".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO chaos_trial_lessons(trial_id,lesson_id) VALUES(?1,?2)",
            params![trial.to_string(), lesson.to_string()],
        )?;
        Ok(())
    }
    pub(crate) fn insert_reflex(&self, reflex: &Reflex) -> Result<()> {
        if reflex.status != ReflexStatus::Candidate
            || reflex.response == ReflexResponse::Block
            || reflex.version != 1
            || reflex.source_lessons.is_empty()
        {
            return Err(Error::InvalidInput(
                "Only non-blocking Candidate Reflexes can be proposed".into(),
            ));
        }
        let source = self.chaos_trial(&reflex.source_trial)?;
        let tx = transaction(self)?;
        tx.execute(
            "INSERT INTO reflexes(id,source_trial,version,data) VALUES(?1,?2,1,?3)",
            params![reflex.id.to_string(), source.id.to_string(), json(reflex)?],
        )?;
        for id in &reflex.source_lessons {
            if self.lesson(id)?.source_experience != source.experience_id {
                return Err(Error::InvalidInput(
                    "Reflex Lesson does not originate in its trial".into(),
                ));
            }
            tx.execute(
                "INSERT INTO reflex_lessons(reflex_id,lesson_id) VALUES(?1,?2)",
                params![reflex.id.to_string(), id.to_string()],
            )?;
        }
        write_reflex_version(&tx, reflex)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn insert_recovery(&self, recovery: &Recovery) -> Result<()> {
        if recovery.status != RecoveryStatus::Candidate
            || recovery.version != 1
            || recovery.steps.is_empty()
            || recovery.steps.len() > 16
        {
            return Err(Error::InvalidInput(
                "Recovery must begin as a bounded Candidate procedure".into(),
            ));
        }
        let tx = transaction(self)?;
        tx.execute(
            "INSERT INTO recoveries(id,source_trial,version,data) VALUES(?1,?2,1,?3)",
            params![
                recovery.id.to_string(),
                recovery.source_trial.to_string(),
                json(recovery)?
            ],
        )?;
        write_recovery_version(&tx, recovery)?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_reflex_enabled(&self, id: &ReflexId, enabled: bool) -> Result<Reflex> {
        let tx = transaction(self)?;
        let mut reflex = self.reflex(id)?;
        if enabled {
            if !matches!(
                reflex.status,
                ReflexStatus::Supported | ReflexStatus::Disabled | ReflexStatus::Active
            ) || reflex.response == ReflexResponse::Block
            {
                return Err(Error::Intervention(
                    "Only supported non-blocking Reflexes can be enabled".into(),
                ));
            }
            let tests = self.resilience_tests()?;
            if !tests.iter().any(|t| {
                t.reflex_id.as_ref() == Some(id) && t.status == ResilienceTestStatus::Supported
            }) || tests.iter().any(|t| {
                t.reflex_id.as_ref() == Some(id)
                    && matches!(
                        t.status,
                        ResilienceTestStatus::FalsePositive | ResilienceTestStatus::Contradicted
                    )
            }) {
                return Err(Error::Intervention("Unresolved false-positive/contradiction evidence prevents activation; propose a narrower rule".into()));
            }
        } else if !matches!(
            reflex.status,
            ReflexStatus::Active | ReflexStatus::Supported | ReflexStatus::Disabled
        ) {
            return Err(Error::Intervention(
                "Only a supported or active Reflex can be disabled".into(),
            ));
        }
        let next = if enabled {
            ReflexStatus::Active
        } else {
            ReflexStatus::Disabled
        };
        if reflex.status != next {
            reflex.status = next;
            reflex.version += 1;
            reflex.updated_at = Utc::now();
            update_reflex(&tx, &reflex)?;
        }
        tx.commit()?;
        Ok(reflex)
    }
    pub(crate) fn save_resilience_test(&self, test: &ResilienceTest, new: bool) -> Result<()> {
        if new {
            self.connection.execute("INSERT INTO resilience_tests(id,created_at,reflex_id,recovery_id,source_trial,status,data) VALUES(?1,?2,?3,?4,?5,'running',?6)",params![test.id.to_string(),test.created_at.to_rfc3339(),test.reflex_id.as_ref().map(ToString::to_string),test.recovery_id.as_ref().map(ToString::to_string),test.source_trial.to_string(),json(test)?])?;
        } else {
            write_test(&self.connection, test)?;
        }
        Ok(())
    }
    pub(crate) fn finish_resilience_test(&self, test: &ResilienceTest) -> Result<()> {
        let tx = transaction(self)?;
        let evidence: Vec<_> =
            test.without
                .iter()
                .chain(test.with.iter())
                .map(|id| EvidenceRef::Experience {
                    experience_id: id.clone(),
                    relationship: match test.status {
                        ResilienceTestStatus::Supported => EvidenceRelationship::Supports,
                        ResilienceTestStatus::Contradicted
                        | ResilienceTestStatus::FalsePositive => EvidenceRelationship::Contradicts,
                        _ => EvidenceRelationship::Inconclusive,
                    },
                })
                .collect();
        if let Some(id) = &test.reflex_id {
            let mut reflex = self.reflex(id)?;
            match test.status {
                ResilienceTestStatus::Supported if reflex.status == ReflexStatus::Candidate => {
                    reflex.status = ReflexStatus::Supported;
                    reflex.confidence = 0.84.try_into()?;
                }
                ResilienceTestStatus::FalsePositive | ResilienceTestStatus::Contradicted
                    if reflex.status != ReflexStatus::Retired =>
                {
                    reflex.status = ReflexStatus::Disabled;
                    reflex.confidence = 0.30.try_into()?;
                }
                _ => {}
            }
            reflex.evidence.extend(evidence);
            reflex.version += 1;
            reflex.updated_at = Utc::now();
            update_reflex(&tx, &reflex)?;
        } else if let Some(id) = &test.recovery_id {
            let mut recovery = self.recovery(id)?;
            match test.status {
                ResilienceTestStatus::Supported if recovery.status == RecoveryStatus::Candidate => {
                    recovery.status = RecoveryStatus::Supported;
                    recovery.confidence = 0.81.try_into()?;
                }
                ResilienceTestStatus::Contradicted
                    if recovery.status != RecoveryStatus::Retired =>
                {
                    recovery.status = RecoveryStatus::Contradicted;
                    recovery.confidence = 0.25.try_into()?;
                }
                _ => {}
            }
            recovery.evidence.extend(evidence);
            recovery.version += 1;
            recovery.updated_at = Utc::now();
            tx.execute(
                "UPDATE recoveries SET version=?2,data=?3 WHERE id=?1 AND version=?4",
                params![
                    id.to_string(),
                    recovery.version,
                    json(&recovery)?,
                    recovery.version - 1
                ],
            )?;
            write_recovery_version(&tx, &recovery)?;
        }
        write_test(&tx, test)?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn insert_resilience(&self, tx: &Transaction<'_>, exp: &Experience) -> Result<()> {
        let Some(observation) = &exp.resilience else {
            return Ok(());
        };
        for id in &observation.perturbation_ids {
            tx.execute(
                "INSERT INTO experience_perturbations(experience_id,perturbation_id) VALUES(?1,?2)",
                params![exp.id.to_string(), id.to_string()],
            )?;
        }
        if let Some(origin) = &observation.origin {
            let campaign = self.campaign(&origin.campaign_id)?;
            if campaign.result != CampaignStatus::Running
                || campaign.plan.starting_state != exp.starting_state
                || campaign.plan.goal != exp.goal
                || campaign.plan.evaluation != exp.evaluation.spec
                || campaign.plan.agent != exp.agent
                || campaign.plan.environment.fingerprint != exp.context.environment.fingerprint
            {
                return Err(Error::InvalidInput(
                    "Chaos Experience does not match its running campaign".into(),
                ));
            }
            let perturbations = observation
                .perturbation_ids
                .iter()
                .map(|id| self.perturbation(id))
                .collect::<Result<Vec<_>>>()?;
            if origin.index == 0 {
                if origin.control.is_some() || !perturbations.is_empty() {
                    return Err(Error::InvalidInput(
                        "Control cannot contain perturbations".into(),
                    ));
                }
            } else {
                let control = campaign
                    .control
                    .as_ref()
                    .ok_or_else(|| Error::InvalidInput("Chaos needs a healthy control".into()))?;
                if control.outcome != ChaosTrialOutcome::Pass
                    || origin.control.as_ref() != Some(&control.experience_id)
                    || !exp.relations.contains(&ExperienceRelation::ChaosVariantOf(
                        control.experience_id.clone(),
                    ))
                    || campaign.plan.perturbations.get(origin.index - 1) != Some(&perturbations)
                    || origin.index > campaign.plan.trial_budget
                {
                    return Err(Error::InvalidInput(
                        "Chaos variant lacks healthy control, planned conditions, or budget".into(),
                    ));
                }
            }
            let trial = ChaosTrial {
                id: origin.trial_id.clone(),
                campaign_id: origin.campaign_id.clone(),
                is_control: origin.index == 0,
                index: origin.index,
                reality_id: exp.reality_id.clone(),
                experience_id: exp.id.clone(),
                execution_id: exp.execution_id.clone(),
                evaluation_id: exp.evaluation.id.clone(),
                perturbations,
                outcome: observation.outcome,
                metrics: observation.metrics.clone(),
                failure_signatures: exp
                    .failure_signatures
                    .iter()
                    .map(|s| s.signature.clone())
                    .collect(),
                lessons: vec![],
                reflexes: vec![],
                recoveries: vec![],
            };
            tx.execute("INSERT INTO chaos_trials(id,campaign_id,trial_index,experience_id,control_experience_id,reality_id,execution_id,evaluation_id,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![trial.id.to_string(),trial.campaign_id.to_string(),trial.index as i64,exp.id.to_string(),origin.control.as_ref().map(ToString::to_string),exp.reality_id.to_string(),exp.execution_id.to_string(),exp.evaluation.id.to_string(),json(&trial)?])?;
        }
        for (i, m) in observation.reflex_matches.iter().enumerate() {
            let data: String = self.connection.query_row(
                "SELECT data FROM reflex_versions WHERE reflex_id=?1 AND version=?2",
                params![m.reflex_id.to_string(), m.reflex_version],
                |r| r.get(0),
            )?;
            let reflex: Reflex = serde_json::from_str(&data)?;
            if !reflex.trigger.context.matches(&exp.context)
                || json(&reflex.trigger)? != json(&m.trigger)?
                || reflex.response != m.response
                || reflex.source_lessons != m.source_lessons
                || (!m.test_only && reflex.status != ReflexStatus::Active)
            {
                return Err(Error::InvalidInput(
                    "Reflex match does not match versioned scope and policy".into(),
                ));
            }
            tx.execute("INSERT INTO reflex_matches(experience_id,position,reflex_id,reflex_version,data) VALUES(?1,?2,?3,?4,?5)",params![exp.id.to_string(),i as i64,m.reflex_id.to_string(),m.reflex_version,json(m)?])?;
        }
        if let Some(attempt) = &observation.recovery_attempt {
            tx.execute("INSERT INTO recovery_attempts(experience_id,recovery_id,recovery_version,data) VALUES(?1,?2,?3,?4)",params![exp.id.to_string(),attempt.recovery_id.to_string(),attempt.recovery_version,json(attempt)?])?;
        }
        Ok(())
    }
}
fn write_test(connection: &rusqlite::Connection, test: &ResilienceTest) -> Result<()> {
    let n=connection.execute("UPDATE resilience_tests SET without_experience_id=?2,with_experience_id=?3,status=?4,data=?5 WHERE id=?1 AND status='running'",params![test.id.to_string(),test.without.as_ref().map(ToString::to_string),test.with.as_ref().map(ToString::to_string),json(&test.status)?.trim_matches('"'),json(test)?])?;
    if n != 1 {
        return Err(Error::Intervention("Test is already terminal".into()));
    }
    Ok(())
}
fn write_evidence(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    id: &str,
    evidence: &[EvidenceRef],
) -> Result<()> {
    for e in evidence {
        if let EvidenceRef::Experience {
            experience_id,
            relationship,
        } = e
        {
            tx.execute(&format!("INSERT INTO {table}({column},experience_id,relationship) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING"),params![id,experience_id.to_string(),json(relationship)?])?;
        } else {
            return Err(Error::InvalidInput(
                "Resilience evidence must identify an immutable Experience".into(),
            ));
        }
    }
    Ok(())
}
fn write_reflex_version(tx: &Transaction<'_>, reflex: &Reflex) -> Result<()> {
    tx.execute(
        "INSERT INTO reflex_versions(reflex_id,version,data) VALUES(?1,?2,?3)",
        params![reflex.id.to_string(), reflex.version, json(reflex)?],
    )?;
    write_evidence(
        tx,
        "reflex_evidence",
        "reflex_id",
        &reflex.id.to_string(),
        &reflex.evidence,
    )
}
fn update_reflex(tx: &Transaction<'_>, reflex: &Reflex) -> Result<()> {
    let n = tx.execute(
        "UPDATE reflexes SET version=?2,data=?3 WHERE id=?1 AND version=?4",
        params![
            reflex.id.to_string(),
            reflex.version,
            json(reflex)?,
            reflex.version - 1
        ],
    )?;
    if n != 1 {
        return Err(Error::Intervention("Concurrent Reflex revision".into()));
    }
    write_reflex_version(tx, reflex)
}
fn write_recovery_version(tx: &Transaction<'_>, recovery: &Recovery) -> Result<()> {
    tx.execute(
        "INSERT INTO recovery_versions(recovery_id,version,data) VALUES(?1,?2,?3)",
        params![recovery.id.to_string(), recovery.version, json(recovery)?],
    )?;
    for (i, step) in recovery.steps.iter().enumerate() {
        tx.execute("INSERT INTO recovery_steps(recovery_id,recovery_version,position,data) VALUES(?1,?2,?3,?4)",params![recovery.id.to_string(),recovery.version,i as i64,json(step)?])?;
    }
    write_evidence(
        tx,
        "recovery_evidence",
        "recovery_id",
        &recovery.id.to_string(),
        &recovery.evidence,
    )
}
