// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Error, Result,
    core::*,
    development::*,
    experience::Outcome,
    lesson::{ActionPattern, EvidenceRef, EvidenceRelationship},
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
fn json(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn sql_count(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InvalidInput("Count exceeds SQLite integer range".into()))
}
impl Store {
    pub fn read_projection<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
        let result = f(self)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn development_observations(&self) -> Result<Vec<DevelopmentObservation>> {
        self.list("SELECT data FROM development_observations ORDER BY created_at,id")
    }
    pub fn all_lessons(&self) -> Result<Vec<crate::lesson::Lesson>> {
        self.list("SELECT data FROM lessons ORDER BY created_at,id")
    }
    pub fn lesson_freshness_bases(
        &self,
        lessons: &[crate::lesson::Lesson],
    ) -> Result<std::collections::HashMap<LessonId, FreshnessBasis>> {
        let linked:Vec<DevelopmentObservation>=self.list("SELECT data FROM development_observations WHERE id IN (SELECT source_experience FROM lessons UNION SELECT coalesce(l.experience_id,t.experience_id) FROM lesson_evidence l LEFT JOIN trials t ON t.id=l.trial_id WHERE l.relationship='supports')")?;
        let observations: std::collections::HashMap<_, _> =
            linked.into_iter().map(|e| (e.id.clone(), e)).collect();
        let mut result = std::collections::HashMap::new();
        for lesson in lessons {
            let support = self.lesson_support_experiences(&lesson.id)?;
            if let Some(basis) = crate::development::lesson_basis(lesson, &observations, &support) {
                result.insert(lesson.id.clone(), basis);
            }
        }
        Ok(result)
    }
    pub(crate) fn reflex_freshness_observations(&self) -> Result<Vec<DevelopmentObservation>> {
        self.list("SELECT data FROM development_observations WHERE id IN (SELECT t.experience_id FROM reflexes r JOIN chaos_trials t ON t.id=r.source_trial UNION SELECT experience_id FROM reflex_evidence WHERE relationship='supports')")
    }
    pub fn latest_packages(&self) -> Result<Vec<crate::curriculum::ExperiencePackage>> {
        self.list("SELECT p.data FROM experience_packages p WHERE p.created_at=(SELECT MAX(q.created_at) FROM experience_packages q WHERE q.skill_id=p.skill_id AND q.profile=p.profile) ORDER BY p.skill_id,p.profile")
    }
    pub fn lesson_support_experiences(&self, id: &LessonId) -> Result<Vec<ExperienceId>> {
        let mut s=self.connection.prepare("SELECT coalesce(l.experience_id,t.experience_id) FROM lesson_evidence l LEFT JOIN trials t ON t.id=l.trial_id WHERE l.lesson_id=?1 AND l.relationship='supports'")?;
        s.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| r?.parse())
            .collect()
    }
    pub fn profile_cache(&self, p: &ExperienceProfile) -> Result<()> {
        self.connection.execute("INSERT INTO experience_profiles(id,subject,policy_hash,updated_at,data) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET policy_hash=excluded.policy_hash,updated_at=excluded.updated_at,data=excluded.data",params![p.id.to_string(),json(&p.subject)?,p.policy_hash,p.updated_at.to_rfc3339(),json(p)?])?;
        Ok(())
    }
    pub fn save_profile_snapshot(&self, p: &ExperienceProfile) -> Result<ProfileSnapshot> {
        self.profile_cache(p)?;
        let snapshot = snapshot(p);
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO profile_snapshots(id,profile_id,captured_at,data) VALUES(?1,?2,?3,?4)",
            params![
                snapshot.id.to_string(),
                p.id.to_string(),
                snapshot.captured_at.to_rfc3339(),
                json(&snapshot)?
            ],
        )?;
        for id in &snapshot.evidence_ids {
            tx.execute(
                "INSERT INTO snapshot_evidence(snapshot_id,experience_id) VALUES(?1,?2)",
                params![snapshot.id.to_string(), id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(snapshot)
    }
    pub fn profile_history(&self, id: &ExperienceProfileId) -> Result<Vec<ProfileSnapshot>> {
        let mut s = self.connection.prepare(
            "SELECT data FROM profile_snapshots WHERE profile_id=?1 ORDER BY captured_at,id",
        )?;
        s.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn profile_snapshot(&self, id: &ProfileSnapshotId) -> Result<ProfileSnapshot> {
        self.get(
            "SELECT data FROM profile_snapshots WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn save_regressions(&self, r: &GrowthReport) -> Result<()> {
        for g in &r.regressions {
            self.connection.execute("INSERT INTO development_regressions(from_snapshot,to_snapshot,metric,data) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![r.from.to_string(),r.to.to_string(),json(&g.metric)?,json(g)?])?;
        }
        Ok(())
    }
    pub fn skill_revisions(&self, id: &SkillId) -> Result<Vec<SkillRevision>> {
        let mut s = self
            .connection
            .prepare("SELECT data FROM skill_revisions WHERE skill_id=?1 ORDER BY revision")?;
        s.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn revise_skill(&self, name: &str, source: &ExperienceId) -> Result<SkillRevision> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let skill = self.skill(name)?;
        let e = self.experience(source)?;
        if e.outcome != Outcome::Success
            || e.evaluation.spec.checks.is_empty()
            || !skill.context.matches(&e.context)
            || !e.perturbations.is_empty()
            || e.resilience.as_ref().is_some_and(|r| {
                !r.perturbation_ids.is_empty()
                    || !r.reflex_matches.is_empty()
                    || r.recovery_attempt.is_some()
            })
        {
            return Err(Error::InvalidInput(
                "A Skill revision requires successful evaluated evidence inside the existing scope"
                    .into(),
            ));
        }
        let script = e.replay.as_ref().ok_or_else(|| {
            Error::InvalidInput("Revision requires a replayable procedure".into())
        })?;
        let previous = self
            .skill_revisions(&skill.id)?
            .last()
            .map(|r| r.revision)
            .unwrap_or(0);
        let r = SkillRevision {
            skill_id: skill.id,
            revision: previous + 1,
            created_at: Utc::now(),
            procedure: vec![ActionPattern::shell(&script.script)],
            context: skill.context,
            evidence: vec![EvidenceRef::Experience {
                experience_id: source.clone(),
                relationship: EvidenceRelationship::Supports,
            }],
            parent_revision: Some(previous),
            source_experience: source.clone(),
            behavioral_contract: skill.behavioral_contract,
        };
        tx.execute("INSERT INTO skill_revisions(skill_id,revision,created_at,source_experience,data) VALUES(?1,?2,?3,?4,?5)",params![r.skill_id.to_string(),sql_count(r.revision)?,r.created_at.to_rfc3339(),source.to_string(),json(&r)?])?;
        tx.commit()?;
        Ok(r)
    }
    pub fn save_package_revision(
        &self,
        p: &crate::curriculum::ExperiencePackage,
    ) -> Result<ExperiencePackageRevision> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let skill_revision = self
            .skill_revisions(&p.skill)?
            .last()
            .map(|r| r.revision)
            .ok_or_else(|| Error::NotFound("Skill revision missing".into()))?;
        let package_id: ExperiencePackageId =
            stable_id("package-", &(&p.skill, &p.coverage.profile))?.parse()?;
        let mut canonical = serde_json::to_value(p)?;
        canonical
            .as_object_mut()
            .expect("package object")
            .remove("generated_at");
        let evidence_hash = blake3::hash(&serde_json::to_vec(&(skill_revision, canonical))?)
            .to_hex()
            .to_string();
        let previous:Option<String>=tx.query_row("SELECT data FROM experience_package_revisions WHERE package_id=?1 ORDER BY revision DESC LIMIT 1",[package_id.to_string()],|r|r.get(0)).optional()?;
        let previous: Option<ExperiencePackageRevision> =
            previous.map(|s| serde_json::from_str(&s)).transpose()?;
        if let Some(old) = &previous
            && old.evidence_hash == evidence_hash
        {
            return Ok(old.clone());
        }
        let items = p
            .provenance
            .iter()
            .filter_map(|p| {
                p.version.map(|v| ExperienceRef {
                    kind: p.kind.clone(),
                    id: p.id.clone(),
                    revision: v as u64,
                })
            })
            .collect();
        let revision = ExperiencePackageRevision {
            package_id,
            skill_id: p.skill.clone(),
            revision: previous.map(|r| r.revision + 1).unwrap_or(1),
            created_at: p.generated_at,
            skill_revision,
            items,
            package: p.clone(),
            evidence_hash,
        };
        tx.execute("INSERT INTO experience_package_revisions(package_id,skill_id,revision,skill_revision,created_at,evidence_hash,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![revision.package_id.to_string(),p.skill.to_string(),sql_count(revision.revision)?,sql_count(skill_revision)?,revision.created_at.to_rfc3339(),revision.evidence_hash,json(&revision)?])?;
        tx.commit()?;
        Ok(revision)
    }
    pub fn package_revisions(
        &self,
        skill: &SkillId,
        profile: &str,
    ) -> Result<Vec<ExperiencePackageRevision>> {
        let id = stable_id("package-", &(skill, Some(profile)))?;
        let mut s = self.connection.prepare(
            "SELECT data FROM experience_package_revisions WHERE package_id=?1 ORDER BY revision",
        )?;
        s.query_map([id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn revalidations(&self) -> Result<Vec<RevalidationItem>> {
        self.list("SELECT data FROM revalidation_queue ORDER BY created_at,id")
    }
    pub fn enqueue_revalidation(&self, item: &RevalidationItem) -> Result<()> {
        let key = stable_id(
            "",
            &(
                &item.item,
                &item.reason,
                &item.context.repository,
                &item.context.environment.fingerprint,
            ),
        )?;
        self.connection.execute("INSERT INTO revalidation_queue(id,dedup_key,created_at,status,data) VALUES(?1,?2,?3,'pending',?4) ON CONFLICT(dedup_key) DO NOTHING",params![item.id.to_string(),key,item.created_at.to_rfc3339(),json(item)?])?;
        Ok(())
    }
    pub fn finish_revalidation(&self, item: &RevalidationItem) -> Result<()> {
        if item.experiment_id.is_none() || item.status == "pending" {
            return Err(Error::InvalidInput(
                "Revalidation needs recorded engine evidence".into(),
            ));
        }
        self.experiment(item.experiment_id.as_ref().expect("checked experiment"))?;
        if self.connection.execute(
            "UPDATE revalidation_queue SET status=?2,data=?3 WHERE id=?1 AND status='pending'",
            params![item.id.to_string(), item.status, json(item)?],
        )? != 1
        {
            return Err(Error::Intervention(
                "Revalidation changed or is terminal".into(),
            ));
        }
        Ok(())
    }
    pub fn episodes(&self) -> Result<Vec<DevelopmentEpisode>> {
        self.list("SELECT data FROM development_episodes ORDER BY started_at,id")
    }
    pub fn save_episode(&self, e: &DevelopmentEpisode, new: bool) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if new {
            if e.ended_at.is_some() {
                return Err(Error::InvalidInput("Episode must begin open".into()));
            }
            tx.execute(
                "INSERT INTO development_episodes(id,started_at,data) VALUES(?1,?2,?3)",
                params![e.id.to_string(), e.started_at.to_rfc3339(), json(e)?],
            )?;
        } else {
            if e.ended_at.is_none() {
                return Err(Error::InvalidInput(
                    "Episode finish needs an end time".into(),
                ));
            }
            if tx.execute("UPDATE development_episodes SET ended_at=?2,data=?3 WHERE id=?1 AND ended_at IS NULL",params![e.id.to_string(),e.ended_at.map(|t|t.to_rfc3339()),json(e)?])?!=1{return Err(Error::Intervention("Episode already ended".into()));}
        }
        for id in &e.experiences {
            tx.execute("INSERT INTO episode_evidence(episode_id,experience_id) VALUES(?1,?2) ON CONFLICT DO NOTHING",params![e.id.to_string(),id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn benchmark_runs(&self) -> Result<Vec<BenchmarkResult>> {
        self.list("SELECT data FROM benchmark_runs ORDER BY created_at,id")
    }
    pub fn save_benchmark(&self, b: &BenchmarkResult, new: bool) -> Result<()> {
        if new {
            self.connection.execute(
                "INSERT INTO benchmark_runs(id,created_at,status,data) VALUES(?1,?2,?3,?4)",
                params![
                    b.id.to_string(),
                    b.created_at.to_rfc3339(),
                    b.status,
                    json(b)?
                ],
            )?;
        } else {
            let changed = self.connection.execute(
                "UPDATE benchmark_runs SET status=?2,data=?3 WHERE id=?1 AND status='running'",
                params![b.id.to_string(), b.status, json(b)?],
            )?;
            if changed != 1 {
                return Err(Error::Intervention(
                    "Benchmark is missing or terminal".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn save_benchmark_metric(
        &self,
        id: &BenchmarkRunId,
        arm: &str,
        episode: u32,
        metric: &str,
        n: u64,
        value: Option<f64>,
    ) -> Result<()> {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(Error::InvalidInput("Metric must be finite".into()));
        }
        self.connection.execute("INSERT INTO benchmark_metrics(run_id,arm,episode,metric,sample_count,value) VALUES(?1,?2,?3,?4,?5,?6)",params![id.to_string(),arm,episode,metric,sql_count(n)?,value])?;
        Ok(())
    }
    pub fn development_timeline(&self, limit: usize) -> Result<Vec<TimelineEvent>> {
        let mut s=self.connection.prepare("SELECT at,kind,id,revision,experience_id,description FROM (
 SELECT created_at AS at,'experience' AS kind,id,NULL AS revision,id AS experience_id,json_extract(data,'$.outcome') AS description FROM experiences
 UNION ALL SELECT json_extract(data,'$.updated_at'),'lesson_revision',lesson_id,version,json_extract(data,'$.source_experience'),json_extract(data,'$.status') FROM lesson_versions
 UNION ALL SELECT created_at,'skill_revision',skill_id,revision,source_experience,'procedure revision' FROM skill_revisions
 UNION ALL SELECT p.created_at,'package_revision',p.package_id,p.revision,s.source_experience,json_extract(p.data,'$.package.maturity') FROM experience_package_revisions p JOIN skill_revisions s ON s.skill_id=p.skill_id AND s.revision=p.skill_revision
 UNION ALL SELECT json_extract(v.data,'$.updated_at'),'reflex_revision',v.reflex_id,v.version,t.experience_id,json_extract(v.data,'$.status') FROM reflex_versions v JOIN chaos_trials t ON t.id=json_extract(v.data,'$.source_trial')
 UNION ALL SELECT json_extract(v.data,'$.updated_at'),'recovery_revision',v.recovery_id,v.version,t.experience_id,json_extract(v.data,'$.status') FROM recovery_versions v JOIN chaos_trials t ON t.id=json_extract(v.data,'$.source_trial')
 UNION ALL SELECT coalesce(ended_at,started_at),'episode',id,NULL,NULL,CASE WHEN ended_at IS NULL THEN 'started' ELSE 'completed' END FROM development_episodes
 UNION ALL SELECT created_at,'benchmark',id,NULL,NULL,status FROM benchmark_runs
 ) ORDER BY at DESC,id DESC LIMIT ?1")?;
        s.query_map([limit.min(20000) as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .map(|row| {
            let (at, kind, id, revision, e, description) = row?;
            Ok(TimelineEvent {
                at: chrono::DateTime::parse_from_rfc3339(&at)
                    .map_err(|e| Error::InvalidInput(e.to_string()))?
                    .with_timezone(&Utc),
                kind,
                id,
                revision: revision
                    .map(|v| {
                        u64::try_from(v)
                            .map_err(|_| Error::InvalidInput("Negative revision".into()))
                    })
                    .transpose()?,
                experience_id: e.map(|v| v.parse()).transpose()?,
                description,
            })
        })
        .collect()
    }
    pub fn database_health(&self) -> Result<serde_json::Value> {
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        let violations = self
            .connection
            .prepare("PRAGMA foreign_key_check")?
            .exists([])?;
        let snapshot_count: i64 =
            self.connection
                .query_row("SELECT count(*) FROM profile_snapshots", [], |r| r.get(0))?;
        Ok(
            serde_json::json!({"integrity":integrity,"foreign_key_violations":violations,"snapshot_count":snapshot_count}),
        )
    }
}
