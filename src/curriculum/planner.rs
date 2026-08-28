// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    budget::{ExperienceBudget, ExperienceUsage},
    core::*,
    experience::{EnvironmentContext, Experience},
    experimentation::*,
    lesson::{EvidenceRef, LessonStatus},
    perturbation::{Perturbation, PerturbationParameters},
    resilience::*,
};

pub trait CurriculumPlanner {
    fn plan(
        &self,
        target: &CurriculumTarget,
        context: &CurriculumContext,
        budget: &ExperienceBudget,
    ) -> Result<Curriculum>;
}
pub struct DeterministicCurriculumPlanner;
impl CurriculumPlanner for DeterministicCurriculumPlanner {
    fn plan(
        &self,
        target: &CurriculumTarget,
        c: &CurriculumContext,
        budget: &ExperienceBudget,
    ) -> Result<Curriculum> {
        c.config.validate()?;
        let limit = budget.max_curriculum_trials.unwrap_or(c.config.max_trials);
        if limit == 0
            || limit > c.config.max_trials
            || budget.max_realities > c.config.max_realities
            || budget.max_agent_runs > c.config.max_agent_runs
            || budget.max_parallel_trials.unwrap_or(1) != 1
            || budget
                .max_duration_ms
                .is_none_or(|ms| ms == 0 || ms > c.config.max_duration_seconds * 1000)
            || budget.max_commands_per_reality.is_some()
        {
            return Err(Error::InvalidInput("Curriculum budget exceeds local limits or requests an unenforceable command cap/parallelism; all controls and response arms count".into()));
        }
        let mut proposals: Vec<(CurriculumGoal, Option<CurriculumTrial>)> = vec![];
        for skill in &c.skills {
            if !matches!(
                skill.status,
                SkillStatus::Supported | SkillStatus::Validated
            ) || skill.procedure.len() != 1
                || skill.procedure[0].shell_script().is_none()
            {
                return Err(Error::InvalidInput(
                    "Curriculum requires a supported, replayable single-script Skill".into(),
                ));
            }
            let source = c
                .experiences
                .iter()
                .find(|e| e.id == skill.source_experience)
                .ok_or_else(|| Error::NotFound("Skill source experience missing".into()))?;
            source.evaluation.spec.validate()?;
            if source.evaluation.spec.checks.is_empty() {
                return Err(Error::InvalidInput(
                    "Curriculum needs an explicit evaluator".into(),
                ));
            }
            let state = crate::dojo::capture_state(&source.starting_state.repo_path)?;
            let package = c
                .packages
                .iter()
                .find(|p| p.skill == skill.id)
                .ok_or_else(|| Error::InvalidInput("Missing Skill evidence inventory".into()))?;
            let kind = fixture_kind(source);
            for condition in &c.profile.conditions {
                let known = package
                    .coverage
                    .dimensions
                    .iter()
                    .flat_map(|d| &d.tested)
                    .any(|o| o.condition == condition.name);
                if known {
                    continue;
                }
                let safety = if !condition.supported
                    || (condition.fixture_only && !fixture_supports(kind, condition))
                {
                    TrialSafety::Unsupported
                } else {
                    TrialSafety::Safe
                };
                let gap = EvidenceGap {
                    dimension: condition.dimension.clone(),
                    known_values: package
                        .coverage
                        .dimensions
                        .iter()
                        .find(|d| d.name == condition.dimension)
                        .map(|d| d.tested.iter().map(|o| o.condition.clone()).collect())
                        .unwrap_or_default(),
                    unknown_values: vec![condition.name.clone()],
                    rationale: format!(
                        "{} has {} matching executions, but no recent conclusive observation of {} for this Skill, commit, agent, evaluator and runtime",
                        skill.name, package.evidence.usage.execution_count, condition.name
                    ),
                };
                let g = goal(
                    c,
                    if condition.name == "control" {
                        CurriculumGoalKind::ValidateSkill
                    } else {
                        CurriculumGoalKind::ExploreUnknownCondition
                    },
                    gap,
                    condition.severity,
                    safety,
                    budget,
                );
                let execution = if condition.name == "control" {
                    experiment(
                        source,
                        &state,
                        skill.procedure[0].shell_script().unwrap_or_default(),
                        "Replicate base Skill",
                        1,
                        budget,
                    )
                } else {
                    TrialExecution::Chaos {
                        plan: Box::new(campaign_plan(
                            skill,
                            source,
                            state.clone(),
                            condition,
                            budget,
                            kind,
                        )?),
                    }
                };
                let fp = fingerprint(
                    skill,
                    &state,
                    &source.agent,
                    &source.evaluation.spec,
                    condition,
                )?;
                let trial = trial(
                    skill,
                    &g,
                    condition.name.clone(),
                    fp,
                    execution,
                    TrialIntent::NovelExploration,
                );
                proposals.push((g, Some(trial)));
            }
            if package.evidence.freshness.stale
                || (package.evidence.base_successes < 2
                    && !proposals.iter().any(|(_, t)| {
                        t.as_ref().is_some_and(|t| {
                            t.skill_id == skill.id
                                && matches!(t.execution, TrialExecution::Chaos { .. })
                        })
                    }))
            {
                let gap = EvidenceGap {
                    dimension: "freshness".into(),
                    known_values: vec![source.starting_state.git_commit.clone()],
                    unknown_values: vec![state.git_commit.clone()],
                    rationale: if package.evidence.freshness.stale {
                        package.evidence.freshness.reasons.join("; ")
                    } else {
                        "Base Skill has not been independently replayed under current controlled conditions".into()
                    },
                };
                let g = goal(
                    c,
                    if package.evidence.freshness.stale {
                        CurriculumGoalKind::RevalidateOldExperience
                    } else {
                        CurriculumGoalKind::ValidateSkill
                    },
                    gap,
                    Severity::Medium,
                    TrialSafety::Safe,
                    budget,
                );
                let condition = condition("control")?;
                let fp = fingerprint(
                    skill,
                    &state,
                    &source.agent,
                    &source.evaluation.spec,
                    &condition,
                )?;
                // A single control trial covers both the missing base and freshness goals.
                proposals.retain(|(_, t)| {
                    t.as_ref()
                        .is_none_or(|t| t.skill_id != skill.id || t.condition != "control")
                });
                let t = trial(
                    skill,
                    &g,
                    "control".into(),
                    fp,
                    experiment(
                        source,
                        &state,
                        skill.procedure[0].shell_script().unwrap_or_default(),
                        "Revalidate base Skill",
                        1,
                        budget,
                    ),
                    TrialIntent::Revalidation,
                );
                proposals.push((g, Some(t)));
            }
            for id in &package.recoveries {
                let Some(r) = c.recoveries.iter().find(|r| r.id == *id) else {
                    continue;
                };
                if r.status != RecoveryStatus::Candidate {
                    continue;
                }
                let gap=EvidenceGap {dimension:"recovery".into(),known_values:vec![r.failure_signature.signature.clone()],unknown_values:vec!["tested recovery".into()],rationale:"A known failure has only a Candidate recovery; paired failure reproduction and restoration are missing".into()};
                let g = goal(
                    c,
                    CurriculumGoalKind::TestRecovery,
                    gap,
                    Severity::High,
                    TrialSafety::Safe,
                    budget,
                );
                let fp = hash(&(skill.id.clone(), r.id.clone(), r.version, "recovery"))?;
                let t = trial(
                    skill,
                    &g,
                    format!("recovery:{}", r.id),
                    fp,
                    TrialExecution::Recovery {
                        recovery_id: r.id.clone(),
                        version: r.version,
                    },
                    TrialIntent::Revalidation,
                );
                proposals.push((g, Some(t)));
            }
            for condition in &package.evidence.high_failure_recovery_gaps {
                let signatures: Vec<_> = package
                    .coverage
                    .dimensions
                    .iter()
                    .flat_map(|d| &d.tested)
                    .filter(|o| &o.condition == condition && o.outcome == ChaosTrialOutcome::Fail)
                    .filter_map(|o| c.experiences.iter().find(|e| e.id == o.experience_id))
                    .flat_map(|e| &e.failure_signatures)
                    .map(|s| s.signature.as_str())
                    .collect();
                let matching_candidate = c.recoveries.iter().any(|r| {
                    package.recoveries.contains(&r.id)
                        && r.status == RecoveryStatus::Candidate
                        && signatures.contains(&r.failure_signature.signature.as_str())
                });
                if matching_candidate {
                    continue;
                }
                let gap=EvidenceGap {dimension:"recovery".into(),known_values:vec![condition.clone()],unknown_values:vec!["recovery recipe".into()],rationale:"High-severity failure has no matching Candidate recovery to test. Inspect any prior response evidence and supply a bounded local recipe; an unrelated recovery does not close this gap".into()};
                let mut g = goal(
                    c,
                    CurriculumGoalKind::TestRecovery,
                    gap,
                    Severity::High,
                    TrialSafety::Unsupported,
                    budget,
                );
                g.status = GoalStatus::Deferred;
                g.reason = "No executable recovery recipe".into();
                proposals.push((g, None));
            }
            for id in &package.evidence.reflex_check_gaps {
                let Some(r) = c.reflexes.iter().find(|r| r.id == *id) else {
                    continue;
                };
                let conditions = vec![Perturbation::new(
                    if r.trigger.repeated_failures.is_some() {
                        PerturbationParameters::CommandFailure {
                            failures: r.trigger.repeated_failures.unwrap_or(3),
                            exit_code: 17,
                        }
                    } else {
                        PerturbationParameters::CommandDelay { milliseconds: 0 }
                    },
                )];
                let gap=EvidenceGap {dimension:"reflex".into(),known_values:vec![r.id.to_string()],unknown_values:vec!["negative control".into()],rationale:"This Reflex lacks a negative control where original behavior succeeds; challenge it before trusting it".into()};
                let g = goal(
                    c,
                    CurriculumGoalKind::ValidateReflex,
                    gap,
                    Severity::Medium,
                    TrialSafety::Safe,
                    budget,
                );
                let t = trial(
                    skill,
                    &g,
                    format!("reflex:{}", r.id),
                    hash(&(
                        skill.id.clone(),
                        r.id.clone(),
                        r.version,
                        "negative-control",
                    ))?,
                    TrialExecution::Reflex {
                        reflex_id: r.id.clone(),
                        version: r.version,
                        conditions,
                    },
                    TrialIntent::Revalidation,
                );
                proposals.push((g, Some(t)));
            }
            for l in &c.lessons {
                if l.status != LessonStatus::Contradicted
                    || !l.context_match.matches(&source.context)
                {
                    continue;
                }
                let mut contexts = vec![l.source_experience.clone()];
                contexts.extend(l.evidence.iter().filter_map(|e| {
                    match e {
                        EvidenceRef::Experience { experience_id, .. } => {
                            Some(experience_id.clone())
                        }
                        EvidenceRef::Trial {
                            experiment_id,
                            trial_id,
                            ..
                        } => c
                            .experiments
                            .iter()
                            .find(|e| e.id == *experiment_id)
                            .and_then(|e| e.trials.iter().find(|t| t.spec.id == *trial_id))
                            .map(|t| t.experience_id.clone()),
                    }
                }));
                let mut states = std::collections::HashSet::new();
                for id in contexts {
                    let Some(e) = c.experiences.iter().find(|e| e.id == id) else {
                        continue;
                    };
                    if !states.insert((
                        e.starting_state.repo_path.clone(),
                        e.starting_state.git_commit.clone(),
                    )) {
                        continue;
                    }
                    let (Some(avoid), Some(prefer)) = (
                        l.avoid.as_ref().and_then(|a| a.shell_script()),
                        l.prefer.as_ref().and_then(|a| a.shell_script()),
                    ) else {
                        continue;
                    };
                    let gap=EvidenceGap {dimension:"contradiction".into(),known_values:vec![l.id.to_string(),e.starting_state.git_commit.clone()],unknown_values:vec!["context boundary".into()],rationale:"Contradictory Lesson evidence: replay alternatives in each recorded context, then mark for scope review; no automatic scope rewrite".into()};
                    let g = goal(
                        c,
                        CurriculumGoalKind::ResolveContradiction,
                        gap,
                        Severity::High,
                        TrialSafety::Safe,
                        budget,
                    );
                    let mut execution = experiment(
                        e,
                        &e.starting_state,
                        avoid,
                        "Investigate contradictory Lesson",
                        2,
                        budget,
                    );
                    if let TrialExecution::Experiment { request } = &mut execution {
                        request.candidates[1].execution = CandidateExecution::Shell {
                            commands: vec![prefer.into()],
                        };
                    }
                    let t = trial(
                        skill,
                        &g,
                        format!("contradiction:{}:{}", l.id, e.id),
                        hash(&(l.id.clone(), l.version, &e.starting_state, avoid, prefer))?,
                        execution,
                        TrialIntent::Revalidation,
                    );
                    proposals.push((g, Some(t)));
                }
            }
        }
        proposals.sort_by(|(a, ta), (b, tb)| {
            b.score.score.cmp(&a.score.score).then_with(|| {
                ta.as_ref()
                    .map(|t| (&t.condition, t.skill_id.to_string()))
                    .cmp(&tb.as_ref().map(|t| (&t.condition, t.skill_id.to_string())))
            })
        });
        let mut goals = vec![];
        let mut trials = vec![];
        let mut reserved = ExperienceUsage::default();
        // Reserve one slot for the only permitted adaptive step: a newly exposed recovery gap.
        let first_limit = if c.config.max_rounds == 2 && limit > 1 {
            limit - 1
        } else {
            limit
        };
        for (mut g, t) in proposals {
            if let Some(t) = t {
                let recent = c.history.iter().filter(|h| {
                    c.now.signed_duration_since(h.updated_at)
                        <= chrono::Duration::days(c.config.stale_after_days as i64)
                });
                let observations = recent
                    .flat_map(|h| &h.trials)
                    .filter(|t| t.status == GoalStatus::Completed)
                    .filter_map(|t| {
                        t.result
                            .as_ref()
                            .map(|r| (t.fingerprint.clone(), r.experiences.clone()))
                    })
                    .collect();
                let duplicate = (ExactNoveltyPolicy { observations }.score(&t, &c.experiences)
                    == 0.0)
                    || c.history
                        .iter()
                        .filter(|h| !h.status.terminal())
                        .flat_map(|h| &h.trials)
                        .any(|old| {
                            old.fingerprint == t.fingerprint
                                && matches!(old.status, GoalStatus::Planned | GoalStatus::Running)
                        });
                let fits = trials.len() < first_limit
                    && reserved.realities + t.estimated_budget.realities <= budget.max_realities
                    && reserved.agent_runs + t.estimated_budget.agent_runs <= budget.max_agent_runs;
                if g.decision == CurriculumDecision::Approved && fits && !duplicate {
                    reserved.realities += t.estimated_budget.realities;
                    reserved.agent_runs += t.estimated_budget.agent_runs;
                    trials.push(t);
                } else if g.decision == CurriculumDecision::Approved {
                    g.status = GoalStatus::Deferred;
                    g.decision = CurriculumDecision::Reduced;
                    g.reason = if duplicate {
                        "Exact trial already queued/running or recently observed"
                    } else {
                        "Aggregate budget or first-round reservation; no partial comparison"
                    }
                    .into();
                }
            }
            goals.push(g);
        }
        Ok(Curriculum {
            id: CurriculumId::new(),
            target: target.clone(),
            profile: c.profile.name.clone(),
            goals,
            trials,
            budget: budget.clone(),
            usage: ExperienceUsage::default(),
            reserved: ExperienceUsage::default(),
            trials_executed: 0,
            status: CurriculumStatus::Planned,
            created_at: c.now,
            updated_at: c.now,
            rounds: 1,
            max_rounds: c.config.max_rounds,
            revision: 1,
            before: c.packages.clone(),
            after: vec![],
            stop_reason: None,
            session_id: None,
            quality: CurriculumQuality::Medium,
        })
    }
}
fn hash(v: &impl serde::Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(v)?).to_hex().to_string())
}
fn goal(
    c: &CurriculumContext,
    kind: CurriculumGoalKind,
    gap: EvidenceGap,
    severity: Severity,
    safety: TrialSafety,
    budget: &ExperienceBudget,
) -> CurriculumGoal {
    let score = TransparentPriorityPolicy.priority(&gap, c);
    let mut g=CurriculumGoal {id:CurriculumGoalId::new(),kind,description:format!("{:?}: {}",kind,gap.unknown_values.join(", ")),priority:score.priority,score,evidence_gap:gap,status:GoalStatus::Planned,decision:CurriculumDecision::Approved,reason:"Supported local condition and explicit evaluator; controls are included in estimated cost".into(),severity,safety};
    g.decision = LocalCurriculumPolicy.evaluate(&g, &c.capabilities, budget);
    if g.decision != CurriculumDecision::Approved {
        g.status = GoalStatus::Rejected;
        g.reason="Unsupported condition, external effects, or insufficient provider isolation; no trial scheduled".into();
    }
    g
}
fn trial(
    skill: &Skill,
    g: &CurriculumGoal,
    condition: String,
    fingerprint: String,
    execution: TrialExecution,
    intent: TrialIntent,
) -> CurriculumTrial {
    let (realities, agents) = match &execution {
        TrialExecution::Experiment { request } => (request.candidates.len(), 0),
        _ => (2, 2),
    };
    CurriculumTrial {id:CurriculumTrialId::new(),goal_id:g.id.clone(),skill_id:skill.id.clone(),condition,fingerprint,intent,execution,result:None,learning_outcome:None,status:GoalStatus::Planned,estimated_budget:ExperienceUsage {realities,agent_runs:agents,..Default::default()},expected_value:"Observe one exact condition or paired response; retain evidence and update only the tested scope".into(),required_isolation:RealityCapabilities::default(),round:1}
}
pub fn fixture_supports(kind: Option<FixtureKind>, condition: &CatalogCondition) -> bool {
    match kind {
        Some(FixtureKind::SkillHardening | FixtureKind::SkillHardeningTransfer) => true,
        Some(FixtureKind::StaleCredential) => condition.dimension == "credential_state",
        Some(FixtureKind::ConfigDrift) => condition.dimension == "configuration",
        _ => false,
    }
}
fn campaign_plan(
    skill: &Skill,
    source: &Experience,
    state: StateRef,
    condition: &CatalogCondition,
    budget: &ExperienceBudget,
    fixture: Option<FixtureKind>,
) -> Result<CampaignPlan> {
    let environment = EnvironmentContext::capture(&state.repo_path, EnvironmentMode::Controlled)?;
    Ok(CampaignPlan {
        target: ChaosTarget::Skill(skill.id.clone()),
        starting_state: state,
        goal: source.goal.clone(),
        command: CommandSpec::shell(
            skill.procedure[0].shell_script().unwrap_or_default(),
            EnvironmentMode::Controlled,
        ),
        evaluation: source.evaluation.spec.clone(),
        agent: source.agent.clone(),
        fixture,
        perturbations: vec![
            condition
                .parameters
                .clone()
                .map(Perturbation::new)
                .into_iter()
                .collect(),
        ],
        trial_budget: 1,
        timeout_secs: source
            .replay
            .as_ref()
            .map(|r| r.timeout_secs)
            .unwrap_or(30)
            .clamp(1, 300),
        max_duration_secs: (budget.max_duration_ms.unwrap_or(300_000) / 1000).clamp(1, 3600),
        environment,
        hardknock_version: env!("CARGO_PKG_VERSION").into(),
        runtime_version: crate::resilience::fixture::RUNTIME_VERSION.into(),
        fixture_version: fixture.map(|_| "1".into()),
        active_reflexes: vec![],
    })
}
fn experiment(
    source: &Experience,
    state: &StateRef,
    script: &str,
    question: &str,
    n: usize,
    budget: &ExperienceBudget,
) -> TrialExecution {
    TrialExecution::Experiment {
        request: Box::new(ExperimentRequest {
            id: ExperimentRequestId::new(),
            session_id: "curriculum".into(),
            question: question.into(),
            hypothesis: None,
            candidates: (0..n)
                .map(|i| ExperimentCandidate {
                    id: CandidateId::new(),
                    name: format!("arm-{i}"),
                    description: String::new(),
                    execution: CandidateExecution::Shell {
                        commands: vec![script.into()],
                    },
                    expected_outcome: None,
                })
                .collect(),
            starting_state: ExperimentStartingState {
                state_ref: state.clone(),
                expected_fingerprint: None,
                parent_reality: None,
                source: SnapshotSource::RepositoryCommit,
            },
            evaluator: source.evaluation.spec.clone(),
            budget: budget.clone(),
            requested_by: source.agent.clone(),
            created_at: chrono::Utc::now(),
            criteria: ComparisonCriteria::default(),
            origin: ExperimentOrigin::User,
            intent: ExperimentIntent::ValidateHypothesis,
            capabilities: ExperimentCapabilities::default(),
        }),
    }
}
