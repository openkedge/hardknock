// SPDX-License-Identifier: Apache-2.0
mod support;
use chrono::{Duration, Utc};
use hardknock::{
    bridge::{Bridge, cache::ExperienceHotCache, config::Config, protocol},
    cancellation::Cancellation,
    core::*,
    development::*,
    dojo::capture_state,
    experience::{Experience, ExperienceContext, Outcome},
    lesson::{ActionPattern, HeuristicConfidence, Lesson, LessonStatus},
    reflection::{ManualReflection, ReflectionProvider},
    retrieval::{
        DeterministicRetriever, LessonRetriever, QueryContext, RetrievalOptions, freshness_score,
    },
    store::{LessonStore, Store},
};
use serde_json::{Value, json};
use std::{fs, time::Instant};
use support::{Fixture, git};

fn train(f: &Fixture) -> Value {
    f.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "resolve dependencies",
        ],
        0,
    )
}
fn build(store: &Store, subject: &ExperienceSubject, window: ProfileWindow) -> ExperienceProfile {
    EvidenceProfileBuilder {
        store,
        config: &DevelopmentConfig::default(),
        now: Utc::now(),
        context: None,
    }
    .build(subject, window)
    .unwrap()
}
fn subject() -> ExperienceSubject {
    ExperienceSubject::Agent(AgentSubject {
        agent_kind: "test-agent".into(),
        agent_version: None,
        model: None,
        configuration: None,
        profile_scope: ProfileScope::LocalStore,
    })
}
fn context(f: &Fixture) -> ExperienceContext {
    let state = capture_state(&f.repo).unwrap();
    ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled).unwrap()
}

#[test]
fn empty_profiles_and_missing_metrics_are_unknown_not_zero() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let p = build(&store, &subject(), ProfileWindow::AllTime);
    assert_eq!(p.experience_count, 0);
    for k in DevelopmentMetricKind::ALL {
        assert_eq!(p.metrics.metric(k).value, None);
        assert_eq!(p.metrics.metric(k).sample_count, 0);
    }
    assert_eq!(
        f.cli(&["profile", "show", "--agent", "test-agent"], 0)["result"]["profile"]["metrics"]["task_success_rate"]
            ["value"],
        Value::Null
    );
    assert_eq!(
        f.cli(&["growth", "--agent", "test-agent"], 0)["result"]["status"],
        "insufficient_evidence"
    );
    assert!(
        EvidenceProfileBuilder {
            store: &store,
            config: &DevelopmentConfig::default(),
            now: Utc::now(),
            context: None
        }
        .build(&subject(), ProfileWindow::LastDays(0))
        .is_err()
    );
    assert!(
        EvidenceProfileBuilder {
            store: &store,
            config: &DevelopmentConfig::default(),
            now: Utc::now(),
            context: None
        }
        .build(
            &ExperienceSubject::OrganizationScope("not implemented".into()),
            ProfileWindow::AllTime
        )
        .is_err()
    );
    assert_eq!(
        f.cli(&["doctor"], 0)["result"]["database"]["integrity"],
        "ok"
    );
}

#[test]
fn profiles_rebuild_from_canonical_evidence_and_snapshots_stay_immutable() {
    let f = Fixture::pnpm();
    let r = train(&f);
    let store = Store::open(&f.home).unwrap();
    let subject = ExperienceSubject::Repository(f.repo.canonicalize().unwrap());
    let p = build(&store, &subject, ProfileWindow::AllTime);
    assert_eq!(p.experience_count, 4);
    assert_eq!(p.task_count, 2);
    assert_eq!(p.metrics.task_success_rate.value, Some(0.5));
    assert_eq!(p.metrics.experiment_success_rate.sample_count, 1);
    assert_eq!(
        build(&store, &self::subject(), ProfileWindow::AllTime)
            .metrics
            .experiment_success_rate
            .sample_count,
        1
    );
    let last = build(&store, &subject, ProfileWindow::LastExperiences(1));
    assert_eq!(last.task_count, 1);
    assert_eq!(last.metrics.task_success_rate.value, Some(1.0));
    let future = build(
        &store,
        &subject,
        ProfileWindow::Since(Utc::now() + Duration::days(1)),
    );
    assert_eq!(future.metrics.task_success_rate.value, None);
    let s = store.save_profile_snapshot(&p).unwrap();
    assert_eq!(f.cli(&["doctor"], 0)["result"]["snapshots"], 1);
    let before = serde_json::to_value(&s).unwrap();
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute("UPDATE profile_snapshots SET data='{}'", [])
            .is_err()
    );
    assert!(db.execute("DELETE FROM profile_snapshots", []).is_err());
    assert!(db.execute("DELETE FROM snapshot_evidence", []).is_err());
    db.execute("DELETE FROM experience_profiles WHERE id NOT IN (SELECT profile_id FROM profile_snapshots)",[]).unwrap();
    let rebuilt = build(&store, &subject, ProfileWindow::AllTime);
    assert_eq!(
        serde_json::to_value(&p.metrics).unwrap(),
        serde_json::to_value(&rebuilt.metrics).unwrap()
    );
    assert_eq!(p.policy_hash, rebuilt.policy_hash);
    store
        .retire_lesson(
            &r["lesson"]["id"].as_str().unwrap().parse().unwrap(),
            Some("archive".into()),
        )
        .unwrap();
    assert_eq!(
        before,
        serde_json::to_value(store.profile_snapshot(&s.id).unwrap()).unwrap()
    );
    let history = f.cli(
        &["lesson", "history", r["lesson"]["id"].as_str().unwrap()],
        0,
    );
    assert!(history["result"]["revisions"].as_array().unwrap().len() >= 3);
    let output = f.temp.path().join("profile.json");
    f.cli(
        &["profile", "export", "--output", output.to_str().unwrap()],
        0,
    );
    let export = fs::read_to_string(&output).unwrap();
    assert!(!export.contains("stdout"));
    assert!(!export.contains("agent-script.sh"));
    assert!(
        !f.command()
            .args(["profile", "export", "--output", output.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    f.assert_source_unchanged();
}

#[test]
fn episode_windows_detect_regressions_without_starting_work() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let subject = ExperienceSubject::Repository(f.repo.canonicalize().unwrap());
    let cfg = DevelopmentConfig {
        min_trend_samples: 2,
        ..Default::default()
    };
    let a = start_episode(&store, subject.clone(), "successes", &cfg).unwrap();
    for _ in 0..2 {
        f.cli(
            &[
                "run",
                "--script",
                "true",
                "--check",
                "true",
                "--no-experience",
                "task",
            ],
            0,
        );
    }
    let a = finish_episode(&store, &a.id, &cfg).unwrap();
    let b = start_episode(&store, subject.clone(), "failures", &cfg).unwrap();
    for _ in 0..2 {
        f.cli(
            &[
                "run",
                "--script",
                "false",
                "--check",
                "false",
                "--no-experience",
                "task",
            ],
            1,
        );
    }
    let b = finish_episode(&store, &b.id, &cfg).unwrap();
    let before = store
        .profile_snapshot(a.profile_after.as_ref().unwrap())
        .unwrap();
    let after = store
        .profile_snapshot(b.profile_after.as_ref().unwrap())
        .unwrap();
    let r = compare_snapshots(&before, &after, &cfg);
    assert_eq!(r.comparisons[0].trend, MetricTrend::Regressing);
    assert_eq!(r.regressions.len(), 1);
    assert!(!r.regressions[0].auto_run);
    store.save_regressions(&r).unwrap();
    assert_eq!(store.development_observations().unwrap().len(), 4);
    assert_eq!(a.experiences.len(), 2);
    assert_eq!(b.experiences.len(), 2);
    assert_eq!(
        serde_json::to_value(&b).unwrap(),
        serde_json::to_value(finish_episode(&store, &b.id, &cfg).unwrap()).unwrap()
    );
    let mut changed = after.clone();
    changed.policy_hash.push('x');
    assert_eq!(
        compare_snapshots(&before, &changed, &cfg).comparisons[0].trend,
        MetricTrend::InsufficientEvidence
    );
    changed = after.clone();
    changed.evidence_ids.push(before.evidence_ids[0].clone());
    assert_eq!(
        compare_snapshots(&before, &changed, &cfg).comparisons[0].trend,
        MetricTrend::InsufficientEvidence
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute("UPDATE development_episodes SET data='{}'", [])
            .is_err()
    );
    assert_eq!(
        f.cli(&["timeline", "--limit", "2"], 0)["result"]["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn trend_thresholds_sample_gates_and_lower_is_better_are_explicit() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let p = build(&store, &subject(), ProfileWindow::AllTime);
    let mut a = snapshot(&p);
    let mut b = a.clone();
    b.id = ProfileSnapshotId::new();
    b.captured_at += Duration::seconds(1);
    let cfg = DevelopmentConfig::default();
    a.metrics.repeated_mistake_rate = MetricValue::ratio(4, 10, &ProfileWindow::AllTime, "test");
    b.metrics.repeated_mistake_rate = MetricValue::ratio(1, 10, &ProfileWindow::AllTime, "test");
    assert_eq!(
        compare_metric(&a, &b, DevelopmentMetricKind::RepeatedMistakeRate, &cfg).trend,
        MetricTrend::Improving
    );
    b.metrics.repeated_mistake_rate = MetricValue::ratio(4, 10, &ProfileWindow::AllTime, "test");
    assert_eq!(
        compare_metric(&a, &b, DevelopmentMetricKind::RepeatedMistakeRate, &cfg).trend,
        MetricTrend::Stable
    );
    b.metrics.repeated_mistake_rate = MetricValue::ratio(1, 2, &ProfileWindow::AllTime, "test");
    assert_eq!(
        compare_metric(&a, &b, DevelopmentMetricKind::RepeatedMistakeRate, &cfg).trend,
        MetricTrend::InsufficientEvidence
    );
    assert_eq!(benchmark::median(&[2, 4]), Some(3));
    assert_eq!(benchmark::median(&[]), None);
}

#[test]
fn version_eight_migration_backfills_skill_lineage_without_rewriting_evidence() {
    let f = Fixture::new();
    let run = f.cli(
        &[
            "run",
            "--script",
            "true",
            "--check",
            "true",
            "--no-experience",
            "legacy",
        ],
        0,
    );
    let store = Store::open(&f.home).unwrap();
    let id: ExperienceId = run["experience"]["id"].as_str().unwrap().parse().unwrap();
    let skill = store.register_skill("legacy", &id).unwrap();
    let legacy = f.temp.path().join("legacy-data");
    fs::create_dir(&legacy).unwrap();
    let db = rusqlite::Connection::open(legacy.join("hardknock.db")).unwrap();
    db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO schema_migrations(version) VALUES(1),(2),(3),(4),(5),(6),(7),(8);").unwrap();
    for sql in [
        include_str!("../migrations/001_substrate.sql"),
        include_str!("../migrations/002_experiences.sql"),
        include_str!("../migrations/003_learning.sql"),
        include_str!("../migrations/004_transfer.sql"),
        include_str!("../migrations/005_resilience.sql"),
        include_str!("../migrations/006_bridge.sql"),
        include_str!("../migrations/007_agent_experiments.sql"),
        include_str!("../migrations/008_curriculum.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    db.execute(
        "ATTACH DATABASE ?1 AS source",
        [f.home.join("hardknock.db").to_str().unwrap()],
    )
    .unwrap();
    for table in [
        "realities",
        "executions",
        "evaluations",
        "experiences",
        "experience_artifacts",
        "skills",
    ] {
        db.execute(
            &format!("INSERT INTO {table} SELECT * FROM source.{table}"),
            [],
        )
        .unwrap();
    }
    let before: String = db
        .query_row("SELECT data FROM experiences", [], |r| r.get(0))
        .unwrap();
    db.execute_batch("DETACH DATABASE source").unwrap();
    drop(db);
    let migrated = Store::open(&legacy).unwrap();
    assert_eq!(
        migrated.skill_revisions(&skill.id).unwrap()[0].source_experience,
        id
    );
    let db = rusqlite::Connection::open(legacy.join("hardknock.db")).unwrap();
    let after: String = db
        .query_row("SELECT data FROM experiences", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        build(
            &migrated,
            &ExperienceSubject::SharedLocal,
            ProfileWindow::AllTime
        )
        .metrics
        .task_success_rate
        .value,
        Some(1.0)
    );
}

#[test]
fn changing_policies_changes_rebuilt_hash_but_not_recorded_snapshots() {
    let f = Fixture::pnpm();
    train(&f);
    let store = Store::open(&f.home).unwrap();
    let p = build(&store, &subject(), ProfileWindow::AllTime);
    let s = store.save_profile_snapshot(&p).unwrap();
    let mut config = Config::default();
    config.curriculum.min_hardening_dimensions += 1;
    fs::write(
        f.home.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let after = build(&store, &subject(), ProfileWindow::AllTime);
    assert_ne!(p.policy_hash, after.policy_hash);
    assert_eq!(
        store.profile_snapshot(&s.id).unwrap().policy_hash,
        p.policy_hash
    );
    let next = store.save_profile_snapshot(&after).unwrap();
    assert_eq!(
        compare_snapshots(&s, &next, &config.development).comparisons[0].trend,
        MetricTrend::InsufficientEvidence
    );
    let compared = f.cli(
        &[
            "profile",
            "compare",
            "--from",
            s.id.to_string().as_str(),
            "--to",
            next.id.to_string().as_str(),
        ],
        0,
    );
    assert_eq!(
        compared["result"]["growth"]["comparisons"][0]["trend"],
        "insufficient_evidence"
    );
}

#[test]
fn freshness_requires_context_change_to_mark_old_evidence_stale() {
    let f = Fixture::pnpm();
    let r = train(&f);
    let store = Store::open(&f.home).unwrap();
    let lesson = store
        .lesson(&r["lesson"]["id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    let bases = store
        .lesson_freshness_bases(std::slice::from_ref(&lesson))
        .unwrap();
    let basis = &bases[&lesson.id];
    let mut q = QueryContext::new(
        &basis.context,
        "dependencies",
        vec![ActionPattern::shell("npm install")],
    );
    let cfg = DevelopmentConfig::default();
    let later = Utc::now() + Duration::days(121);
    assert_eq!(
        assess_freshness(basis, &q, None, later, &cfg).state,
        EvidenceState::Aging
    );
    let score = freshness_score(&lesson, &q, Some(basis), &cfg, later).0;
    q.environment.fingerprint = "changed-runtime".into();
    let stale = assess_freshness(basis, &q, None, later, &cfg);
    assert_eq!(stale.state, EvidenceState::Stale);
    assert!(f64::from(freshness_score(&lesson, &q, Some(basis), &cfg, later).0) < f64::from(score));
    let p = EvidenceProfileBuilder {
        store: &store,
        config: &cfg,
        now: later,
        context: Some(q.clone()),
    }
    .build(&ExperienceSubject::SharedLocal, ProfileWindow::AllTime)
    .unwrap();
    assert!(p.freshness.stale > 0);
    let report = maintain(&store, &p, &q.experience_context(), true).unwrap();
    assert!(!report.auto_run);
    assert!(!report.revalidation.is_empty());
    assert_eq!(store.development_observations().unwrap().len(), 4);
    assert_eq!(
        report.revalidation.len(),
        maintain(&store, &p, &q.experience_context(), true)
            .unwrap()
            .revalidation
            .len()
    );
    let mut refreshed = basis.clone();
    refreshed.last_supported_at = later;
    refreshed.context = q.experience_context();
    assert_eq!(
        assess_freshness(&refreshed, &q, None, later, &cfg).state,
        EvidenceState::Fresh
    );
    refreshed.contradicted = true;
    assert_eq!(
        assess_freshness(&refreshed, &q, None, later, &cfg).multiplier,
        0.0
    );
}

#[tokio::test]
async fn explicit_revalidation_records_contradiction_and_retains_old_evidence() {
    let f = Fixture::pnpm();
    let r = train(&f);
    let store = Store::open(&f.home).unwrap();
    let id: LessonId = r["lesson"]["id"].as_str().unwrap().parse().unwrap();
    let old = serde_json::to_value(
        store
            .experience(&r["experience"]["id"].as_str().unwrap().parse().unwrap())
            .unwrap(),
    )
    .unwrap();
    for file in ["agent-script.sh", "test.sh", "hardknock-fixture.json"] {
        fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/pnpm-workspace-contradiction")
                .join(file),
            f.repo.join(file),
        )
        .unwrap();
    }
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "environment changes"]);
    let c = context(&f);
    let cfg = DevelopmentConfig::default();
    let query = QueryContext::new(&c, "", vec![ActionPattern::shell("npm install")]);
    let p = EvidenceProfileBuilder {
        store: &store,
        config: &cfg,
        now: Utc::now(),
        context: Some(query.clone()),
    }
    .build(&ExperienceSubject::SharedLocal, ProfileWindow::AllTime)
    .unwrap();
    let queue = maintain(&store, &p, &c, true).unwrap();
    assert_eq!(queue.revalidation.len(), 1);
    let completed = run_revalidation(&store, &queue.revalidation[0], &Cancellation::default())
        .await
        .unwrap();
    assert!(completed.experiment_id.is_some());
    assert_eq!(
        store.lesson(&id).unwrap().status,
        LessonStatus::Contradicted
    );
    assert!(
        DeterministicRetriever {
            store: &store,
            options: RetrievalOptions::default()
        }
        .retrieve(&query)
        .unwrap()
        .matches
        .is_empty()
    );
    assert!(
        ExperienceHotCache::load(&store)
            .unwrap()
            .retrieve(&c, "", vec![ActionPattern::shell("npm install")])
            .is_empty()
    );
    let raw = serde_json::to_value(
        store
            .experience(&r["experience"]["id"].as_str().unwrap().parse().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(old, raw);
    assert!(store.lesson_versions(&id).unwrap().len() >= 4);
    assert_eq!(
        run_revalidation(&store, &completed, &Cancellation::default())
            .await
            .unwrap()
            .experiment_id,
        completed.experiment_id
    );
}

#[test]
fn skill_and_package_revisions_pin_procedures_and_never_overwrite_history() {
    let f = Fixture::pnpm();
    let a = f.cli(
        &[
            "run",
            "--script",
            "./agent-script.sh alternative",
            "--check",
            "./test.sh",
            "--no-experience",
            "procedure",
        ],
        0,
    );
    let store = Store::open(&f.home).unwrap();
    let skill = store
        .register_skill(
            "procedure",
            &a["experience"]["id"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
    let package = f.cli(&["skill", "package", "procedure"], 0);
    assert_eq!(package["result"]["kind"], "package");
    let revisions = store
        .package_revisions(&skill.id, "resilience-basic")
        .unwrap();
    assert_eq!(revisions.len(), 1);
    let mut same = revisions[0].package.clone();
    same.generated_at += Duration::seconds(1);
    assert_eq!(store.save_package_revision(&same).unwrap().revision, 1);
    let b = f.cli(
        &[
            "run",
            "--script",
            "./agent-script.sh alternative; printf revised",
            "--check",
            "./test.sh",
            "--no-experience",
            "procedure",
        ],
        0,
    );
    let source: ExperienceId = b["experience"]["id"].as_str().unwrap().parse().unwrap();
    let r = store.revise_skill("procedure", &source).unwrap();
    assert_eq!(r.revision, 2);
    assert_eq!(r.parent_revision, Some(1));
    assert_eq!(
        store.skill_revisions(&skill.id).unwrap()[0]
            .source_experience
            .to_string(),
        a["experience"]["id"].as_str().unwrap()
    );
    f.cli(&["skill", "package", "procedure"], 0);
    let revisions = store
        .package_revisions(&skill.id, "resilience-basic")
        .unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].skill_revision, 1);
    assert_eq!(revisions[1].skill_revision, 2);
    let diff = f.cli(
        &[
            "skill",
            "package",
            "diff",
            "procedure",
            "--from",
            "1",
            "--to",
            "2",
        ],
        0,
    );
    assert_eq!(diff["result"]["skill_revision"], json!([1, 2]));
    assert_eq!(
        f.cli(&["skill", "history", "procedure"], 0)["result"]["revisions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let output = f.temp.path().join("package.json");
    f.cli(
        &[
            "skill",
            "package",
            "export",
            "procedure",
            "--revision",
            "1",
            "--output",
            output.to_str().unwrap(),
        ],
        0,
    );
    let exported: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(exported["trust"], "untrusted_when_shared");
    assert_eq!(
        exported["revision"]["evidence_hash"],
        revisions[0].evidence_hash
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(db.execute("DELETE FROM skill_revisions", []).is_err());
    assert!(
        db.execute("UPDATE experience_package_revisions SET revision=55", [])
            .is_err()
    );
}

#[test]
fn profile_scope_and_promotion_never_turn_local_evidence_into_global_authority() {
    let f = Fixture::pnpm();
    let r = train(&f);
    let store = Store::open(&f.home).unwrap();
    let p = build(
        &store,
        &ExperienceSubject::Repository("/unrelated".into()),
        ProfileWindow::AllTime,
    );
    assert_eq!(p.task_count, 0);
    assert!(p.lessons.is_empty());
    let family = store
        .register_task_family(
            "fixture-family",
            vec![r["experience"]["id"].as_str().unwrap().parse().unwrap()],
        )
        .unwrap();
    assert!(
        build(
            &store,
            &ExperienceSubject::TaskFamily(family.id),
            ProfileWindow::AllTime
        )
        .task_count
            > 0
    );
    let lesson = store
        .lesson(&r["lesson"]["id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    let decision = ConservativePromotion.evaluate(&lesson);
    assert!(!decision.eligible_for_review);
    assert!(!decision.auto_promote);
    let mut scoped = lesson.clone();
    scoped.context_match.repository = Some(f.repo.clone());
    scoped.status = LessonStatus::Validated;
    assert!(!ConservativePromotion.evaluate(&scoped).eligible_for_review);
}

#[test]
fn bridge_development_context_is_optional_bounded_and_does_not_start_work() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut cfg = Config::default();
    cfg.development.bridge_context = true;
    fs::write(f.home.join("config.toml"), toml::to_string(&cfg).unwrap()).unwrap();
    let (bridge, worker) = Bridge::open(&f.home).unwrap();
    let start = protocol::SessionStarted {
        session_id: "development".into(),
        agent: protocol::AgentIdentity::new("test-adapter"),
        cwd: f.repo.to_string_lossy().into(),
        repository: None,
        task: Some("OPENAI_API_KEY=never-export-this".into()),
        environment: Default::default(),
    };
    let response = bridge
        .handle(protocol::AgentEvent::SessionStarted(start.clone()))
        .unwrap();
    assert_eq!(response["development_context"]["auto_run"], false);
    let resumed = bridge
        .handle(protocol::AgentEvent::SessionStarted(start))
        .unwrap();
    assert_eq!(resumed["development_context"]["auto_run"], false);
    assert!(
        !resumed["development_context"]["known_unknowns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(bytes.len() <= cfg.bridge.max_context_bytes);
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("never-export-this")
    );
    assert!(store.development_observations().unwrap().is_empty());
    bridge.flush().unwrap();
    drop(bridge);
    worker.join().unwrap();
}

#[tokio::test]
async fn longitudinal_benchmark_uses_recorded_tasks_and_preserves_agent_origins() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let result = benchmark::run(&store, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(result.status, "completed");
    assert_eq!(result.tasks.len(), 90);
    for (arm, n) in [
        ("stateless", 3),
        ("reflection_memory", 9),
        ("hardknock", 23),
    ] {
        let arm_home = result.metadata["arms"][arm]["home"].as_str().unwrap();
        let arm_store = Store::open(std::path::Path::new(arm_home)).unwrap();
        let tasks: Vec<_> = result.tasks.iter().filter(|t| t.arm == arm).collect();
        assert_eq!(tasks.iter().filter(|t| t.success).count(), n);
        for t in tasks {
            let e = arm_store.experience(&t.experience_id).unwrap();
            assert_eq!(e.outcome == Outcome::Success, t.success);
            assert!(!e.evaluation.spec.checks.is_empty());
        }
        if arm != "hardknock" {
            assert!(arm_store.all_lessons().unwrap().is_empty());
        }
        assert_eq!(
            arm_store.database_health().unwrap()["foreign_key_violations"],
            false
        );
        assert!(
            arm_store
                .realities()
                .unwrap()
                .iter()
                .all(|r| r.status == RealityStatus::Discarded)
        );
    }
    assert_eq!(
        result.metrics["hardknock"]["aggregate"]["recovery_success_rate"]["numerator"],
        12
    );
    assert_eq!(result.portability["metric"]["numerator"], 3);
    assert_eq!(result.portability["independent_replication"], false);
    assert_eq!(
        result.portability["origin_agent"]["version"],
        "fixture-agent-a-v1"
    );
    assert_eq!(
        result.portability["new_agent"]["version"],
        "fixture-agent-b-v1"
    );
    assert_eq!(result.stale_rule["lesson_after"], "contradicted");
    assert_eq!(
        result.stale_rule["hardknock"]["task_success_rate"]["numerator"],
        2
    );
    assert_eq!(result.snapshots.len(), 5);
    let snapshots: Vec<_> = result
        .snapshots
        .iter()
        .map(|id| store.profile_snapshot(id).unwrap())
        .collect();
    assert_eq!(
        compare_snapshots(&snapshots[0], &snapshots[1], &DevelopmentConfig::default()).comparisons
            [0]
        .trend,
        MetricTrend::Improving
    );
    assert!(
        benchmark::run(&store, &Cancellation::default())
            .await
            .is_err()
    );
    assert!(
        store
            .home
            .join("artifacts")
            .join(format!("{}.json", result.id))
            .is_file()
    );
    let cached = build(&store, &subject(), ProfileWindow::AllTime);
    assert_eq!(cached.metrics.task_success_rate.numerator, Some(23));
    assert!(cached.metrics.experiment_success_rate.sample_count >= 2);
}

#[test]
fn ten_thousand_observation_projection_retrieval_timeline_and_hot_lookup_scale() {
    // Synthetic load test ONLY: duplicate complete local records with new IDs.
    // No synthetic row is part of the longitudinal benchmark or a claimed real outcome.
    let f = Fixture::pnpm();
    let trained = train(&f);
    let store = Store::open(&f.home).unwrap();
    let success: Experience =
        serde_json::from_value(trained["retries"][0]["experience"].clone()).unwrap();
    let failed: Experience = serde_json::from_value(trained["experience"].clone()).unwrap();
    let mut db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..10000 {
        let template = if i % 2 == 0 { &success } else { &failed };
        let mut e = serde_json::to_value(template).unwrap();
        let id = ExperienceId::new().to_string();
        let execution = ExecutionId::new().to_string();
        let evaluation = EvaluationId::new().to_string();
        e["id"] = json!(id);
        e["execution_id"] = json!(execution);
        e["evaluation"]["id"] = json!(evaluation);
        e["created_at"] = json!((Utc::now() - Duration::seconds(10001 - i)).to_rfc3339());
        e["tags"] = json!(["synthetic-scale-test-only"]);
        e["lesson_applications"] = json!([]);
        e["relations"] = json!([]);
        e["repeated_mistakes"] = json!([]);
        tx.execute("INSERT INTO executions SELECT ?1,reality_id,created_at,json_set(data,'$.id',?1) FROM executions WHERE id=?2",rusqlite::params![execution,template.execution_id.to_string()]).unwrap();
        tx.execute("INSERT INTO evaluations SELECT ?1,?2,json_set(data,'$.id',?1) FROM evaluations WHERE id=?3",rusqlite::params![evaluation,execution,template.evaluation.id.to_string()]).unwrap();
        tx.execute("INSERT INTO experiences(id,created_at,reality_id,execution_id,evaluation_id,outcome,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",rusqlite::params![id,e["created_at"].as_str().unwrap(),template.reality_id.to_string(),execution,evaluation,serde_json::to_string(&template.outcome).unwrap(),serde_json::to_string(&e).unwrap()]).unwrap();
    }
    tx.commit().unwrap();
    for i in 0..1000 {
        let h = ManualReflection {
            claim: format!("Synthetic scoped hypothesis {i}"),
            avoid: "false".into(),
            prefer: "true".into(),
        }
        .reflect(&failed)
        .unwrap()
        .remove(0);
        store.insert_hypothesis(&h).unwrap();
        LessonStore::insert(&store, &Lesson::candidate(&h, &HeuristicConfidence)).unwrap();
    }
    for i in 0..100 {
        store
            .register_skill(&format!("synthetic-skill-{i}"), &success.id)
            .unwrap();
    }
    let started = Instant::now();
    let p = build(
        &store,
        &ExperienceSubject::SharedLocal,
        ProfileWindow::AllTime,
    );
    let profile_ms = started.elapsed().as_millis();
    assert_eq!(p.experience_count, 10004);
    assert_eq!(p.lessons.len(), 1001);
    assert_eq!(p.skills.len(), 100);
    let q = QueryContext::new(
        &failed.context,
        "dependencies",
        vec![ActionPattern::shell("npm install")],
    );
    let started = Instant::now();
    let report = DeterministicRetriever {
        store: &store,
        options: RetrievalOptions::default(),
    }
    .retrieve(&q)
    .unwrap();
    let retrieval_ms = started.elapsed().as_millis();
    assert_eq!(report.matches.len(), 1);
    let started = Instant::now();
    assert_eq!(store.development_timeline(200).unwrap().len(), 200);
    let timeline_ms = started.elapsed().as_millis();
    let hot = ExperienceHotCache::load(&store).unwrap();
    let proposed = protocol::ActionProposed {
        hardknock_session_id: "load-test".into(),
        action_id: "action".into(),
        action: protocol::NormalizedAction::Shell {
            command: "npm install".into(),
            cwd: failed.context.environment.cwd.to_string_lossy().into(),
        },
        context: Default::default(),
    };
    let started = Instant::now();
    for _ in 0..1000 {
        hot.evaluate(&failed.context, &proposed, 0, &Config::default().bridge);
    }
    let hot_ms = started.elapsed().as_millis();
    let snap = store.save_profile_snapshot(&p).unwrap();
    let started = Instant::now();
    assert_eq!(store.profile_history(&p.id).unwrap()[0].id, snap.id);
    let history_ms = started.elapsed().as_millis();
    println!(
        "V06_SCALE profile_ms={profile_ms} retrieval_ms={retrieval_ms} timeline_ms={timeline_ms} hot_1000_ms={hot_ms} history_ms={history_ms}"
    );
    assert!(
        profile_ms < 10000
            && retrieval_ms < 10000
            && timeline_ms < 5000
            && hot_ms < 5000
            && history_ms < 5000,
        "Broad debug-build regression limits exceeded"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}
