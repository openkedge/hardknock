// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    application::RunLearningOptions,
    bridge::config::Config,
    cancellation::Cancellation,
    core::*,
    curriculum::*,
    dojo::capture_state,
    evaluation::EvaluationSpec,
    experience::{EnvironmentContext, Outcome, ReplaySpec},
    perturbation::{Perturbation, PerturbationParameters},
    resilience::{runtime::RunResilienceOptions, *},
    store::Store,
    workflow::{RunRequest, run_with_resilience},
};
use std::{collections::BTreeMap, fs};
use support::{Fixture, git};

fn config() -> Config {
    let mut c = Config::default();
    c.curriculum.profiles.insert(
        "hardening".into(),
        ProfileConfig {
            conditions: vec![
                "delay:500".into(),
                "env:missing".into(),
                "config:drift".into(),
                "dependency:unavailable".into(),
            ],
        },
    );
    c
}
fn request(f: &Fixture, _kind: FixtureKind) -> RunRequest {
    RunRequest {
        state: capture_state(&f.repo).unwrap(),
        goal: "process-task-successfully".into(),
        agent: AgentIdentity {
            kind: "test-agent".into(),
            executable: "/bin/sh".into(),
            version: Some(hardknock::resilience::fixture::RUNTIME_VERSION.into()),
            model: None,
        },
        command: CommandSpec::shell("/bin/sh ./operation.sh", EnvironmentMode::Controlled),
        evaluation: EvaluationSpec {
            checks: vec!["/bin/sh ./test.sh".into()],
        },
        timeout_secs: 10,
        keep: false,
        replay: Some(ReplaySpec {
            script: "/bin/sh ./operation.sh".into(),
            timeout_secs: 10,
        }),
        perturbations: vec![],
        expected_fingerprint: Some(
            EnvironmentContext::capture(&f.repo, EnvironmentMode::Controlled)
                .unwrap()
                .fingerprint,
        ),
    }
}
async fn seed(f: &Fixture, store: &Store, kind: FixtureKind) -> Skill {
    let result = run_with_resilience(
        store,
        request(f, kind),
        &RunLearningOptions::default(),
        &RunResilienceOptions {
            fixture: Some(kind),
            ..Default::default()
        },
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.experience.outcome, Outcome::Success);
    store
        .register_skill("process-task-successfully", &result.experience.id)
        .unwrap()
}
fn clean(f: &Fixture, store: &Store) {
    f.assert_source_unchanged();
    assert!(
        store
            .realities()
            .unwrap()
            .iter()
            .all(|r| r.status == RealityStatus::Discarded)
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn maturity_freshness_suggestion_and_replication_policies_are_conservative() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let skill = seed(&f, &store, FixtureKind::SkillHardening).await;
    let cfg = config();
    let ctx = inventory(
        &store,
        &CurriculumTarget::Skill(skill.id.clone()),
        "hardening",
        &cfg.curriculum,
    )
    .unwrap();
    let mut evidence = ctx.packages[0].evidence.clone();
    evidence.base_successes = 2;
    evidence.tested_dimensions = 3;
    let policy = ConfiguredMaturityPolicy(&cfg.curriculum);
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Hardened);
    evidence
        .high_failure_recovery_gaps
        .push("credential:stale".into());
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Validated);
    evidence.high_failure_recovery_gaps.clear();
    evidence.reflex_check_gaps.push(ReflexId::new());
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Validated);
    evidence.reflex_check_gaps.clear();
    evidence.unresolved_critical = 1;
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Degraded);
    evidence.unresolved_critical = 0;
    evidence.base_failed = true;
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Degraded);
    evidence.base_failed = false;
    evidence.freshness.stale = true;
    assert_eq!(policy.evaluate(&skill, &evidence), SkillMaturity::Supported);
    let source = store.experience(&skill.source_experience).unwrap();
    let query = hardknock::retrieval::QueryContext::new(&source.context, &source.goal, vec![]);
    let now = chrono::Utc::now();
    let policy = ConservativeFreshnessPolicy { now, age_days: 30 };
    let mut summary = EvidenceSummary {
        last_supported_at: now - chrono::Duration::days(30),
        environment: source.context.clone(),
        agent: source.agent.clone(),
    };
    assert!(!policy.evaluate(&summary, &query).stale);
    summary.last_supported_at -= chrono::Duration::seconds(1);
    let freshness = policy.evaluate(&summary, &query);
    assert!(freshness.stale);
    assert!(freshness.reasons[0].contains("do not delete"));
    let suggestions = validate_suggestions(
        &ctx,
        vec![
            CurriculumSuggestion {
                condition: "delay:500".into(),
                rationale: "A catalog condition".into(),
            },
            CurriculumSuggestion {
                condition: "send-email:real".into(),
                rationale: "Untrusted suggestion".into(),
            },
        ],
    )
    .unwrap();
    assert_eq!(suggestions[0].1, CurriculumDecision::RequiresApproval);
    assert_eq!(suggestions[1].1, CurriculumDecision::Rejected);
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let target = CurriculumTarget::Skill(skill.id);
    let first = engine
        .plan(
            target.clone(),
            "hardening",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    let duplicate = engine
        .plan(
            target.clone(),
            "hardening",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert!(duplicate.trials.is_empty());
    let replicated = engine
        .plan_replication(target, "hardening", &cfg.curriculum.budget(4).unwrap())
        .unwrap();
    assert_eq!(replicated.trials.len(), 4);
    assert!(
        replicated
            .trials
            .iter()
            .all(|t| t.intent == TrialIntent::Replication)
    );
    assert_eq!(
        first.before[0].coverage.profile_coverage,
        replicated.before[0].coverage.profile_coverage
    );
    let mut bad = first.clone();
    bad.trials[0].estimated_budget.realities = 0;
    assert!(engine.validate(&bad).is_err());
    bad = first.clone();
    bad.trials[0].required_isolation.network_isolation = IsolationLevel::Isolated;
    assert!(engine.validate(&bad).is_err());
    bad = first;
    if let TrialExecution::Chaos { plan } = &mut bad.trials[0].execution {
        plan.command =
            CommandSpec::shell("sendmail real@example.invalid", EnvironmentMode::Controlled);
    }
    assert!(engine.validate(&bad).is_err());
    assert_eq!(
        store.realities().unwrap().len(),
        1,
        "All tests above are planning only"
    );
    clean(&f, &store);
}

#[tokio::test]
async fn changing_runtime_or_commit_does_not_reuse_old_response_support_for_hardening() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::SkillHardening).await;
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    for budget in [4, 3] {
        let c = engine
            .plan(
                CurriculumTarget::Skill(s.id.clone()),
                "hardening",
                &cfg.curriculum.budget(budget).unwrap(),
            )
            .unwrap();
        assert_eq!(
            engine
                .run(&c.id, &Cancellation::default())
                .await
                .unwrap()
                .status,
            CurriculumStatus::Completed
        );
    }
    assert_eq!(
        skill_package(&store, &s.name, "hardening", &cfg.curriculum)
            .unwrap()
            .maturity,
        SkillMaturity::Hardened
    );
    fs::write(f.repo.join("environment-version"), "2\n").unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "new context"]);
    let c = engine
        .plan(
            CurriculumTarget::Skill(s.id),
            "hardening",
            &cfg.curriculum.budget(5).unwrap(),
        )
        .unwrap();
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed);
    assert_eq!(c.after[0].maturity, SkillMaturity::Validated);
    assert_eq!(c.after[0].evidence.high_failure_recovery_gaps.len(), 2);
    clean(&f, &store);
}

#[tokio::test]
async fn contradictory_lesson_schedules_both_recorded_contexts_and_marks_review() {
    let f = Fixture::pnpm();
    let learned = f.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "upgrade demo dependencies",
        ],
        0,
    );
    let lesson_id: LessonId = learned["lesson"]["id"].as_str().unwrap().parse().unwrap();
    let mut other = Fixture::from_fixture("pnpm-workspace-contradiction");
    other.home = f.home.clone();
    let contradicted = other.cli(&["lesson", "test", &lesson_id.to_string()], 0);
    assert_eq!(contradicted["lesson"]["status"], "contradicted");
    let store = Store::open(&f.home).unwrap();
    let skill = store
        .register_skill(
            "upgrade",
            &learned["retries"][0]["experience"]["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
    let before = store.lesson(&lesson_id).unwrap();
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let c = engine
        .plan(
            CurriculumTarget::Skill(skill.id),
            "latency-basic",
            &cfg.curriculum.budget(2).unwrap(),
        )
        .unwrap();
    assert_eq!(c.trials.len(), 2);
    assert!(
        c.trials
            .iter()
            .all(|t| t.condition.starts_with("contradiction:"))
    );
    let states: std::collections::HashSet<_> = c
        .trials
        .iter()
        .filter_map(|t| {
            if let TrialExecution::Experiment { request } = &t.execution {
                Some(request.starting_state.state_ref.repo_path.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(states.len(), 2);
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed, "{:?}", c.stop_reason);
    assert_eq!(c.usage.realities, 4);
    assert_eq!(store.curriculum_reviews().unwrap().len(), 2);
    assert_eq!(store.lesson(&lesson_id).unwrap().version, before.version);
    assert_eq!(
        store.lesson(&lesson_id).unwrap().context_match,
        before.context_match
    );
    clean(&f, &store);
    other.assert_source_unchanged();
}
#[tokio::test]
async fn hardening_records_exact_conditions_and_requires_tested_responses_for_maturity() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let skill = seed(&f, &store, FixtureKind::SkillHardening).await;
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let c = engine
        .plan(
            CurriculumTarget::Skill(skill.id.clone()),
            "hardening",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert_eq!(c.before[0].coverage.profile_coverage, Some(0.2));
    assert_eq!(c.before[0].maturity, SkillMaturity::Supported);
    assert_eq!(c.trials.len(), 4);
    assert!(c.goals.iter().all(|g| !g.evidence_gap.rationale.is_empty()));
    assert_eq!(
        store.realities().unwrap().len(),
        1,
        "Planning must not execute"
    );
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed, "{:?}", c.stop_reason);
    let outcomes: BTreeMap<_, _> = c
        .trials
        .iter()
        .map(|t| {
            (
                t.condition.as_str(),
                t.result.as_ref().unwrap().outcome.unwrap(),
            )
        })
        .collect();
    assert_eq!(outcomes["delay:500"], ChaosTrialOutcome::Pass);
    assert_eq!(outcomes["env:missing"], ChaosTrialOutcome::Fail);
    assert_eq!(outcomes["config:drift"], ChaosTrialOutcome::Fail);
    assert_eq!(
        outcomes["dependency:unavailable"],
        ChaosTrialOutcome::Degraded
    );
    let p = &c.after[0];
    assert_eq!(p.lessons.len(), 2);
    assert_eq!(p.reflexes.len(), 1);
    assert_eq!(p.recoveries.len(), 2);
    assert_eq!(p.operating_envelopes.len(), 4);
    assert_eq!(p.coverage.profile_coverage, Some(1.0));
    assert_eq!(p.maturity, SkillMaturity::Validated);
    let mut display = vec![];
    hardknock::cli::curriculum::print_package(&mut display, &skill, p).unwrap();
    let display = String::from_utf8(display).unwrap();
    for item in &p.provenance {
        assert!(display.contains(&item.id));
    }
    assert_eq!(c.usage.realities, 8);
    assert_eq!(c.reserved.realities, 8);
    assert_eq!(c.trials_executed, 4);
    for t in &c.trials {
        let e = t.result.as_ref().unwrap();
        assert!(e.campaign_id.is_some());
        assert_eq!(e.experiences.len(), 2);
        for id in &e.experiences {
            assert!(store.experience(id).unwrap().resilience.is_some());
        }
    }
    // A planner inventory with one missing recipe must keep that class visible,
    // even when another failure already has a Candidate recovery.
    let mut partial = inventory(
        &store,
        &CurriculumTarget::Skill(skill.id.clone()),
        "hardening",
        &cfg.curriculum,
    )
    .unwrap();
    let credential = c
        .trials
        .iter()
        .find(|t| t.condition == "env:missing")
        .unwrap();
    let credential = store
        .experience(
            credential
                .result
                .as_ref()
                .unwrap()
                .experiences
                .last()
                .unwrap(),
        )
        .unwrap();
    partial.recoveries.retain(|r| {
        !credential
            .failure_signatures
            .iter()
            .any(|s| s.signature == r.failure_signature.signature)
    });
    assert_eq!(partial.recoveries.len(), 1);
    let missing = DeterministicCurriculumPlanner
        .plan(
            &CurriculumTarget::Skill(skill.id.clone()),
            &partial,
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert!(
        missing
            .goals
            .iter()
            .any(|g| g.status == GoalStatus::Deferred
                && g.evidence_gap.dimension == "recovery"
                && g.evidence_gap.known_values == ["env:missing"])
    );
    let next = engine
        .plan(
            CurriculumTarget::Skill(skill.id.clone()),
            "hardening",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert_eq!(next.trials.len(), 3);
    assert_eq!(next.goals[0].kind, CurriculumGoalKind::TestRecovery);
    let next = engine
        .run(&next.id, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        next.status,
        CurriculumStatus::Completed,
        "{:?}",
        next.stop_reason
    );
    assert_eq!(next.after[0].maturity, SkillMaturity::Hardened);
    assert!(next.after[0].evidence.high_failure_recovery_gaps.is_empty());
    assert!(next.after[0].evidence.reflex_check_gaps.is_empty());
    let last = engine
        .plan(
            CurriculumTarget::Skill(skill.id),
            "hardening",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert!(last.trials.is_empty());
    let same = engine
        .run(&next.id, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(same.revision, next.revision);
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute(
            "UPDATE curricula SET status='planned' WHERE id=?1",
            [next.id.to_string()]
        )
        .is_err()
    );
    assert!(
        db.execute(
            "UPDATE curriculum_trials SET fingerprint='fake' WHERE id=?1",
            [c.trials[0].id.to_string()]
        )
        .is_err()
    );
    assert!(
        db.execute("UPDATE experience_packages SET data='{}'", [])
            .is_err()
    );
    clean(&f, &store);
}

#[tokio::test]
async fn aggregate_budget_and_unknown_conditions_do_not_create_unsafe_trials() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::SkillHardening).await;
    let mut cfg = config();
    cfg.curriculum.profiles.insert(
        "unsafe".into(),
        ProfileConfig {
            conditions: vec!["send-email:real".into(), "credential:revoked".into()],
        },
    );
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let mut b = cfg.curriculum.budget(5).unwrap();
    b.max_realities = 2;
    b.max_agent_runs = 2;
    let c = engine
        .plan(CurriculumTarget::Skill(s.id.clone()), "hardening", &b)
        .unwrap();
    assert_eq!(c.trials.len(), 1);
    assert!(
        c.goals
            .iter()
            .any(|g| g.decision == CurriculumDecision::Reduced)
    );
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.usage.realities, 2);
    let u = engine
        .plan(CurriculumTarget::Skill(s.id), "unsafe", &b)
        .unwrap();
    assert!(u.trials.iter().all(|t| matches!(
        t.execution,
        TrialExecution::Recovery { .. } | TrialExecution::Reflex { .. }
    )));
    assert_eq!(
        u.goals
            .iter()
            .filter(|g| g.decision == CurriculumDecision::Rejected)
            .count(),
        2
    );
    assert_eq!(u.before[0].coverage.profile_coverage, Some(1.0 / 3.0));
    assert!(
        u.before[0]
            .coverage
            .dimensions
            .iter()
            .flat_map(|d| &d.unknown)
            .any(|u| u == "credential:revoked")
    );
    b.max_commands_per_reality = Some(1);
    assert!(engine.plan(u.target.clone(), "hardening", &b).is_err());
    clean(&f, &store);
}

#[tokio::test]
async fn adaptive_round_only_validates_new_recovery_with_remaining_budget() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::SkillHardening).await;
    let mut cfg = config();
    cfg.curriculum.max_rounds = 2;
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let c = engine
        .plan(
            CurriculumTarget::Skill(s.id),
            "hardening",
            &cfg.curriculum.budget(3).unwrap(),
        )
        .unwrap();
    assert_eq!(c.trials.len(), 2);
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed, "{:?}", c.stop_reason);
    assert_eq!(c.rounds, 2);
    assert_eq!(c.trials.len(), 3);
    assert_eq!(c.trials[2].round, 2);
    assert!(matches!(
        c.trials[2].execution,
        TrialExecution::Recovery { .. }
    ));
    assert_eq!(c.usage.realities, 6);
    assert_eq!(c.trials_executed, 3);
    clean(&f, &store);
}

#[tokio::test]
async fn task_families_use_explicit_context_and_share_one_aggregate_budget() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::SkillHardening).await;
    store
        .register_skill("second-procedure", &s.source_experience)
        .unwrap();
    let family = store
        .register_task_family("task-processing", vec![s.source_experience])
        .unwrap();
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let mut b = cfg.curriculum.budget(5).unwrap();
    b.max_realities = 4;
    b.max_agent_runs = 4;
    let c = engine
        .plan(CurriculumTarget::TaskFamily(family.id), "hardening", &b)
        .unwrap();
    assert_eq!(c.before.len(), 2);
    assert_eq!(c.trials.len(), 2);
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.usage.realities, 4);
    assert_eq!(c.status, CurriculumStatus::Completed);
    clean(&f, &store);
}

#[tokio::test]
async fn stale_commit_recommends_revalidation_without_deleting_experience() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::SkillHardening).await;
    fs::write(f.repo.join("environment-version"), "2\n").unwrap();
    git(&f.repo, &["add", "environment-version"]);
    git(&f.repo, &["commit", "-m", "version 2"]);
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let c = engine
        .plan(
            CurriculumTarget::Skill(s.id),
            "hardening",
            &cfg.curriculum.budget(5).unwrap(),
        )
        .unwrap();
    assert!(
        c.goals
            .iter()
            .any(|g| g.kind == CurriculumGoalKind::RevalidateOldExperience)
    );
    assert!(c.before[0].evidence.freshness.stale);
    let old = store.experience(&s.source_experience).unwrap();
    assert_ne!(
        old.starting_state.git_commit,
        capture_state(&f.repo).unwrap().git_commit
    );
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed, "{:?}", c.stop_reason);
    assert!(!c.after[0].evidence.freshness.stale);
    assert_eq!(
        store.experience(&old.id).unwrap().starting_state,
        old.starting_state
    );
    clean(&f, &store);
}

#[tokio::test]
async fn false_positive_reflex_is_challenged_and_disabled() {
    let f = Fixture::from_fixture("retry-resilience");
    let store = Store::open(&f.home).unwrap();
    let s = seed(&f, &store, FixtureKind::RetryResilience).await;
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let c = engine
        .plan(
            CurriculumTarget::Skill(s.id.clone()),
            "retry-behavior",
            &cfg.curriculum.budget(3).unwrap(),
        )
        .unwrap();
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Completed);
    let next = engine
        .plan(
            CurriculumTarget::Skill(s.id),
            "retry-behavior",
            &cfg.curriculum.budget(4).unwrap(),
        )
        .unwrap();
    assert!(
        next.goals
            .iter()
            .any(|g| g.kind == CurriculumGoalKind::ValidateReflex)
    );
    let next = engine
        .run(&next.id, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        next.status,
        CurriculumStatus::Completed,
        "{:?}",
        next.stop_reason
    );
    let tests = store.resilience_tests().unwrap();
    assert!(
        tests
            .iter()
            .any(|t| t.status == ResilienceTestStatus::FalsePositive)
    );
    let r = store.reflexes().unwrap().pop().unwrap();
    assert_eq!(r.status, ReflexStatus::Disabled);
    assert!(f64::from(r.confidence) < 0.58);
    assert!(store.set_reflex_enabled(&r.id, true).is_err());
    clean(&f, &store);
}

#[tokio::test]
async fn cancellation_and_deadline_keep_partial_evidence_and_clean_realities() {
    let f = Fixture::new();
    fs::write(
        f.repo.join("operation.sh"),
        "#!/bin/sh\nsleep 1\necho success > result\n",
    )
    .unwrap();
    fs::write(f.repo.join("test.sh"), "test \"$(cat result)\" = success\n").unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "slow local task"]);
    let store = Store::open(&f.home).unwrap();
    let r = run_with_resilience(
        &store,
        request(&f, FixtureKind::SkillHardening),
        &RunLearningOptions::default(),
        &RunResilienceOptions::default(),
        &Cancellation::default(),
    )
    .await
    .unwrap();
    let s = store.register_skill("slow", &r.experience.id).unwrap();
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    let mut b = cfg.curriculum.budget(1).unwrap();
    b.max_duration_ms = Some(200);
    let c = engine
        .plan(CurriculumTarget::Skill(s.id.clone()), "latency-basic", &b)
        .unwrap();
    let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Cancelled);
    assert!(c.usage.duration_ms < 4000);
    assert!(c.trials[0].result.as_ref().unwrap().experiences.len() <= 1);
    assert_ne!(c.after[0].maturity, SkillMaturity::Hardened);
    let c = engine
        .plan(
            CurriculumTarget::Skill(s.id.clone()),
            "latency-basic",
            &cfg.curriculum.budget(1).unwrap(),
        )
        .unwrap();
    let cancel = Cancellation::default();
    cancel.cancel();
    let c = engine.run(&c.id, &cancel).await.unwrap();
    assert_eq!(c.status, CurriculumStatus::Cancelled);
    assert_eq!(c.trials_executed, 0);
    let planned = engine
        .plan(
            CurriculumTarget::Skill(s.id),
            "latency-basic",
            &cfg.curriculum.budget(1).unwrap(),
        )
        .unwrap();
    let realities = store.realities().unwrap().len();
    assert!(store.cancel_curriculum(&planned.id).unwrap());
    let cancelled = store.curriculum(&planned.id).unwrap();
    assert_eq!(cancelled.status, CurriculumStatus::Cancelled);
    assert_eq!(cancelled.trials_executed, 0);
    assert!(!store.cancel_curriculum(&planned.id).unwrap());
    let again = engine
        .run(&planned.id, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(again.revision, cancelled.revision);
    assert_eq!(store.realities().unwrap().len(), realities);
    clean(&f, &store);
}

#[tokio::test]
async fn cli_harden_package_report_and_why_are_the_same_core_workflow() {
    let f = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&f.home).unwrap();
    seed(&f, &store, FixtureKind::SkillHardening).await;
    fs::write(f.home.join("config.toml"),"[curriculum.profiles.hardening]\nconditions=['delay:500','env:missing','config:drift','dependency:unavailable']\n").unwrap();
    let result = f.cli(
        &[
            "skill",
            "harden",
            "process-task-successfully",
            "--profile",
            "hardening",
            "--budget",
            "4",
        ],
        0,
    );
    assert_eq!(result["event"], "curriculum");
    let id = result["result"]["curriculum"]["id"].as_str().unwrap();
    for command in ["show", "why", "report"] {
        let r = f.cli(&["curriculum", command, id], 0);
        assert_eq!(r["result"]["report"]["summary"]["trials"], 4);
    }
    assert_eq!(
        f.cli(
            &[
                "skill",
                "package",
                "process-task-successfully",
                "--profile",
                "hardening"
            ],
            0
        )["result"]["package"]["coverage"]["profile_coverage"],
        1.0
    );
    assert!(
        f.cli(&["skill", "show", "process-task-successfully"], 0)["result"]["package"].is_object()
    );
    assert_eq!(
        f.cli(&["curriculum", "list"], 0)["result"]["curricula"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    clean(&f, &store);
}

#[tokio::test]
async fn held_out_resilience_benchmark_compares_no_experience_lessons_and_full_package() {
    let train = Fixture::from_fixture("skill-hardening");
    let store = Store::open(&train.home).unwrap();
    let skill = seed(&train, &store, FixtureKind::SkillHardening).await;
    let cfg = config();
    let engine = CurriculumExecutor {
        store: &store,
        config: &cfg,
    };
    for budget in [4, 3] {
        let c = engine
            .plan(
                CurriculumTarget::Skill(skill.id.clone()),
                "hardening",
                &cfg.curriculum.budget(budget).unwrap(),
            )
            .unwrap();
        let c = engine.run(&c.id, &Cancellation::default()).await.unwrap();
        assert_eq!(c.status, CurriculumStatus::Completed);
    }
    let package = skill_package(&store, &skill.name, "hardening", &cfg.curriculum).unwrap();
    assert_eq!(package.maturity, SkillMaturity::Hardened);
    let heldout = Fixture::from_fixture("skill-hardening-transfer");
    let conditions = [
        PerturbationParameters::EnvironmentVariable {
            key: "HK_TOKEN_STATE".into(),
            value: "EXPIRED_HELD_OUT_TOKEN".into(),
        },
        PerturbationParameters::FileMutation {
            path: "generation".into(),
            content: "9\n".into(),
        },
    ];
    let mut successes = [0; 3];
    let mut repeated = [0; 3];
    let mut latencies = vec![];
    let mut recovery_success = 0;
    let mut evidence = vec![];
    for p in conditions {
        let options = RunResilienceOptions {
            fixture: Some(FixtureKind::SkillHardeningTransfer),
            perturbations: vec![Perturbation::new(p)],
            ..Default::default()
        };
        let baseline = run_with_resilience(
            &store,
            request(&heldout, FixtureKind::SkillHardeningTransfer),
            &RunLearningOptions::default(),
            &options,
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(baseline.experience.outcome, Outcome::Failure);
        let recovery = package
            .recoveries
            .iter()
            .map(|id| store.recovery(id).unwrap())
            .find(|r| {
                baseline
                    .experience
                    .failure_signatures
                    .iter()
                    .any(|s| s.signature == r.failure_signature.signature)
                    && matches!(
                        r.status,
                        RecoveryStatus::Supported | RecoveryStatus::Validated
                    )
            })
            .expect("Observed failure must match a tested package recovery");
        let signature = &recovery.failure_signature.signature;
        assert!(recovery.context.matches(&baseline.experience.context));
        repeated[0] += 1;
        evidence.push(baseline.experience.id.clone());
        // A fixture controller explicitly tries the recorded Candidate Lesson preference.
        // It receives only Lesson advice, not restoration steps or a recovery procedure.
        let lesson = package
            .lessons
            .iter()
            .map(|id| store.lesson(id).unwrap())
            .find(|l| l.claim.contains(signature))
            .unwrap();
        let prefer = lesson.prefer.as_ref().unwrap().shell_script().unwrap();
        let mut lesson_request = request(&heldout, FixtureKind::SkillHardeningTransfer);
        lesson_request.command = CommandSpec::shell(
            &format!("/bin/sh ./operation.sh || {prefer}"),
            EnvironmentMode::Controlled,
        );
        let lesson_only = run_with_resilience(
            &store,
            lesson_request,
            &RunLearningOptions::default(),
            &RunResilienceOptions {
                fixture: None,
                ..options.clone()
            },
            &Cancellation::default(),
        )
        .await
        .unwrap();
        if lesson_only.experience.outcome == Outcome::Success {
            successes[1] += 1;
        } else {
            repeated[1] += 1;
        }
        evidence.push(lesson_only.experience.id);
        let full = run_with_resilience(
            &store,
            request(&heldout, FixtureKind::SkillHardeningTransfer),
            &RunLearningOptions::default(),
            &RunResilienceOptions {
                recovery: Some(recovery),
                ..options
            },
            &Cancellation::default(),
        )
        .await
        .unwrap();
        let attempt = full
            .experience
            .resilience
            .as_ref()
            .unwrap()
            .recovery_attempt
            .as_ref()
            .unwrap();
        assert!(attempt.reproduced_failure && attempt.attempted && attempt.succeeded);
        recovery_success += 1;
        latencies.push(attempt.time_to_recovery_ms);
        if full.experience.outcome == Outcome::Success {
            successes[2] += 1;
        } else {
            repeated[2] += 1;
        }
        evidence.push(full.experience.id);
    }
    assert_eq!(successes, [0, 1, 2]);
    assert_eq!(repeated, [2, 1, 0]);
    assert_eq!(recovery_success, 2);
    assert!(successes[2] > successes[0]);
    let metrics = serde_json::json!({"cases":2,"success_rate":{"none":0.0,"lessons_only":0.5,"full_package":1.0},"resilience_gain":1.0,"repeated_failure_rate":{"none":1.0,"lessons_only":0.5,"full_package":0.0},"recovery_success_rate":{"none":null,"lessons_only":null,"full_package":1.0},"time_to_recovery_ms":{"none":null,"lessons_only":null,"full_package":latencies},"experience_ids":evidence,"scope":"Two discrete held-out local fixture cases; not continuous or production generalization"});
    println!(
        "HELD_OUT_BENCHMARK {}",
        serde_json::to_string(&metrics).unwrap()
    );
    train.assert_source_unchanged();
    heldout.assert_source_unchanged();
    assert!(
        store
            .realities()
            .unwrap()
            .iter()
            .all(|r| r.status == RealityStatus::Discarded)
    );
}
