// SPDX-License-Identifier: Apache-2.0
//! Reconstructable evidence projections; raw Experience records remain canonical.
use super::*;
use crate::{
    Error, Result,
    application::{ApplicationVerification, LessonInfluence},
    core::*,
    curriculum::{CurriculumQuery, SkillMaturity},
    experience::{ExperienceContext, Outcome},
    lesson::{EvidenceRef, EvidenceRelationship, Lesson, LessonStatus},
    retrieval::{DeterministicRetriever, LessonRetriever, QueryContext, RetrievalOptions},
    store::{CurriculumStore, RuntimeStore, Store},
};
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, HashMap, HashSet};

pub trait ExperienceProfileBuilder {
    fn build(
        &self,
        subject: &ExperienceSubject,
        window: ProfileWindow,
    ) -> Result<ExperienceProfile>;
}
pub struct EvidenceProfileBuilder<'a> {
    pub store: &'a Store,
    pub config: &'a DevelopmentConfig,
    pub now: DateTime<Utc>,
    pub context: Option<QueryContext>,
}
impl ExperienceProfileBuilder for EvidenceProfileBuilder<'_> {
    fn build(
        &self,
        subject: &ExperienceSubject,
        window: ProfileWindow,
    ) -> Result<ExperienceProfile> {
        self.config.validate()?;
        self.store
            .read_projection(|_| self.build_inner(subject, window))
    }
}
pub fn matches_subject(
    subject: &ExperienceSubject,
    e: &DevelopmentObservation,
    families: &[crate::curriculum::TaskFamily],
) -> bool {
    match subject {
        ExperienceSubject::Agent(a) => {
            a.agent_kind == e.agent.kind
                && a.agent_version
                    .as_ref()
                    .is_none_or(|v| Some(v) == e.agent.version.as_ref())
                && a.model
                    .as_ref()
                    .is_none_or(|v| Some(v) == e.agent.model.as_ref())
                && a.configuration
                    .as_ref()
                    .is_none_or(|f| *f == e.context.environment.fingerprint)
                && match &a.profile_scope {
                    ProfileScope::LocalStore => true,
                    ProfileScope::Repository(p) => *p == e.context.repository.path,
                }
        }
        ExperienceSubject::Repository(p) => *p == e.context.repository.path,
        ExperienceSubject::TaskFamily(id) => families
            .iter()
            .find(|f| f.id == *id)
            .is_some_and(|f| f.selector.matches(&e.context)),
        ExperienceSubject::SharedLocal => true,
        _ => false,
    }
}
pub trait ExperienceActivationPolicy {
    fn activate(
        &self,
        profile: &ExperienceProfile,
        context: &QueryContext,
    ) -> Result<ActiveExperienceSet>;
}
pub struct BoundedActivation<'a> {
    pub store: &'a Store,
    pub config: &'a DevelopmentConfig,
}
impl ExperienceActivationPolicy for BoundedActivation<'_> {
    fn activate(&self, p: &ExperienceProfile, c: &QueryContext) -> Result<ActiveExperienceSet> {
        self.config.validate()?;
        let lessons = DeterministicRetriever {
            store: self.store,
            options: RetrievalOptions::default(),
        }
        .retrieve(c)?
        .matches
        .into_iter()
        .filter(|r| {
            p.lessons
                .iter()
                .find(|l| l.item.id == r.lesson.id.to_string())
                .is_none_or(|l| matches!(l.state, EvidenceState::Fresh | EvidenceState::Aging))
        })
        .take(self.config.max_lessons)
        .collect();
        let eligible = |a: &&ArtifactSummary| {
            a.context.matches(&c.experience_context())
                && matches!(a.state, EvidenceState::Fresh | EvidenceState::Aging)
                && matches!(a.status.as_str(), "supported" | "validated" | "active")
        };
        Ok(ActiveExperienceSet {
            lessons,
            reflexes: p
                .reflexes
                .iter()
                .filter(eligible)
                .take(self.config.max_reflexes)
                .map(|a| a.item.clone())
                .collect(),
            recoveries: p
                .recoveries
                .iter()
                .filter(eligible)
                .take(self.config.max_recoveries)
                .map(|a| a.item.clone())
                .collect(),
        })
    }
}
pub fn context_bundle(
    store: &Store,
    context: &ExperienceContext,
    agent: &AgentIdentity,
    cfg: &DevelopmentConfig,
) -> Result<ExperienceContextBundle> {
    let query = QueryContext::new(context, "", vec![]);
    let p = EvidenceProfileBuilder {
        store,
        config: cfg,
        now: Utc::now(),
        context: Some(query.clone()),
    }
    .build(
        &ExperienceSubject::Repository(context.repository.path.clone()),
        ProfileWindow::AllTime,
    )?;
    let active = BoundedActivation { store, config: cfg }.activate(&p, &query)?;
    let stale_items = p
        .lessons
        .iter()
        .chain(&p.reflexes)
        .chain(&p.recoveries)
        .filter(|a| matches!(a.state, EvidenceState::Aging | EvidenceState::Stale))
        .take(8)
        .map(|a| a.item.clone())
        .collect();
    let contradictions = p
        .lessons
        .iter()
        .chain(&p.reflexes)
        .chain(&p.recoveries)
        .filter(|a| a.state == EvidenceState::Contradicted)
        .take(8)
        .map(|a| a.item.clone())
        .collect();
    let mut recommendations = vec![];
    if p.contributing_agents.iter().any(|a| a != agent) {
        recommendations.push("Agent/model identity differs from some evidence origins; preserve provenance and consider explicit revalidation, not independent replication".into());
    }
    if p.freshness.needs_revalidation > 0 {
        recommendations.push("Review stale or conflicting items with revalidation list; no work starts automatically".into());
    }
    let mut known_unknowns: Vec<_> = p.coverage.known_unknowns.into_iter().take(12).collect();
    if p.coverage.skills.is_empty() {
        known_unknowns.push("No tested Skill coverage is recorded for this repository; competence outside observed conditions is UNKNOWN".into());
    }
    Ok(ExperienceContextBundle {
        relevant: active,
        known_unknowns,
        stale_items,
        contradictions,
        recommendations,
        auto_run: false,
    })
}
pub fn maintain(
    store: &Store,
    p: &ExperienceProfile,
    context: &ExperienceContext,
    persist: bool,
) -> Result<MaintenanceReport> {
    let mut proposed = vec![];
    let ids: HashSet<_> = p.evidence_ids.iter().collect();
    let rejected: HashSet<_> = store
        .development_observations()?
        .into_iter()
        .filter(|e| ids.contains(&e.id))
        .flat_map(|e| {
            e.applications
                .into_iter()
                .filter(|a| a.influence == LessonInfluence::Rejected)
                .map(|a| a.lesson_id.to_string())
        })
        .collect();
    for a in p.lessons.iter().chain(&p.reflexes).chain(&p.recoveries) {
        if !a.context.matches(context) {
            continue;
        }
        let reason = match a.state {
            EvidenceState::Contradicted => Some(RevalidationReason::Contradicted),
            EvidenceState::Stale => Some(RevalidationReason::Stale),
            EvidenceState::Aging => Some(RevalidationReason::EnvironmentChanged),
            EvidenceState::Fresh if a.status == "candidate" => {
                Some(RevalidationReason::LowConfidence)
            }
            _ if rejected.contains(&a.item.id) => Some(RevalidationReason::AgentRejected),
            _ => None,
        };
        if let Some(reason) = reason {
            let item = RevalidationItem {
                id: RevalidationId::new(),
                item: a.item.clone(),
                reason,
                explanation: format!(
                    "{} {}: {}. Explicit invocation required",
                    a.item.kind,
                    a.item.id,
                    a.reasons.join("; ")
                ),
                context: context.clone(),
                created_at: Utc::now(),
                status: "pending".into(),
                experiment_id: None,
            };
            if persist {
                store.enqueue_revalidation(&item)?;
            }
            proposed.push(item);
        }
    }
    let mut groups: BTreeMap<String, Vec<LessonId>> = BTreeMap::new();
    for l in store.all_lessons()? {
        if p.lessons.iter().any(|a| a.item.id == l.id.to_string()) {
            let normalized = l
                .claim
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            groups
                .entry(stable_id(
                    "",
                    &(&l.context_match, &l.avoid, &l.prefer, normalized),
                )?)
                .or_default()
                .push(l.id);
        }
    }
    let revalidation = if persist {
        let ids: HashSet<_> = proposed.iter().map(|i| i.item.id.clone()).collect();
        store
            .revalidations()?
            .into_iter()
            .filter(|i| {
                ids.contains(&i.item.id)
                    && i.status == "pending"
                    && i.context.repository == context.repository
                    && i.context.environment.fingerprint == context.environment.fingerprint
            })
            .collect()
    } else {
        proposed
    };
    Ok(MaintenanceReport {
        health: p.freshness.clone(),
        revalidation,
        possible_duplicates: groups.into_values().filter(|g| g.len() > 1).collect(),
        auto_run: false,
    })
}
pub async fn run_revalidation(
    store: &Store,
    item: &RevalidationItem,
    cancel: &crate::cancellation::Cancellation,
) -> Result<RevalidationItem> {
    if item.status != "pending" {
        return Ok(item.clone());
    }
    if item.item.kind != "lesson" {
        return Err(Error::InvalidInput("Use skill harden for response/Skill revalidation; direct queue execution currently supports Lessons".into()));
    }
    let id: LessonId = item.item.id.parse()?;
    let lesson = store.lesson(&id)?;
    if lesson.version as u64 != item.item.revision {
        return Err(Error::Intervention(
            "Lesson revision changed; refresh maintenance before revalidating".into(),
        ));
    }
    let state = crate::dojo::capture_state(&item.context.repository.path)?;
    if state.git_commit != item.context.repository.commit {
        return Err(Error::Intervention(
            "Repository changed after revalidation was queued".into(),
        ));
    }
    if crate::experience::EnvironmentContext::capture(
        &state.repo_path,
        item.context.environment.mode,
    )?
    .fingerprint
        != item.context.environment.fingerprint
    {
        return Err(Error::Intervention(
            "Runtime context changed after revalidation was queued; refresh maintenance".into(),
        ));
    }
    let source = store.experience(&lesson.source_experience)?;
    if source.evaluation.spec.checks.is_empty() {
        return Err(Error::InvalidInput(
            "Revalidation requires an explicit evaluator".into(),
        ));
    }
    let experiment = crate::experiment::ExperimentEngine { store }
        .execute_at(
            &id,
            Some((
                state,
                source.evaluation.spec,
                "Explicit queued revalidation".into(),
            )),
            cancel,
        )
        .await?;
    let mut done = item.clone();
    done.experiment_id = Some(experiment.id);
    done.status = if experiment.status == crate::experiment::ExperimentStatus::Completed {
        "recorded"
    } else {
        "incomplete"
    }
    .into();
    store.finish_revalidation(&done)?;
    Ok(done)
}
pub fn start_episode(
    store: &Store,
    subject: ExperienceSubject,
    name: &str,
    cfg: &DevelopmentConfig,
) -> Result<DevelopmentEpisode> {
    if name.trim().is_empty() || name.len() > 128 {
        return Err(Error::InvalidInput("Episode needs a bounded name".into()));
    }
    let now = Utc::now();
    let p = EvidenceProfileBuilder {
        store,
        config: cfg,
        now,
        context: None,
    }
    .build(&subject, ProfileWindow::AllTime)?;
    let before = store.save_profile_snapshot(&p)?;
    let e = DevelopmentEpisode {
        id: DevelopmentEpisodeId::new(),
        name: name.into(),
        subject,
        started_at: now,
        ended_at: None,
        task_family: None,
        experiences: vec![],
        learning_artifacts: Default::default(),
        profile_before: Some(before.id),
        profile_after: None,
    };
    store.save_episode(&e, true)?;
    Ok(e)
}
pub fn finish_episode(
    store: &Store,
    id: &DevelopmentEpisodeId,
    cfg: &DevelopmentConfig,
) -> Result<DevelopmentEpisode> {
    let mut e = store
        .episodes()?
        .into_iter()
        .find(|e| e.id == *id)
        .ok_or_else(|| Error::NotFound("Episode not found".into()))?;
    if e.ended_at.is_some() {
        return Ok(e);
    }
    let now = Utc::now();
    let p = EvidenceProfileBuilder {
        store,
        config: cfg,
        now,
        context: None,
    }
    .build(&e.subject, ProfileWindow::Since(e.started_at))?;
    e.experiences = p.evidence_ids.clone();
    e.profile_after = Some(store.save_profile_snapshot(&p)?.id);
    e.ended_at = Some(now);
    let ids: HashSet<_> = e.experiences.iter().collect();
    for c in CurriculumStore::list(store, CurriculumQuery::default())? {
        for t in c.trials {
            if let Some(l) = t.learning_outcome
                && l.new_experiences.iter().any(|i| ids.contains(i))
            {
                e.learning_artifacts
                    .lessons_created
                    .extend(l.lessons_created);
                e.learning_artifacts
                    .reflexes_created
                    .extend(l.reflexes_created);
                e.learning_artifacts
                    .recoveries_created
                    .extend(l.recoveries_created);
                e.learning_artifacts
                    .envelope_updates
                    .extend(l.envelope_updates);
            }
        }
    }
    e.learning_artifacts.new_experiences = e.experiences.clone();
    for l in store.all_lessons()? {
        if l.created_at >= e.started_at && l.created_at <= now && ids.contains(&l.source_experience)
        {
            e.learning_artifacts.lessons_created.push(l.id);
        }
    }
    e.learning_artifacts
        .lessons_created
        .sort_by_key(ToString::to_string);
    e.learning_artifacts.lessons_created.dedup();
    store.save_episode(&e, false)?;
    Ok(e)
}
pub fn lesson_basis(
    lesson: &Lesson,
    observations: &HashMap<ExperienceId, DevelopmentObservation>,
    support: &[ExperienceId],
) -> Option<FreshnessBasis> {
    let source = observations.get(&lesson.source_experience)?;
    let latest = support
        .iter()
        .filter_map(|id| observations.get(id))
        .filter(|e| e.outcome == Outcome::Success)
        .max_by_key(|e| e.created_at)
        .unwrap_or(source);
    Some(FreshnessBasis {
        origin_context: Some(source.context.clone()),
        last_supported_at: latest.created_at,
        context: latest.context.clone(),
        agent: latest.agent.clone(),
        contradicted: lesson.status == LessonStatus::Contradicted
            || lesson.evidence.iter().any(|e| {
                matches!(
                    e,
                    EvidenceRef::Experience {
                        relationship: EvidenceRelationship::Contradicts,
                        ..
                    } | EvidenceRef::Trial {
                        relationship: EvidenceRelationship::Contradicts,
                        ..
                    }
                )
            }),
    })
}
fn summarize_health(items: impl Iterator<Item = EvidenceState>) -> ExperienceHealth {
    let mut h = ExperienceHealth::default();
    for state in items {
        match state {
            EvidenceState::Fresh => h.fresh += 1,
            EvidenceState::Aging => h.aging += 1,
            EvidenceState::Stale => h.stale += 1,
            EvidenceState::Superseded => h.superseded += 1,
            EvidenceState::Contradicted => h.contradicted += 1,
            EvidenceState::Retired => h.retired += 1,
        };
        if matches!(
            state,
            EvidenceState::Stale | EvidenceState::Contradicted | EvidenceState::Aging
        ) {
            h.needs_revalidation += 1;
        }
    }
    h
}
impl EvidenceProfileBuilder<'_> {
    fn build_inner(
        &self,
        subject: &ExperienceSubject,
        window: ProfileWindow,
    ) -> Result<ExperienceProfile> {
        if matches!(
            subject,
            ExperienceSubject::Workspace(_) | ExperienceSubject::OrganizationScope(_)
        ) {
            return Err(Error::InvalidInput("Workspace/organization profiles are design-only; use local Agent, Repository, TaskFamily or SharedLocal".into()));
        }
        if matches!(
            window,
            ProfileWindow::LastDays(0) | ProfileWindow::LastExperiences(0)
        ) {
            return Err(Error::InvalidInput(
                "Profile window must be positive".into(),
            ));
        }
        let families = self.store.task_families()?;
        if let ExperienceSubject::TaskFamily(id) = subject {
            self.store.task_family(&id.to_string())?;
        }
        let observations: Vec<_> = self
            .store
            .development_observations()?
            .into_iter()
            .filter(|o| o.created_at <= self.now)
            .collect();
        let all: HashMap<_, _> = observations
            .iter()
            .map(|o| (o.id.clone(), o.clone()))
            .collect();
        let subject_all: Vec<_> = observations
            .iter()
            .filter(|o| matches_subject(subject, o, &families))
            .collect();
        let owned: HashSet<_> = subject_all.iter().map(|o| o.id.clone()).collect();
        let start = match &window {
            ProfileWindow::Since(t) => Some(*t),
            ProfileWindow::LastDays(d) => Some(self.now - Duration::days(*d as i64)),
            _ => None,
        };
        let mut selected: Vec<_> = subject_all
            .iter()
            .copied()
            .filter(|o| start.is_none_or(|t| o.created_at >= t))
            .collect();
        if let ProfileWindow::LastExperiences(n) = &window {
            let remove = selected
                .len()
                .saturating_sub((*n).min(usize::MAX as u64) as usize);
            selected.drain(..remove);
        }
        let selected_ids: HashSet<_> = selected.iter().map(|e| e.id.clone()).collect();
        let metric_start = if matches!(window, ProfileWindow::LastExperiences(_)) {
            selected.first().map(|e| e.created_at)
        } else {
            start
        };
        let in_period = |at: DateTime<Utc>| at <= self.now && metric_start.is_none_or(|s| at >= s);
        let included_lessons: HashSet<_> = subject_all
            .iter()
            .flat_map(|e| e.applications.iter().map(|a| a.lesson_id.clone()))
            .collect();
        let mut lessons = self.store.all_lessons()?;
        // Historical diagnostic clocks must not read a future Lesson revision.
        for l in &mut lessons {
            if l.updated_at > self.now
                && let Some(old) = self
                    .store
                    .lesson_versions(&l.id)?
                    .into_iter()
                    .rfind(|v| v.updated_at <= self.now)
            {
                *l = old;
            }
        }
        lessons.retain(|l| l.created_at <= self.now);
        let lesson_map: HashMap<_, _> = lessons.iter().map(|l| (l.id.clone(), l)).collect();
        let query_for = |basis: &FreshnessBasis| -> QueryContext {
            self.context.clone().unwrap_or_else(|| {
                let latest = subject_all
                    .iter()
                    .rev()
                    .find(|e| e.context.repository.path == basis.context.repository.path)
                    .copied();
                QueryContext::new(
                    latest.map(|e| &e.context).unwrap_or(&basis.context),
                    "",
                    vec![],
                )
            })
        };
        let current_agent = subject_all.last().map(|e| &e.agent);
        let mut lesson_summaries = vec![];
        let mut efficiency = vec![];
        for l in lessons
            .iter()
            .filter(|l| owned.contains(&l.source_experience) || included_lessons.contains(&l.id))
        {
            let support = self.store.lesson_support_experiences(&l.id)?;
            let Some(basis) = lesson_basis(l, &all, &support) else {
                continue;
            };
            let assessment = assess_freshness(
                &basis,
                &query_for(&basis),
                current_agent,
                self.now,
                self.config,
            );
            let item = ExperienceRef {
                kind: "lesson".into(),
                id: l.id.to_string(),
                revision: l.version as u64,
            };
            let first_validated = self
                .store
                .lesson_versions(&l.id)?
                .into_iter()
                .find(|v| v.status == LessonStatus::Validated && v.updated_at <= self.now);
            let count = first_validated
                .map(|v| -> Result<u64> {
                    let mut ids: HashSet<_> = v
                        .evidence
                        .iter()
                        .filter_map(|e| {
                            if let EvidenceRef::Experience { experience_id, .. } = e {
                                Some(experience_id.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    for id in &support {
                        if all.get(id).is_some_and(|e| e.created_at <= v.updated_at) {
                            ids.insert(id.clone());
                        }
                    }
                    for e in &v.evidence {
                        if let EvidenceRef::Trial {
                            experiment_id,
                            trial_id,
                            ..
                        } = e
                            && let Some(t) = self
                                .store
                                .experiment(experiment_id)?
                                .trials
                                .into_iter()
                                .find(|t| t.spec.id == *trial_id)
                        {
                            ids.insert(t.experience_id);
                        }
                    }
                    Ok(ids.len() as u64)
                })
                .transpose()?;
            efficiency.push(LearningEfficiency{artifact:item.clone(),experiences_to_validation:count,definition:"Unique linked Experience IDs present by the first validated revision, including controlled support arms; not all store traffic".into()});
            lesson_summaries.push(ArtifactSummary {
                scope: if l.context_match.repository.is_some() {
                    ExperienceScope::Repository
                } else {
                    ExperienceScope::Shared
                },
                item,
                status: serde_json::to_value(l.status)?
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                confidence: Some(l.confidence.into()),
                context: l.context_match.clone(),
                source_experience: l.source_experience.clone(),
                origin_agent: all[&l.source_experience].agent.clone(),
                state: if l.status == LessonStatus::Retired {
                    EvidenceState::Retired
                } else {
                    assessment.state
                },
                last_supported_at: basis.last_supported_at,
                reasons: assessment.reasons,
                trust: if l
                    .evidence
                    .iter()
                    .any(|e| matches!(e, EvidenceRef::Trial { .. }))
                {
                    EvidenceTrust::LocalExperiment
                } else {
                    EvidenceTrust::LocalObserved
                },
            });
        }
        let mut reflexes = vec![];
        let mut recoveries = vec![];
        let response_summary = |kind: &str,
                                id: String,
                                version: u32,
                                status: String,
                                confidence: f64,
                                context: crate::lesson::ContextSelector,
                                source_trial: &ChaosTrialId,
                                evidence: &[EvidenceRef]|
         -> Result<Option<ArtifactSummary>> {
            let source = self.store.chaos_trial(source_trial)?.experience_id;
            if !owned.contains(&source) {
                return Ok(None);
            }
            let Some(origin) = all.get(&source) else {
                return Ok(None);
            };
            let support = support_ids(evidence);
            let latest = support
                .iter()
                .filter_map(|id| all.get(id))
                .filter(|e| e.outcome == Outcome::Success)
                .max_by_key(|e| e.created_at)
                .unwrap_or(origin);
            let basis = FreshnessBasis {
                origin_context: Some(origin.context.clone()),
                last_supported_at: latest.created_at,
                context: latest.context.clone(),
                agent: latest.agent.clone(),
                contradicted: status == "contradicted",
            };
            let a = assess_freshness(
                &basis,
                &query_for(&basis),
                current_agent,
                self.now,
                self.config,
            );
            Ok(Some(ArtifactSummary {
                scope: if context.repository.is_some() {
                    ExperienceScope::Repository
                } else {
                    ExperienceScope::Shared
                },
                item: ExperienceRef {
                    kind: kind.into(),
                    id,
                    revision: version as u64,
                },
                status: status.clone(),
                confidence: Some(confidence),
                context,
                source_experience: source,
                origin_agent: origin.agent.clone(),
                state: if status == "retired" {
                    EvidenceState::Retired
                } else {
                    a.state
                },
                last_supported_at: basis.last_supported_at,
                reasons: a.reasons,
                trust: EvidenceTrust::LocalExperiment,
            }))
        };
        for r in self.store.reflexes()? {
            if let Some(s) = response_summary(
                "reflex",
                r.id.to_string(),
                r.version,
                serde_json::to_value(r.status)?
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                r.confidence.into(),
                r.trigger.context,
                &r.source_trial,
                &r.evidence,
            )? {
                reflexes.push(s);
            }
        }
        for r in self.store.recoveries()? {
            if let Some(s) = response_summary(
                "recovery",
                r.id.to_string(),
                r.version,
                serde_json::to_value(r.status)?
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                r.confidence.into(),
                r.context,
                &r.source_trial,
                &r.evidence,
            )? {
                recoveries.push(s);
            }
        }
        let packages = self.store.latest_packages()?;
        let policies = crate::bridge::config::Config::load(&self.store.home)?;
        let mut skills = vec![];
        let mut coverage=ExperienceCoverage{note:"Per-profile finite catalogs from the latest stored package; stale snapshots are not fresh validation. No universal coverage percentage.".into(),..Default::default()};
        for s in self
            .store
            .skills()?
            .into_iter()
            .filter(|s| owned.contains(&s.source_experience))
        {
            let revisions = self.store.skill_revisions(&s.id)?;
            let latest_revision = revisions.last();
            let revision = latest_revision.map(|r| r.revision).unwrap_or(1);
            let p = packages
                .iter()
                .filter(|p| p.skill == s.id)
                .filter(|p| {
                    latest_revision
                        .is_none_or(|r| r.revision == 1 || p.generated_at >= r.created_at)
                })
                .max_by_key(|p| p.generated_at);
            use crate::curriculum::SkillMaturityPolicy;
            let mut maturity = p
                .map(|p| {
                    crate::curriculum::ConfiguredMaturityPolicy(&policies.curriculum)
                        .evaluate(&s, &p.evidence)
                })
                .unwrap_or(s.maturity);
            if let Some(p) = p {
                let basis = FreshnessBasis {
                    origin_context: None,
                    last_supported_at: p.evidence.freshness.last_supported_at,
                    context: all[&s.source_experience].context.clone(),
                    agent: all[&s.source_experience].agent.clone(),
                    contradicted: false,
                };
                let query = query_for(&basis);
                if p.evidence.freshness.environment_version.as_deref()
                    != Some(query.repository.commit.as_str())
                    || assess_freshness(&basis, &query, None, self.now, self.config).state
                        == EvidenceState::Stale
                {
                    maturity = SkillMaturity::Supported;
                }
                coverage.skills.push((s.id.clone(), p.coverage.clone()));
                coverage
                    .known_unknowns
                    .extend(p.coverage.dimensions.iter().flat_map(|d| d.unknown.clone()));
            }
            if subject_all
                .iter()
                .rev()
                .find(|e| {
                    e.goal == all[&s.source_experience].goal
                        && !e.perturbed
                        && e.recovery.is_none()
                        && e.context.repository.path
                            == all[&s.source_experience].context.repository.path
                })
                .is_some_and(|e| e.outcome == Outcome::Failure)
            {
                maturity = SkillMaturity::Degraded;
            }
            skills.push(SkillSummary {
                skill_id: s.id,
                revision,
                name: s.name,
                maturity,
                source_experience: s.source_experience,
                coverage: p.map(|p| p.coverage.clone()).unwrap_or_default(),
            });
        }
        coverage.known_unknowns.sort();
        coverage.known_unknowns.dedup();
        let tasks: Vec<_> = selected.iter().copied().filter(|e| e.task).collect();
        let conclusive: Vec<_> = tasks
            .iter()
            .copied()
            .filter(|e| {
                matches!(
                    e.outcome,
                    Outcome::Success | Outcome::Failure | Outcome::TimedOut
                )
            })
            .collect();
        let audits: Vec<_> = tasks.iter().copied().filter(|e| e.audited).collect();
        let recovery_attempts: Vec<_> = tasks
            .iter()
            .filter_map(|e| e.recovery.as_ref())
            .filter(|r| r.attempted && r.reproduced_failure)
            .collect();
        let mut latencies: Vec<_> = recovery_attempts
            .iter()
            .filter(|r| r.succeeded)
            .map(|r| r.time_to_recovery_ms)
            .collect();
        latencies.sort();
        let observed: Vec<_> = tasks
            .iter()
            .flat_map(|e| {
                e.applications
                    .iter()
                    .filter(|a| a.delivered && a.verification == ApplicationVerification::Observed)
                    .map(move |a| (*e, a))
            })
            .collect();
        let applied: Vec<_> = observed
            .iter()
            .filter(|(_, a)| a.influence == LessonInfluence::Applied)
            .collect();
        let transfers: Vec<_> = observed
            .iter()
            .filter(|(e, a)| {
                lesson_map
                    .get(&a.lesson_id)
                    .and_then(|l| all.get(&l.source_experience))
                    .is_some_and(|o| o.tree_hash != e.tree_hash)
            })
            .collect();
        let mut portable = vec![];
        for (e, a) in &observed {
            let l = self.store.lesson_version(&a.lesson_id, a.lesson_version)?;
            if l.status == LessonStatus::Validated
                && all
                    .get(&l.source_experience)
                    .is_some_and(|o| o.agent != e.agent)
            {
                portable.push((*e, *a));
            }
        }
        let mut known_failures = HashSet::new();
        let mut repeated_failure = 0;
        let mut known_encounters = 0;
        for e in &subject_all {
            if !e.task {
                continue;
            }
            let signature = e
                .recovery
                .as_ref()
                .and_then(|r| r.failure_signature.as_ref());
            let signatures: Vec<_> = e
                .failure_signatures
                .iter()
                .chain(signature)
                .filter(|s| s.as_str() != "required_check_failed")
                .collect();
            let known = signatures.iter().any(|s| {
                known_failures.contains(&(e.context.repository.path.clone(), (*s).clone()))
            });
            if selected_ids.contains(&e.id) && known {
                known_encounters += 1;
                if e.outcome == Outcome::Failure
                    && !e
                        .recovery
                        .as_ref()
                        .is_some_and(|r| r.attempted && r.succeeded)
                {
                    repeated_failure += 1;
                }
            }
            for s in signatures {
                known_failures.insert((e.context.repository.path.clone(), s.clone()));
            }
        }
        let tests = self.store.resilience_tests()?;
        let fired: Vec<_> = tests
            .iter()
            .filter(|t| {
                (t.with.as_ref().is_some_and(|id| selected_ids.contains(id))
                    || (in_period(t.created_at)
                        && self
                            .store
                            .chaos_trial(&t.source_trial)
                            .is_ok_and(|c| owned.contains(&c.experience_id))))
                    && t.false_positive.is_some()
            })
            .collect();
        let experiments: Vec<_> = self
            .store
            .experiments()?
            .into_iter()
            .filter(|e| {
                e.trials
                    .iter()
                    .any(|t| selected_ids.contains(&t.experience_id))
                    || (owned.contains(&e.source_experience) && in_period(e.created_at))
            })
            .collect();
        let curricula = CurriculumStore::list(self.store, CurriculumQuery::default())?;
        let curriculum_trials: Vec<_> = curricula
            .iter()
            .flat_map(|c| &c.trials)
            .filter(|t| {
                t.result
                    .as_ref()
                    .is_some_and(|r| r.experiences.iter().any(|id| selected_ids.contains(id)))
            })
            .collect();
        let artifacts = curriculum_trials
            .iter()
            .filter_map(|t| t.learning_outcome.as_ref())
            .map(|l| {
                l.lessons_created.len() + l.reflexes_created.len() + l.recoveries_created.len()
            })
            .sum::<usize>();
        let ratio = |n: usize, d: usize, definition: &str| {
            MetricValue::ratio(n as u64, d as u64, &window, definition)
        };
        let metrics = DevelopmentMetrics {
            task_success_rate: ratio(
                conclusive
                    .iter()
                    .filter(|e| e.outcome == Outcome::Success)
                    .count(),
                conclusive.len(),
                "Successful evaluated task attempts / conclusive task attempts; internal experiment/chaos/response arms excluded; retries count as attempts",
            ),
            repeated_mistake_rate: ratio(
                audits.iter().filter(|e| e.repeated_mistake).count(),
                audits.len(),
                "Task attempts with a recorded matched-Lesson repeated action / attempts with observed actions; unobservable usage is UNKNOWN",
            ),
            repeated_failure_rate: ratio(
                repeated_failure,
                known_encounters,
                "Unresolved repeated failure / task encounters of a previously observed concrete signature in the same repository; generic evaluator failures excluded",
            ),
            recovery_success_rate: ratio(
                recovery_attempts.iter().filter(|r| r.succeeded).count(),
                recovery_attempts.len(),
                "Successful typed recovery attempts / reproduced, attempted task recoveries",
            ),
            median_time_to_recovery_ms: benchmark::median(&latencies),
            recovery_latency_samples: latencies.len() as u64,
            experience_transfer_rate: ratio(
                transfers
                    .iter()
                    .filter(|(e, a)| {
                        a.influence == LessonInfluence::Applied && e.outcome == Outcome::Success
                    })
                    .count(),
                transfers.len(),
                "Beneficial observed applications / observed delivered items in a different source tree; not an independence claim",
            ),
            lesson_precision: ratio(
                applied
                    .iter()
                    .filter(|(e, _)| e.outcome == Outcome::Success)
                    .count(),
                applied.len(),
                "Successful task outcomes with observed applied Lessons / observed applications; association, not causal precision",
            ),
            reflex_false_positive_rate: ratio(
                fired
                    .iter()
                    .filter(|t| t.false_positive == Some(true))
                    .count(),
                fired.len(),
                "Proven false positives / paired tests where a Reflex fired; normal runtime firings alone cannot label false positives",
            ),
            experiment_success_rate: ratio(
                experiments
                    .iter()
                    .filter(|e| {
                        e.conclusion != crate::experiment::ExperimentConclusion::Inconclusive
                    })
                    .count(),
                experiments.len(),
                "Conclusive paired Lesson experiments / observed paired Lesson experiments; strategy comparisons are not pooled",
            ),
            curriculum_yield: ratio(
                artifacts,
                curriculum_trials.len(),
                "New Lesson/Reflex/Recovery artifacts / executed curriculum trials; can exceed one and does not measure usefulness",
            ),
            hardened_skill_count: skills
                .iter()
                .filter(|s| s.maturity == SkillMaturity::Hardened)
                .count() as u64,
            experience_portability_rate: ratio(
                portable
                    .iter()
                    .filter(|(e, a)| {
                        a.influence == LessonInfluence::Applied && e.outcome == Outcome::Success
                    })
                    .count(),
                portable.len(),
                "Beneficial observed applications / delivered validated Lesson applications to a changed agent/version/model; not independent replication",
            ),
        };
        let capabilities = families
            .iter()
            .filter_map(|f| {
                let matching: Vec<_> = conclusive
                    .iter()
                    .filter(|e| f.selector.matches(&e.context))
                    .collect();
                (!matching.is_empty()).then(|| CapabilityProfile {
                    task_family: f.id.clone(),
                    matching_tasks: matching.len() as u64,
                    task_success: ratio(
                        matching
                            .iter()
                            .filter(|e| e.outcome == Outcome::Success)
                            .count(),
                        matching.len(),
                        "Evaluated task attempts matching the explicit family selector",
                    ),
                })
            })
            .collect();
        let freshness = summarize_health(
            lesson_summaries
                .iter()
                .chain(&reflexes)
                .chain(&recoveries)
                .map(|s| s.state),
        );
        let mut agents = BTreeMap::new();
        for o in &subject_all {
            agents.insert(serde_json::to_string(&o.agent)?, o.agent.clone());
        }
        let mut evidence_ids: Vec<_> = selected.iter().map(|e| e.id.clone()).collect();
        evidence_ids.extend(
            experiments
                .iter()
                .flat_map(|e| e.trials.iter().map(|t| t.experience_id.clone())),
        );
        evidence_ids.extend(
            fired
                .iter()
                .flat_map(|t| t.with.iter().chain(&t.without).cloned()),
        );
        evidence_ids.sort_by_key(ToString::to_string);
        evidence_ids.dedup();
        Ok(ExperienceProfile {
            id: stable_id("profile-", subject)?.parse()?,
            subject: subject.clone(),
            window,
            created_at: subject_all
                .first()
                .map(|e| e.created_at)
                .unwrap_or(self.now),
            updated_at: self.now,
            experience_count: selected.len() as u64,
            task_count: tasks.len() as u64,
            skills,
            lessons: lesson_summaries,
            reflexes,
            recoveries,
            capabilities,
            metrics,
            coverage,
            freshness,
            efficiency,
            evidence_ids,
            policy_versions: self.config.versions(),
            policy_hash: blake3::hash(&serde_json::to_vec(&(
                self.config.hash()?,
                &policies.curriculum,
            ))?)
            .to_hex()
            .to_string(),
            contributing_agents: agents.into_values().collect(),
            runtime_control: self.store.runtime_development_metrics()?,
        })
    }
}
