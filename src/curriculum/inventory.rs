// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    core::*,
    dojo::capture_state,
    experience::{Experience, ExperienceContext, Outcome},
    lesson::EvidenceRef,
    resilience::*,
    store::{CurriculumStore, ExperienceQuery, ExperienceStore, LessonQuery, LessonStore, Store},
};
use std::collections::{BTreeMap, HashSet};

pub fn fingerprint(
    skill: &Skill,
    state: &StateRef,
    agent: &AgentIdentity,
    evaluator: &crate::evaluation::EvaluationSpec,
    condition: &CatalogCondition,
) -> Result<String> {
    let environment = crate::experience::EnvironmentContext::capture(
        &state.repo_path,
        EnvironmentMode::Controlled,
    )?;
    Ok(blake3::hash(&serde_json::to_vec(&(
        "curriculum-signature-v1",
        &skill.id,
        &skill.procedure,
        state,
        agent,
        evaluator,
        &condition.parameters,
        &condition.name,
        &environment.fingerprint,
        crate::resilience::fixture::RUNTIME_VERSION,
    ))?)
    .to_hex()
    .to_string())
}
pub fn skill_state(store: &Store, skill: &Skill) -> Result<StateRef> {
    capture_state(
        &store
            .experience(&skill.source_experience)?
            .starting_state
            .repo_path,
    )
}
pub fn fixture_kind(source: &Experience) -> Option<FixtureKind> {
    [
        FixtureKind::SkillHardening,
        FixtureKind::SkillHardeningTransfer,
        FixtureKind::RetryResilience,
        FixtureKind::StaleCredential,
        FixtureKind::ConfigDrift,
    ]
    .into_iter()
    .find(|k| {
        source
            .context
            .tags
            .contains(&format!("fixture-kind:{}", k.name()))
    })
}
pub fn inventory(
    store: &Store,
    target: &CurriculumTarget,
    profile: &str,
    config: &CurriculumConfig,
) -> Result<CurriculumContext> {
    let skills=match target {
        CurriculumTarget::Skill(id)=>vec![store.skill(&id.to_string())?],
        CurriculumTarget::TaskFamily(id)=>{
            let f=store.task_family(&id.to_string())?;
            store.skills()?.into_iter().filter(|s|store.experience(&s.source_experience).is_ok_and(|e|f.selector.matches(&e.context))).collect()
        },
        _=>return Err(Error::InvalidInput("V0.5 plans Skills and manually defined TaskFamilies; agent-wide and Lesson targets are design-only".into())),
    };
    if skills.is_empty() || skills.len() > 32 {
        return Err(Error::InvalidInput(
            "Target requires 1..32 registered Skills".into(),
        ));
    }
    let experiences = ExperienceStore::list(store, ExperienceQuery::default())?
        .into_iter()
        .map(|e| store.experience(&e.id))
        .collect::<Result<Vec<_>>>()?;
    let lessons = LessonStore::list(store, LessonQuery::default())?
        .into_iter()
        .map(|l| store.lesson(&l.id))
        .collect::<Result<Vec<_>>>()?;
    let mut ctx = CurriculumContext {
        experiments: store.experiments()?,
        skills,
        experiences,
        lessons,
        envelopes: store.envelopes()?,
        reflexes: store.reflexes()?,
        recoveries: store.recoveries()?,
        tests: store.resilience_tests()?,
        history: CurriculumStore::list(store, CurriculumQuery::default())?,
        packages: vec![],
        profile: PerturbationCatalog::configured(config)?.profile(profile)?,
        capabilities: RealityCapabilities::default(),
        now: chrono::Utc::now(),
        config: config.clone(),
    };
    for s in &ctx.skills {
        ctx.packages.push(package_from_context(store, s, &ctx)?);
    }
    Ok(ctx)
}
pub fn skill_package(
    store: &Store,
    name: &str,
    profile: &str,
    config: &CurriculumConfig,
) -> Result<ExperiencePackage> {
    let skill = store.skill(name)?;
    let ctx = inventory(store, &CurriculumTarget::Skill(skill.id), profile, config)?;
    ctx.packages
        .into_iter()
        .next()
        .ok_or_else(|| Error::InvalidInput("Missing Skill package".into()))
}
fn relevant<'a>(
    skill: &Skill,
    source: &Experience,
    experiences: &'a [Experience],
) -> Vec<&'a Experience> {
    experiences
        .iter()
        .filter(|e| {
            skill.context.matches(&e.context)
                && e.goal == source.goal
                && e.replay
                    .as_ref()
                    .is_some_and(|r| skill.procedure.iter().any(|a| a.matches_shell(&r.script)))
        })
        .collect()
}
fn package_from_context(
    store: &Store,
    s: &Skill,
    c: &CurriculumContext,
) -> Result<ExperiencePackage> {
    let source = store.experience(&s.source_experience)?;
    let state = skill_state(store, s)?;
    let experiences = relevant(s, &source, &c.experiences);
    let ids: HashSet<_> = experiences.iter().map(|e| &e.id).collect();
    let current =
        ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled)?;
    let env = &current.environment.fingerprint;
    let base: Vec<_> = experiences
        .iter()
        .copied()
        .filter(|e| {
            e.outcome == Outcome::Success
                && e.starting_state == state
                && &e.context.environment.fingerprint == env
                && e.agent == source.agent
                && e.evaluation.spec == source.evaluation.spec
                && c.now.signed_duration_since(e.created_at)
                    <= chrono::Duration::days(c.config.stale_after_days as i64)
                && e.resilience.as_ref().is_none_or(|r| {
                    r.perturbation_ids.is_empty()
                        && r.reflex_matches.is_empty()
                        && r.recovery_attempt.is_none()
                })
                && e.experiment.is_none()
        })
        .collect();
    // Ordinary experiment revalidations are explicitly linked to this Skill.
    let mut base_ids: HashSet<ExperienceId> = base.iter().map(|e| e.id.clone()).collect();
    for t in c
        .history
        .iter()
        .flat_map(|h| &h.trials)
        .filter(|t| t.skill_id == s.id && t.condition == "control")
    {
        if let Some(r) = &t.result {
            for id in &r.experiences {
                if let Some(e) = c.experiences.iter().find(|e| e.id == *id)
                    && e.starting_state == state
                    && e.outcome == Outcome::Success
                    && e.evaluation.spec == source.evaluation.spec
                    && &e.context.environment.fingerprint == env
                    && c.now.signed_duration_since(e.created_at)
                        <= chrono::Duration::days(c.config.stale_after_days as i64)
                {
                    base_ids.insert(id.clone());
                }
            }
        }
    }
    let latest = base_ids
        .iter()
        .filter_map(|id| c.experiences.iter().find(|e| e.id == *id))
        .max_by_key(|e| e.created_at)
        .unwrap_or(&source);
    let freshness = ConservativeFreshnessPolicy {
        now: c.now,
        age_days: c.config.stale_after_days,
    }
    .evaluate(
        &EvidenceSummary {
            last_supported_at: latest.created_at,
            environment: latest.context.clone(),
            agent: latest.agent.clone(),
        },
        &crate::retrieval::QueryContext::new(&current, &source.goal, vec![]),
    );
    let mut observations: BTreeMap<String, Vec<ConditionObservation>> = BTreeMap::new();
    for condition in &c.profile.conditions {
        let fp = fingerprint(s, &state, &source.agent, &source.evaluation.spec, condition)?;
        let mut found = vec![];
        if condition.name == "control" {
            for id in &base_ids {
                if let Some(e) = c.experiences.iter().find(|e| e.id == *id) {
                    found.push(ConditionObservation {
                        condition: condition.name.clone(),
                        outcome: ChaosTrialOutcome::Pass,
                        experience_id: id.clone(),
                        trial_id: None,
                        observed_at: e.created_at,
                        fingerprint: fp.clone(),
                        severity: condition.severity,
                    });
                }
            }
        }
        for t in c.history.iter().flat_map(|h| &h.trials).filter(|t| {
            t.skill_id == s.id
                && t.condition == condition.name
                && t.fingerprint == fp
                && t.status == GoalStatus::Completed
        }) {
            if let Some(r) = &t.result
                && let Some(outcome) = r.outcome
                && outcome != ChaosTrialOutcome::Inconclusive
                && let Some(id) = r.experiences.last()
            {
                let e = store.experience(id)?;
                if c.now.signed_duration_since(e.created_at)
                    <= chrono::Duration::days(c.config.stale_after_days as i64)
                {
                    found.push(ConditionObservation {
                        condition: condition.name.clone(),
                        outcome,
                        experience_id: id.clone(),
                        trial_id: Some(t.id.clone()),
                        observed_at: e.created_at,
                        fingerprint: fp.clone(),
                        severity: condition.severity,
                    });
                }
            }
        }
        // Import point observations from pre-curriculum campaigns, retaining their evidence IDs.
        for envelope in &c.envelopes {
            if !matches!(&envelope.target,ChaosTarget::Skill(id) if id==&s.id) {
                continue;
            }
            for o in &envelope.tested_conditions {
                if o.perturbations.len() != 1
                    || condition.parameters.as_ref()
                        != o.perturbations.first().map(|p| &p.parameters)
                    || o.outcome == ChaosTrialOutcome::Inconclusive
                {
                    continue;
                }
                let e = store.experience(&o.experience_id)?;
                if e.starting_state == state
                    && e.agent == source.agent
                    && e.evaluation.spec == source.evaluation.spec
                    && &e.context.environment.fingerprint == env
                    && c.now.signed_duration_since(e.created_at)
                        <= chrono::Duration::days(c.config.stale_after_days as i64)
                    && !found.iter().any(|f| f.experience_id == e.id)
                {
                    found.push(ConditionObservation {
                        condition: condition.name.clone(),
                        outcome: o.outcome,
                        experience_id: e.id,
                        trial_id: None,
                        observed_at: e.created_at,
                        fingerprint: fp.clone(),
                        severity: condition.severity,
                    });
                }
            }
        }
        found.sort_by_key(|o| o.observed_at);
        observations.insert(condition.name.clone(), found);
    }
    let mut dimensions = BTreeMap::<String, CoverageDimension>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut tested = 0;
    for condition in &c.profile.conditions {
        *counts.entry(condition.dimension.clone()).or_default() += 1;
        let d = dimensions
            .entry(condition.dimension.clone())
            .or_insert_with(|| CoverageDimension {
                name: condition.dimension.clone(),
                tested: vec![],
                unknown: vec![],
                coverage_score: 0.0,
            });
        let found = observations.remove(&condition.name).unwrap_or_default();
        if found.is_empty() {
            d.unknown.push(condition.name.clone());
        } else {
            tested += 1;
            d.tested.extend(found);
        }
    }
    for (name, d) in &mut dimensions {
        d.coverage_score = (counts[name] - d.unknown.len()) as f64 / counts[name] as f64;
    }
    let coverage = SkillCoverage {
        profile: Some(c.profile.name.clone()),
        dimensions: dimensions.into_values().collect(),
        tested_conditions: tested,
        configured_conditions: c.profile.conditions.len(),
        profile_coverage: Some(tested as f64 / c.profile.conditions.len() as f64),
    };
    let related_trials: HashSet<_> = c
        .envelopes
        .iter()
        .filter(|e| matches!(&e.target,ChaosTarget::Skill(id) if id==&s.id))
        .flat_map(|e| e.tested_conditions.iter().map(|o| &o.trial_id))
        .collect();
    let reflexes: Vec<_> = c
        .reflexes
        .iter()
        .filter(|r| related_trials.contains(&r.source_trial))
        .collect();
    let recoveries: Vec<_> = c
        .recoveries
        .iter()
        .filter(|r| related_trials.contains(&r.source_trial))
        .collect();
    let lessons: Vec<_> = c
        .lessons
        .iter()
        .filter(|l| ids.contains(&l.source_experience))
        .collect();
    let mut high_gaps = vec![];
    let mut critical = 0;
    let current_test = |t: &ResilienceTest| {
        t.with
            .as_ref()
            .and_then(|id| c.experiences.iter().find(|e| e.id == *id))
            .is_some_and(|e| {
                e.starting_state == state
                    && e.evaluation.spec == source.evaluation.spec
                    && e.context.environment.fingerprint == *env
                    && e.agent == source.agent
                    && c.now.signed_duration_since(e.created_at)
                        <= chrono::Duration::days(c.config.stale_after_days as i64)
            })
    };
    let current_trial = |id: &ChaosTrialId| {
        c.experiences.iter().any(|e| {
            e.starting_state == state
                && e.resilience
                    .as_ref()
                    .and_then(|r| r.origin.as_ref())
                    .is_some_and(|o| o.trial_id == *id)
        })
    };
    for o in coverage
        .dimensions
        .iter()
        .flat_map(|d| &d.tested)
        .filter(|o| o.outcome == ChaosTrialOutcome::Fail)
    {
        if o.severity == Severity::Critical {
            critical += 1;
        }
        if o.severity >= Severity::High {
            let e = store.experience(&o.experience_id)?;
            let recovered = recoveries.iter().any(|r| {
                matches!(
                    r.status,
                    RecoveryStatus::Supported | RecoveryStatus::Validated
                ) && e
                    .failure_signatures
                    .iter()
                    .any(|s| s.signature == r.failure_signature.signature)
                    && c.tests.iter().any(|t| {
                        t.recovery_id.as_ref() == Some(&r.id)
                            && t.status == ResilienceTestStatus::Supported
                            && current_test(t)
                    })
            });
            if !recovered {
                high_gaps.push(o.condition.clone());
            }
        }
    }
    high_gaps.sort();
    high_gaps.dedup();
    let reflex_gaps = reflexes
        .iter()
        .filter(|r| current_trial(&r.source_trial))
        .filter(|r| {
            r.status != ReflexStatus::Retired
                && !(r.status == ReflexStatus::Disabled
                    && c.tests.iter().any(|t| {
                        t.reflex_id.as_ref() == Some(&r.id)
                            && t.status == ResilienceTestStatus::FalsePositive
                    }))
        })
        .filter(|r| {
            !c.tests.iter().any(|t| {
                t.reflex_id.as_ref() == Some(&r.id)
                    && t.status == ResilienceTestStatus::NegativeControlPassed
                    && current_test(t)
            })
        })
        .map(|r| r.id.clone())
        .collect();
    let evidence = SkillEvidenceSummary {
        usage: UsageStatistics {
            execution_count: experiences.len() as u64,
            recent_execution_count: experiences
                .iter()
                .filter(|e| c.now.signed_duration_since(e.created_at) <= chrono::Duration::days(30))
                .count() as u64,
            failure_count: experiences
                .iter()
                .filter(|e| e.outcome == Outcome::Failure)
                .count() as u64,
        },
        base_successes: base_ids.len(),
        base_failed: experiences.iter().any(|e| {
            e.starting_state == state
                && e.created_at > latest.created_at
                && e.outcome == Outcome::Failure
                && e.experiment.is_none()
                && e.resilience.as_ref().is_none_or(|r| {
                    r.perturbation_ids.is_empty()
                        && r.recovery_attempt.is_none()
                        && r.reflex_matches.is_empty()
                })
        }),
        tested_dimensions: coverage
            .dimensions
            .iter()
            .filter(|d| d.name != "normal" && !d.tested.is_empty())
            .count(),
        unresolved_critical: critical,
        high_failure_recovery_gaps: high_gaps,
        reflex_check_gaps: reflex_gaps,
        freshness,
    };
    let maturity = ConfiguredMaturityPolicy(&c.config).evaluate(s, &evidence);
    let envelopes: Vec<_> = c
        .envelopes
        .iter()
        .filter(|e| matches!(&e.target,ChaosTarget::Skill(id) if id==&s.id))
        .map(|e| e.id.clone())
        .collect();
    let mut provenance = vec![PackageProvenance {
        kind: "skill".into(),
        id: s.id.to_string(),
        version: None,
        evidence: s.evidence.clone(),
    }];
    for e in &experiences {
        provenance.push(PackageProvenance {
            kind: "experience".into(),
            id: e.id.to_string(),
            version: None,
            evidence: vec![EvidenceRef::Experience {
                experience_id: e.id.clone(),
                relationship: crate::lesson::EvidenceRelationship::Origin,
            }],
        });
    }
    for l in &lessons {
        provenance.push(PackageProvenance {
            kind: "lesson".into(),
            id: l.id.to_string(),
            version: Some(l.version),
            evidence: l.evidence.clone(),
        });
    }
    for r in &reflexes {
        provenance.push(PackageProvenance {
            kind: "reflex".into(),
            id: r.id.to_string(),
            version: Some(r.version),
            evidence: r.evidence.clone(),
        });
    }
    for r in &recoveries {
        provenance.push(PackageProvenance {
            kind: "recovery".into(),
            id: r.id.to_string(),
            version: Some(r.version),
            evidence: r.evidence.clone(),
        });
    }
    Ok(ExperiencePackage {
        skill: s.id.clone(),
        operating_envelope: envelopes.last().cloned(),
        operating_envelopes: envelopes,
        lessons: lessons.iter().map(|l| l.id.clone()).collect(),
        reflexes: reflexes.iter().map(|r| r.id.clone()).collect(),
        recoveries: recoveries.iter().map(|r| r.id.clone()).collect(),
        coverage,
        maturity,
        evidence,
        provenance,
        generated_at: c.now,
    })
}
