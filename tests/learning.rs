// SPDX-License-Identifier: Apache-2.0

mod support;
use hardknock::{
    experience::{Experience, Outcome},
    store::{ExperienceQuery, ExperienceStore, Store, artifact},
};
use std::fs;
use support::Fixture;

fn propose(f: &Fixture, source: &serde_json::Value, avoid: &str, prefer: &str) -> String {
    f.cli(
        &[
            "lesson",
            "propose",
            "--experience",
            source["experience"]["id"].as_str().unwrap(),
            "--claim",
            "Test scoped script replacement",
            "--avoid",
            avoid,
            "--prefer",
            prefer,
        ],
        0,
    )["lesson"]["id"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn controlled_environment_omits_inherited_secrets_and_matches_markers() {
    let f = Fixture::pnpm();
    let output = f
        .command()
        .env("HARDKNOCK_TEST_SECRET", "do-not-record-this-value")
        .args([
            "--json",
            "run",
            "--script",
            "test -z \"${HARDKNOCK_TEST_SECRET:-}\"; printf '%s' \"$HOME\"",
            "--check",
            "test \"$PATH\" = /usr/bin:/bin",
            "inspect environment",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("do-not-record-this-value"));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let context = &response["experience"]["context"];
    assert!(
        context["markers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "pnpm-workspace.yaml")
    );
    assert_eq!(context["environment"]["facts"]["HOME"], "$REALITY");
    assert_eq!(context["environment"]["mode"], "controlled");
    let child_home = fs::read_to_string(
        response["execution"]["action"]["stdout"]["path"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(child_home, response["reality"]["root"]);
    f.assert_source_unchanged();
}

#[test]
fn timed_out_baseline_is_inconclusive_even_when_alternative_passes() {
    let f = Fixture::new();
    let source = f.cli(
        &[
            "run",
            "--script",
            "sleep 3",
            "--timeout-secs",
            "1",
            "--check",
            "test -f ok",
            "task",
        ],
        1,
    );
    let id = propose(&f, &source, "sleep 3", "touch ok");
    let result = f.cli(&["experiment", "run", "--lesson", &id], 3);
    assert_eq!(result["experiment"]["trials"][0]["outcome"], "timed_out");
    assert_eq!(result["experiment"]["trials"][1]["outcome"], "success");
    assert_eq!(result["experiment"]["conclusion"], "inconclusive");
    assert_eq!(result["lesson"]["confidence"], 0.42);
    f.assert_source_unchanged();
}

#[test]
fn concurrent_experiments_preserve_both_evidence_revisions() {
    use std::process::Stdio;
    let f = Fixture::new();
    let source = f.cli(
        &["run", "--script", "true", "--check", "test -f ok", "task"],
        1,
    );
    let id = propose(&f, &source, "true", "touch ok");
    let first = f
        .command()
        .args(["--json", "experiment", "run", "--lesson", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = f
        .command()
        .args(["--json", "experiment", "run", "--lesson", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let store = Store::open(&f.home).unwrap();
    let lesson = store.lesson(&id.parse().unwrap()).unwrap();
    assert_eq!(lesson.version, 3);
    assert_eq!(lesson.evidence.len(), 5);
    assert_eq!(store.lesson_versions(&lesson.id).unwrap().len(), 3);
    assert_eq!(store.experiments().unwrap().len(), 2);
    f.assert_source_unchanged();
}

#[test]
fn shell_action_patterns_do_not_guess_at_semantic_equivalence() {
    use hardknock::lesson::ActionPattern;
    let pattern = ActionPattern::shell(" npm install ");
    assert!(pattern.matches_shell("\nnpm install\t"));
    for script in [
        "npm  install",
        "npm install --force",
        "echo npm install",
        "npm install; true",
    ] {
        assert!(!pattern.matches_shell(script));
    }
    let custom = ActionPattern::Custom {
        kind: "future".into(),
        value: "npm install".into(),
    };
    assert!(!custom.matches_shell("npm install"));
    let json = serde_json::to_string(&custom).unwrap();
    assert_eq!(
        serde_json::from_str::<ActionPattern>(&json).unwrap(),
        custom
    );
}

#[test]
fn v1_database_migrates_without_rewriting_old_execution_json() {
    use hardknock::core::{ArtifactKind, EnvironmentMode};
    let original = Fixture::new();
    let run = original.cli(&["run", "--agent-command", "sh -c '{task}'", "true"], 0);
    let f = Fixture::new();
    fs::create_dir(&f.home).unwrap();
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO schema_migrations(version) VALUES(1);").unwrap();
    db.execute_batch(include_str!("../migrations/001_substrate.sql"))
        .unwrap();
    let reality = &run["reality"];
    db.execute(
        "INSERT INTO realities(id,created_at,data) VALUES(?1,?2,?3)",
        rusqlite::params![
            reality["id"].as_str(),
            reality["created_at"].as_str(),
            serde_json::to_string(reality).unwrap()
        ],
    )
    .unwrap();
    let mut execution = run["execution"].clone();
    execution["action"]["command"]
        .as_object_mut()
        .unwrap()
        .remove("environment");
    execution["action"]["stdout"]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    execution["action"]["stderr"]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    execution["diff"].as_object_mut().unwrap().remove("kind");
    let raw = serde_json::to_string(&execution).unwrap();
    db.execute(
        "INSERT INTO executions(id,reality_id,created_at,data) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            execution["id"].as_str(),
            execution["reality_id"].as_str(),
            execution["action"]["started_at"].as_str(),
            raw
        ],
    )
    .unwrap();
    let store = Store::open(&f.home).unwrap();
    let records = store.executions().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].action.command.environment,
        EnvironmentMode::Inherited
    );
    assert_eq!(records[0].diff.kind, ArtifactKind::Other);
    assert_eq!(
        db.query_row("SELECT data FROM executions", [], |r| r.get::<_, String>(0))
            .unwrap(),
        raw
    );
    assert!(
        ExperienceStore::list(&store, ExperienceQuery::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn v3_migration_preserves_raw_experience_json_and_defaults_new_provenance() {
    let original = Fixture::new();
    let run = original.cli(
        &["run", "--script", "true", "--check", "true", "legacy task"],
        0,
    );
    let legacy = Fixture::new();
    fs::create_dir(&legacy.home).unwrap();
    let db = rusqlite::Connection::open(legacy.home.join("hardknock.db")).unwrap();
    db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO schema_migrations(version) VALUES(1),(2),(3);").unwrap();
    for migration in [
        include_str!("../migrations/001_substrate.sql"),
        include_str!("../migrations/002_experiences.sql"),
        include_str!("../migrations/003_learning.sql"),
    ] {
        db.execute_batch(migration).unwrap();
    }
    db.execute(
        "ATTACH DATABASE ?1 AS source",
        [original.home.join("hardknock.db").to_str().unwrap()],
    )
    .unwrap();
    for table in ["realities", "executions", "evaluations"] {
        db.execute(
            &format!("INSERT INTO {table} SELECT * FROM source.{table}"),
            [],
        )
        .unwrap();
    }
    db.execute("INSERT INTO experiences SELECT id,created_at,reality_id,execution_id,evaluation_id,outcome,json_remove(data,'$.lesson_applications','$.relations','$.repeated_mistakes','$.observed_actions','$.application_report_errors') FROM source.experiences", []).unwrap();
    db.execute(
        "INSERT INTO experience_artifacts SELECT * FROM source.experience_artifacts",
        [],
    )
    .unwrap();
    db.execute("DETACH DATABASE source", []).unwrap();
    let raw: String = db
        .query_row("SELECT data FROM experiences", [], |r| r.get(0))
        .unwrap();
    let store = Store::open(&legacy.home).unwrap();
    let experience = store
        .experience(&run["experience"]["id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert!(experience.lesson_applications.is_empty());
    assert!(experience.relations.is_empty());
    assert!(experience.repeated_mistakes.is_empty());
    assert!(experience.observed_actions.is_empty());
    assert!(experience.application_report_errors.is_empty());
    assert_eq!(
        db.query_row("SELECT data FROM experiences", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        raw
    );
    assert_eq!(
        db.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}

#[test]
fn cancelled_evaluation_records_completed_check_and_skips_remaining_checks() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let ready = f.temp.path().join("check-ready");
    let check = format!("touch '{}'; sleep 30", ready.display());
    let mut child = f
        .command()
        .args([
            "--json",
            "run",
            "--script",
            "true",
            "--check",
            "true",
            "--check",
            &check,
            "--check",
            "echo should-not-run",
            "task",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if !ready.exists() {
        child.kill().unwrap();
        panic!("Check never started");
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(5));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = result["experience"]["evaluation"]["checks"]
        .as_array()
        .unwrap();
    assert_eq!(checks[0]["status"], "passed");
    assert_eq!(checks[1]["status"], "interrupted");
    assert_eq!(checks[2]["status"], "not_run");
    assert!(checks[2]["action"].is_null());
    assert_eq!(result["experience"]["outcome"], "interrupted");
    f.assert_source_unchanged();
}

#[test]
fn opaque_agents_and_mismatched_action_or_context_cannot_be_replayed() {
    use hardknock::{
        experiment::CounterfactualPlan,
        lesson::{ActionPattern, ConfidenceScore},
        store::LessonStore,
    };
    let f = Fixture::new();
    let source = f.cli(
        &[
            "run",
            "--agent-command",
            "sh -c '{task}'",
            "--check",
            "false",
            "true",
        ],
        1,
    );
    let id = propose(&f, &source, "true", "touch ok");
    let output = f
        .command()
        .args(["--json", "experiment", "run", "--lesson", &id])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot guarantee equivalent starting state")
    );
    assert!(
        f.cli(&["experiment", "list"], 0)["experiments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let source = f.cli(&["run", "--script", "true", "--check", "false", "task"], 1);
    let id = propose(&f, &source, "true", "touch ok");
    let store = Store::open(&f.home).unwrap();
    let experience: Experience = serde_json::from_value(source["experience"].clone()).unwrap();
    let original = store.lesson(&id.parse().unwrap()).unwrap();
    let mut lesson = original.clone();
    lesson.avoid = Some(ActionPattern::shell("tru"));
    assert!(CounterfactualPlan::from_lesson(&experience, &lesson).is_err());
    lesson = original.clone();
    lesson
        .context_match
        .required_markers
        .push("not-present".into());
    assert!(CounterfactualPlan::from_lesson(&experience, &lesson).is_err());
    lesson = original.clone();
    lesson.version += 1;
    lesson.rationale = "Reviewed by a human; still a candidate".into();
    lesson.updated_at = chrono::Utc::now();
    LessonStore::update(&store, &lesson).unwrap();
    assert!(LessonStore::update(&store, &lesson).is_err());
    lesson.version += 1;
    lesson.status = hardknock::lesson::LessonStatus::Validated;
    assert!(LessonStore::update(&store, &lesson).is_err());
    for score in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert!(ConfidenceScore::try_from(score).is_err());
    }
    assert!(serde_json::from_str::<ConfidenceScore>("1.5").is_err());
    f.assert_source_unchanged();
}

#[tokio::test]
async fn fingerprint_mismatch_and_dirty_snapshot_abort_before_execution() {
    use hardknock::{
        cancellation::Cancellation,
        core::{AgentIdentity, CommandSpec, EnvironmentMode},
        dojo::{GitRealityProvider, RealityProvider, capture_state},
        evaluation::EvaluationSpec,
        workflow::{RunRequest, run_once},
    };
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let state = capture_state(&f.repo).unwrap();
    let provider = GitRealityProvider::new(&store);
    let mut reality = provider.create(&state).unwrap();
    fs::write(reality.root.join("tracked.txt"), "dirty").unwrap();
    assert!(provider.verify_start(&reality).is_err());
    provider.discard(&mut reality).unwrap();
    let result = run_once(
        &store,
        RunRequest {
            state,
            goal: "no spawn".into(),
            agent: AgentIdentity {
                kind: "test".into(),
                executable: "/bin/sh".into(),
                version: None,
                model: None,
            },
            command: CommandSpec::shell("touch should-not-run", EnvironmentMode::Controlled),
            evaluation: EvaluationSpec {
                checks: vec!["true".into()],
            },
            timeout_secs: 5,
            keep: false,
            replay: None,
            perturbations: vec![],
            expected_fingerprint: Some("changed-environment".into()),
        },
        &Cancellation::default(),
    )
    .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("fingerprint mismatch")
    );
    assert!(store.executions().unwrap().is_empty());
    assert!(
        fs::read_dir(f.home.join("realities"))
            .unwrap()
            .next()
            .is_none()
    );
    f.assert_source_unchanged();
}

#[test]
fn interrupted_trial_retains_partial_evidence_and_skips_alternative() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let gate = f.temp.path().join("gate");
    let ready = f.temp.path().join("ready");
    let script = format!(
        "if [ -f '{}' ]; then touch '{}'; sleep 30; fi",
        gate.display(),
        ready.display()
    );
    let source = f.cli(&["run", "--script", &script, "--check", "false", "task"], 1);
    let id = propose(&f, &source, &script, "touch alternative");
    fs::write(&gate, "start waiting").unwrap();
    let mut child = f
        .command()
        .args(["--json", "experiment", "run", "--lesson", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if !ready.exists() {
        child.kill().unwrap();
        panic!("Trial never reached interrupt point");
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["experiment"]["status"], "interrupted");
    assert_eq!(response["experiment"]["conclusion"], "inconclusive");
    assert_eq!(
        response["experiment"]["trials"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        response["experiment"]["trials"][0]["outcome"],
        "interrupted"
    );
    assert_eq!(response["lesson"]["status"], "candidate");
    assert_eq!(response["lesson"]["version"], 1);
    assert_eq!(
        f.cli(&["experience", "list"], 0)["experiences"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    f.assert_source_unchanged();
}

#[test]
fn capture_failure_preserves_worktree_and_finished_baseline_evidence() {
    use hardknock::{
        core::RealityStatus,
        dojo::{GitRealityProvider, RealityProvider},
    };
    let f = Fixture::new();
    let source = f.cli(&["run", "--script", "true", "--check", "false", "task"], 1);
    let id = propose(
        &f,
        &source,
        "true",
        "cp .git git-pointer.txt; rm .git; printf evidence > retained.txt",
    );
    let output = f
        .command()
        .args(["--json", "experiment", "run", "--lesson", &id])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("partial evidence retained"));
    let store = Store::open(&f.home).unwrap();
    let experiments = store.experiments().unwrap();
    assert_eq!(experiments.len(), 1);
    assert_eq!(
        experiments[0].status,
        hardknock::experiment::ExperimentStatus::Failed
    );
    assert_eq!(experiments[0].trials.len(), 1);
    assert_eq!(store.lesson(&id.parse().unwrap()).unwrap().version, 1);
    let mut retained = store
        .realities()
        .unwrap()
        .into_iter()
        .find(|r| r.status != RealityStatus::Discarded)
        .unwrap();
    assert!(!retained.ephemeral);
    assert!(retained.root.join("retained.txt").is_file());
    fs::rename(
        retained.root.join("git-pointer.txt"),
        retained.root.join(".git"),
    )
    .unwrap();
    GitRealityProvider::new(&store)
        .discard(&mut retained)
        .unwrap();
    f.assert_source_unchanged();
}

#[test]
fn deterministic_failure_hypothesis_experiment_evidence_loop() {
    let f = Fixture::pnpm();
    let result = f.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "upgrade dependencies",
        ],
        1,
    );
    assert_eq!(result["experience"]["outcome"], "failure");
    assert_eq!(result["execution"]["status"], "succeeded");
    assert_eq!(result["lesson"]["status"], "counterfactually_supported");
    assert_eq!(result["lesson"]["confidence"], 0.78);
    assert_eq!(result["experiment"]["conclusion"], "supports_hypothesis");
    let trials = result["experiment"]["trials"].as_array().unwrap();
    assert_eq!(trials.len(), 2);
    assert_eq!(trials[0]["outcome"], "failure");
    assert_eq!(trials[1]["outcome"], "success");
    assert_ne!(trials[0]["reality_id"], trials[1]["reality_id"]);
    for trial in trials {
        assert_eq!(
            trial["starting_state"],
            result["experience"]["starting_state"]
        );
        assert_eq!(
            trial["environment_fingerprint"],
            result["experience"]["context"]["environment"]["fingerprint"]
        );
        let id = trial["experience_id"].as_str().unwrap();
        let exp = &f.cli(&["experience", "show", id], 0)["experience"];
        assert_eq!(exp["outcome"], trial["outcome"]);
        for artifact in exp["evidence"]["artifacts"].as_array().unwrap() {
            let path = std::path::Path::new(artifact["path"].as_str().unwrap());
            assert_eq!(
                hardknock::store::artifact(path).unwrap().blake3,
                artifact["blake3"]
            );
        }
    }
    let source_id = result["experience"]["id"].as_str().unwrap();
    assert_eq!(
        f.cli(&["experience", "show", source_id], 0)["experience"],
        result["experience"]
    );
    let lesson_id = result["lesson"]["id"].as_str().unwrap();
    let store = Store::open(&f.home).unwrap();
    let revisions = store.lesson_versions(&lesson_id.parse().unwrap()).unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(f64::from(revisions[0].confidence), 0.42);
    assert_eq!(
        f.cli(&["experience", "list"], 0)["experiences"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "No automatic retry may create a fourth Experience"
    );
    assert_eq!(
        f.cli(&["lesson", "show", lesson_id], 0)["lesson"],
        result["lesson"]
    );
    assert_eq!(
        f.cli(
            &[
                "experiment",
                "show",
                result["experiment"]["id"].as_str().unwrap()
            ],
            0
        )["experiment"],
        result["experiment"]
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM lessons l JOIN hypotheses h ON l.hypothesis_id=h.id JOIN experiments e ON e.lesson_id=l.id JOIN trials t ON t.experiment_id=e.id JOIN evaluations v ON v.id=t.evaluation_id JOIN realities r ON r.id=t.reality_id", [], |r|r.get::<_,i64>(0)).unwrap(), 2);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM lesson_evidence WHERE relationship='supports'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    for table in [
        "hypotheses",
        "lesson_versions",
        "trials",
        "trial_artifacts",
        "lesson_evidence",
        "experiments",
    ] {
        assert!(
            db.execute(&format!("UPDATE {table} SET rowid=rowid"), [])
                .is_err()
        );
        assert!(db.execute(&format!("DELETE FROM {table}"), []).is_err());
    }
    let repeated = f.cli(&["experiment", "run", "--lesson", lesson_id], 0);
    assert_eq!(repeated["lesson"]["status"], "counterfactually_supported");
    assert_eq!(repeated["lesson"]["confidence"], 0.78);
    assert_eq!(repeated["lesson"]["version"], 3);
    assert_eq!(
        f.cli(&["experience", "show", source_id], 0)["experience"],
        result["experience"]
    );
    f.assert_source_unchanged();
}

#[test]
fn manual_experiments_cover_all_four_outcome_pairs() {
    for (baseline, alternate, conclusion, status, confidence, exit) in [
        (
            false,
            true,
            "supports_hypothesis",
            "counterfactually_supported",
            0.78,
            0,
        ),
        (
            true,
            false,
            "contradicts_hypothesis",
            "contradicted",
            0.20,
            0,
        ),
        (true, true, "inconclusive", "candidate", 0.42, 3),
        (false, false, "inconclusive", "candidate", 0.42, 3),
    ] {
        let f = Fixture::new();
        let script = if baseline { "touch ok" } else { "true" };
        let alternative = if alternate {
            "touch ok; printf alternative"
        } else {
            "printf alternative"
        };
        let run = f.cli(
            &[
                "run",
                "--script",
                script,
                "--check",
                "test -f ok",
                "manual task",
            ],
            if baseline { 0 } else { 1 },
        );
        assert!(run["lesson"].is_null());
        let proposal = f.cli(
            &[
                "lesson",
                "propose",
                "--experience",
                run["experience"]["id"].as_str().unwrap(),
                "--claim",
                "A scoped manual hypothesis",
                "--avoid",
                script,
                "--prefer",
                alternative,
            ],
            0,
        );
        assert_eq!(
            proposal["hypothesis"]["generated_by"]["kind"],
            "manual-reflection"
        );
        assert_eq!(proposal["lesson"]["status"], "candidate");
        let id = proposal["lesson"]["id"].as_str().unwrap();
        let result = f.cli(&["experiment", "run", "--lesson", id], exit);
        assert_eq!(result["experiment"]["conclusion"], conclusion);
        assert_eq!(result["lesson"]["status"], status);
        assert_eq!(result["lesson"]["confidence"], confidence);
        let store = Store::open(&f.home).unwrap();
        let mut lesson = store.lesson(&id.parse().unwrap()).unwrap();
        let experiment = store
            .experiment(
                &result["experiment"]["id"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap(),
            )
            .unwrap();
        assert!(
            lesson
                .apply_experiment(&experiment, &hardknock::lesson::HeuristicConfidence)
                .is_err(),
            "Duplicate evidence cannot raise confidence"
        );
        f.assert_source_unchanged();
    }
}

#[test]
fn checks_are_separate_from_process_success_and_all_are_required() {
    let f = Fixture::new();
    let result = f.cli(
        &[
            "run",
            "--agent-command",
            "sh -c '{task}'",
            "--check",
            "echo package_manager_conflict >&2; exit 1",
            "--check",
            "printf check-output; printf checked > checked.txt",
            "exit 0",
        ],
        1,
    );
    assert_eq!(result["execution"]["status"], "succeeded");
    let exp: Experience = serde_json::from_value(result["experience"].clone()).unwrap();
    assert_eq!(exp.outcome, Outcome::Failure);
    assert!(!exp.evaluation.success);
    assert_eq!(exp.evaluation.checks.len(), 2);
    assert!(
        exp.failure_signatures
            .iter()
            .any(|s| s.signature == "package_manager_conflict")
    );
    assert!(
        fs::read_to_string(
            &exp.evidence
                .artifacts
                .iter()
                .find(|a| a.path.ends_with("diff.patch"))
                .unwrap()
                .path
        )
        .unwrap()
        .contains("+checked")
    );
    assert!(
        !fs::read_to_string(result["execution"]["diff"]["path"].as_str().unwrap())
            .unwrap()
            .contains("checked")
    );
    for a in &exp.evidence.artifacts {
        assert_eq!(artifact(&a.path).unwrap().blake3, a.blake3);
    }
    let store = Store::open(&f.home).unwrap();
    assert!(ExperienceStore::insert(&store, &exp).is_err());
    assert_eq!(
        ExperienceStore::list(
            &store,
            ExperienceQuery {
                outcome: Some(Outcome::Failure)
            }
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        ExperienceStore::list(
            &store,
            ExperienceQuery {
                outcome: Some(Outcome::Success)
            }
        )
        .unwrap()
        .is_empty()
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    for table in ["experiences", "evaluations", "experience_artifacts"] {
        assert!(
            db.execute(&format!("UPDATE {table} SET rowid=rowid"), [])
                .is_err()
        );
        assert!(db.execute(&format!("DELETE FROM {table}"), []).is_err());
    }
    drop(store);
    assert_eq!(
        f.cli(&["experience", "show", &exp.id.to_string()], 0)["experience"],
        result["experience"]
    );
    assert_eq!(
        f.cli(&["experience", "list"], 0)["experiences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.assert_source_unchanged();
}

#[test]
fn checks_decide_task_success_and_missing_checks_are_inconclusive() {
    let f = Fixture::new();
    let evaluated = f.cli(
        &[
            "run",
            "--agent-command",
            "sh -c '{task}'",
            "--check",
            "test -f tracked.txt",
            "exit 7",
        ],
        0,
    );
    assert_eq!(evaluated["execution"]["status"], "failed");
    assert_eq!(evaluated["experience"]["outcome"], "success");
    let unknown = f.cli(&["run", "--agent-command", "sh -c '{task}'", "exit 0"], 0);
    assert_eq!(unknown["experience"]["outcome"], "inconclusive");
    assert_eq!(
        unknown["experience"]["evaluation"]["status"],
        "not_configured"
    );
    let timeout = f.cli(
        &[
            "run",
            "--agent-command",
            "sh -c '{task}'",
            "--check",
            "sleep 5",
            "--check",
            "touch must-not-run",
            "--timeout-secs",
            "1",
            "exit 0",
        ],
        1,
    );
    assert_eq!(timeout["experience"]["outcome"], "timed_out");
    assert_eq!(
        timeout["experience"]["evaluation"]["checks"][1]["status"],
        "not_run"
    );
    f.assert_source_unchanged();
}
