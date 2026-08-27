// SPDX-License-Identifier: Apache-2.0

mod support;

use hardknock::{
    experience::Experience,
    lesson::{ActionPattern, Lesson},
    retrieval::{
        DeterministicRelevance, QueryContext, RelevancePolicy, RelevanceScore, RetrievalOptions,
    },
    store::{Store, artifact},
};
use serde_json::{Value, json};
use std::{fs, process::Stdio};
use support::{Fixture, git};

fn train(a: &Fixture) -> Value {
    a.cli(
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
    )
}

fn related(a: &Fixture, name: &str) -> Fixture {
    let mut f = Fixture::from_fixture(name);
    f.home = a.home.clone();
    f
}

fn apply(b: &Fixture, task: &str) -> Value {
    b.cli(
        &["run", "--agent", "test-agent", "--check", "./test.sh", task],
        0,
    )
}

fn commit(f: &Fixture, file: &str, content: &str) {
    fs::write(f.repo.join(file), content).unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "change fixture context"]);
}

#[test]
fn retry_records_new_evidence_from_original_state_without_validating_a_replay() {
    let a = Fixture::pnpm();
    let result = train(&a);
    let source = &result["experience"];
    let retry = &result["retries"][0]["experience"];
    assert_eq!(source["outcome"], "failure");
    assert_eq!(retry["outcome"], "success");
    assert_ne!(source["id"], retry["id"]);
    assert_eq!(source["starting_state"], retry["starting_state"]);
    assert_eq!(source["goal"], retry["goal"]);
    assert_eq!(result["retries"].as_array().unwrap().len(), 1);
    assert!(
        retry["relations"]
            .as_array()
            .unwrap()
            .contains(&json!({"kind":"retry_of", "experience_id":source["id"]}))
    );
    let app = &retry["lesson_applications"][0];
    assert_eq!(app["influence"], "applied");
    assert_eq!(app["verification"], "observed");
    assert_eq!(
        app["resulting_action"]["pattern"],
        "./agent-script.sh alternative"
    );
    assert_eq!(result["lesson"]["status"], "counterfactually_supported");
    assert_eq!(result["lesson"]["confidence"], 0.78);
    assert_eq!(
        result["lesson"]["validation"]["distinct_successful_contexts"],
        0
    );
    assert_eq!(a.cli(&["status"], 0)["counts"]["experiences"], 4);
    for trial in result["experiment"]["trials"].as_array().unwrap() {
        let exp = a.cli(
            &[
                "experience",
                "show",
                trial["experience_id"].as_str().unwrap(),
            ],
            0,
        );
        assert!(
            exp["experience"]["relations"]
                .as_array()
                .unwrap()
                .contains(&json!({"kind":"counterfactual_of", "experience_id":source["id"]}))
        );
    }
    let store = Store::open(&a.home).unwrap();
    let saved: Experience = serde_json::from_value(retry.clone()).unwrap();
    assert_eq!(store.applications(&saved.id).unwrap().len(), 1);
    for proof in &saved.evidence.artifacts {
        assert_eq!(artifact(&proof.path).unwrap().blake3, proof.blake3);
    }
    assert_eq!(
        a.cli(&["experience", "show", source["id"].as_str().unwrap()], 0)["experience"],
        *source
    );
    let db = rusqlite::Connection::open(a.home.join("hardknock.db")).unwrap();
    for table in [
        "lesson_applications",
        "experience_relations",
        "repeated_mistakes",
        "lesson_validations",
    ] {
        // Even a no-op update must be refused when an evidence row exists.
        if table != "repeated_mistakes" {
            assert!(db.execute(&format!("DELETE FROM {table}"), []).is_err());
        }
    }
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    a.assert_source_unchanged();
}

#[test]
fn transfer_improves_outcome_reduces_repeated_mistakes_and_preserves_full_provenance() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    let id = learned["lesson"]["id"].as_str().unwrap();
    let b = related(&a, "pnpm-workspace-transfer");
    let search = b.cli(&["lesson", "search", "--action", "npm install"], 0);
    assert_eq!(search["report"]["matches"][0]["lesson"]["id"], id);
    assert_eq!(search["report"]["matches"][0]["relevance"], 0.8);
    assert_eq!(
        search["report"]["matches"][0]["matched_context"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let control = b.cli(
        &[
            "run",
            "--no-experience",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "upgrade service and worker",
        ],
        1,
    );
    assert_eq!(control["experience"]["outcome"], "failure");
    assert_eq!(
        control["experience"]["repeated_mistakes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        control["experience"]["lesson_applications"][0]["delivered"],
        false
    );
    assert_eq!(
        control["experience"]["lesson_applications"][0]["influence"],
        "ignored"
    );
    assert!(control["experiment"].is_null());
    let aware = apply(&b, "upgrade service and worker");
    let exp = &aware["experience"];
    assert_eq!(exp["outcome"], "success");
    assert!(exp["repeated_mistakes"].as_array().unwrap().is_empty());
    assert_ne!(
        exp["starting_state"]["tree_hash"],
        learned["experience"]["starting_state"]["tree_hash"]
    );
    assert_eq!(exp["lesson_applications"][0]["influence"], "applied");
    assert_eq!(aware["lesson"]["status"], "validated");
    assert_eq!(aware["lesson"]["confidence"], 0.90);
    let why = b.cli(&["why", "--experience", exp["id"].as_str().unwrap()], 0);
    let explanation = &why["explanation"]["applications"][0];
    assert_eq!(explanation["source"]["id"], learned["experience"]["id"]);
    assert_eq!(explanation["source"]["outcome"], "failure");
    assert_eq!(
        explanation["experiments"][0]["id"],
        learned["experiment"]["id"]
    );
    assert_eq!(
        explanation["lesson_at_application"]["status"],
        "counterfactually_supported"
    );
    assert_eq!(explanation["current_lesson"]["status"], "validated");
    assert_eq!(
        b.cli(&["why"], 0)["explanation"]["experience_id"],
        exp["id"]
    );
    assert_eq!(
        a.cli(
            &[
                "experience",
                "show",
                learned["experience"]["id"].as_str().unwrap()
            ],
            0
        )["experience"],
        learned["experience"]
    );
    assert_eq!(
        a.cli(
            &[
                "experiment",
                "show",
                learned["experiment"]["id"].as_str().unwrap()
            ],
            0
        )["experiment"],
        learned["experiment"]
    );
    assert_eq!(b.cli(&["status"], 0)["counts"]["repeated_mistakes"], 1);
    a.assert_source_unchanged();
    b.assert_source_unchanged();
}

#[test]
fn renamed_tasks_and_identical_clones_do_not_inflate_distinct_transfer_count() {
    let a = Fixture::pnpm();
    train(&a);
    let b = related(&a, "pnpm-workspace-transfer");
    apply(&b, "upgrade services");
    let renamed = apply(&b, "different task name");
    assert_eq!(renamed["lesson"]["confidence"], 0.90);
    let clone = related(&a, "pnpm-workspace-transfer");
    let identical = apply(&clone, "another checkout");
    assert_eq!(
        identical["experience"]["starting_state"]["tree_hash"],
        renamed["experience"]["starting_state"]["tree_hash"]
    );
    assert_eq!(
        identical["lesson"]["validation"]["distinct_successful_contexts"],
        1
    );
    assert_eq!(identical["lesson"]["confidence"], 0.90);
    commit(
        &clone,
        "service-configuration.json",
        "{\"queue\":\"priority\"}\n",
    );
    let second = apply(&clone, "upgrade with priority queue");
    assert_eq!(
        second["lesson"]["validation"]["distinct_successful_contexts"],
        2
    );
    assert_eq!(second["lesson"]["confidence"], 0.94);
    clone.assert_source_unchanged();
}

#[test]
fn irrelevant_npm_context_is_excluded_even_with_matching_proposed_action() {
    let a = Fixture::pnpm();
    train(&a);
    let c = related(&a, "npm-ordinary");
    let search = c.cli(&["lesson", "search", "--action", "npm install"], 0);
    assert!(search["report"]["matches"].as_array().unwrap().is_empty());
    assert!(
        search["report"]["excluded"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("scope")
    );
    let run = apply(&c, "install ordinary npm app");
    assert!(
        run["experience"]["lesson_applications"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let log = fs::read_to_string(
        run["execution"]["action"]["stdout"]["path"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(log.contains("ACTION shell npm install"));
    assert!(!log.contains("APPLIED"));
    c.assert_source_unchanged();
}

#[test]
fn retest_records_contradiction_and_explicit_retirement_preserves_history() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    let id = learned["lesson"]["id"].as_str().unwrap();
    let b = related(&a, "pnpm-workspace-transfer");
    let validated = apply(&b, "upgrade services");
    let d = related(&a, "pnpm-workspace-contradiction");
    let tested = d.cli(&["lesson", "test", id], 0);
    assert_eq!(tested["experiment"]["conclusion"], "contradicts_hypothesis");
    assert_eq!(tested["experiment"]["trials"][0]["outcome"], "success");
    assert_eq!(tested["experiment"]["trials"][1]["outcome"], "failure");
    assert_eq!(tested["lesson"]["status"], "contradicted");
    assert_eq!(tested["lesson"]["confidence"], 0.20);
    assert_eq!(tested["lesson"]["validation"]["validated"], false);
    assert!(tested["lesson"]["retired_at"].is_null());
    assert!(
        b.cli(&["lesson", "search", "--action", "npm install"], 0)["report"]["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let retired = d.cli(
        &[
            "lesson",
            "retire",
            id,
            "--reason",
            "legacy output needs npm",
        ],
        0,
    );
    assert_eq!(retired["lesson"]["status"], "retired");
    assert_eq!(
        retired["lesson"]["retired_reason"],
        "legacy output needs npm"
    );
    assert!(retired["lesson"]["retired_at"].is_string());
    assert!(
        d.cli(&["lesson", "list"], 0)["lessons"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        d.cli(&["lesson", "list", "--include-retired"], 0)["lessons"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        d.cli(&["lesson", "retire", id], 0)["lesson"],
        retired["lesson"]
    );
    let store = Store::open(&a.home).unwrap();
    let versions = store.lesson_versions(&id.parse().unwrap()).unwrap();
    assert!(
        versions
            .iter()
            .any(|v| serde_json::to_value(v).unwrap() == validated["lesson"])
    );
    assert_eq!(
        a.cli(
            &[
                "experiment",
                "show",
                learned["experiment"]["id"].as_str().unwrap()
            ],
            0
        )["experiment"],
        learned["experiment"]
    );
    d.assert_source_unchanged();
}

#[test]
fn ignored_advice_does_not_validate_and_retry_budget_is_hard_bounded() {
    let a = Fixture::pnpm();
    train(&a);
    let b = related(&a, "pnpm-workspace-transfer");
    commit(
        &b,
        "ignore-experience",
        "fixture deliberately ignores recommendations\n",
    );
    let result = b.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "--max-retries",
            "2",
            "upgrade services",
        ],
        1,
    );
    assert_eq!(result["retries"].as_array().unwrap().len(), 2);
    let mut previous = result["experience"]["id"].clone();
    for attempt in result["retries"].as_array().unwrap() {
        let exp = &attempt["experience"];
        assert_eq!(exp["outcome"], "failure");
        assert_eq!(exp["lesson_applications"][0]["influence"], "ignored");
        assert_eq!(
            exp["relations"][0],
            json!({"kind":"retry_of", "experience_id":previous})
        );
        previous = exp["id"].clone();
    }
    assert_eq!(result["lesson"]["status"], "counterfactually_supported");
    assert_eq!(result["lesson"]["confidence"], 0.78);
    assert_eq!(b.cli(&["status"], 0)["counts"]["repeated_mistakes"], 3);
    b.assert_source_unchanged();
}

#[test]
fn opaque_agent_usage_is_self_reported_and_malformed_reports_are_visible() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    let id = learned["lesson"]["id"].as_str().unwrap();
    let b = related(&a, "pnpm-workspace-transfer");
    let report = json!({"schema_version":1, "applications":[{"lesson_id":id, "influence":"applied", "resulting_action":{"type":"shell_command", "pattern":"./agent-script.sh alternative"}}]});
    let task = format!(
        "test -f .hardknock/context.json; ./agent-script.sh alternative; printf '%s' '{}' > .hardknock/usage.json",
        report
    );
    let run = b.cli(
        &[
            "run",
            "--with-experience",
            "--action",
            "npm install",
            "--agent-command",
            "sh -c {task}",
            "--check",
            "./test.sh",
            &task,
        ],
        0,
    );
    assert_eq!(
        run["experience"]["lesson_applications"][0]["influence"],
        "applied"
    );
    assert_eq!(
        run["experience"]["lesson_applications"][0]["verification"],
        "self_reported"
    );
    assert_eq!(run["lesson"]["confidence"], 0.78);
    assert_eq!(run["lesson"]["status"], "counterfactually_supported");
    let malformed = b.cli(
        &[
            "run",
            "--with-experience",
            "--action",
            "npm install",
            "--agent-command",
            "sh -c {task}",
            "--check",
            "./test.sh",
            "./agent-script.sh alternative; printf invalid > .hardknock/usage.json",
        ],
        0,
    );
    assert_eq!(
        malformed["experience"]["lesson_applications"][0]["influence"],
        "retrieved"
    );
    assert_eq!(
        malformed["experience"]["application_report_errors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(malformed["lesson"]["status"], "counterfactually_supported");
    b.assert_source_unchanged();
}

#[test]
fn concurrent_transfers_preserve_evidence_without_double_counting_same_tree() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    let b = related(&a, "pnpm-workspace-transfer");
    let children: Vec<_> = (0..2)
        .map(|_| {
            b.command()
                .args([
                    "--json",
                    "run",
                    "--agent",
                    "test-agent",
                    "--check",
                    "./test.sh",
                    "upgrade services",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let store = Store::open(&a.home).unwrap();
    let id = learned["lesson"]["id"].as_str().unwrap().parse().unwrap();
    let lesson = store.lesson(&id).unwrap();
    assert_eq!(lesson.version, 5);
    assert_eq!(f64::from(lesson.confidence), 0.90);
    assert_eq!(
        store
            .lesson_evidence_summary(&id)
            .unwrap()
            .distinct_successes(),
        1
    );
    assert_eq!(lesson.evidence.len(), 6);
    b.assert_source_unchanged();
}

#[test]
fn relevance_scope_and_thresholds_are_deterministic_and_fail_closed() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    let lesson: Lesson = serde_json::from_value(learned["lesson"].clone()).unwrap();
    let exp: Experience = serde_json::from_value(learned["experience"].clone()).unwrap();
    let context = QueryContext::new(
        &exp.context,
        "upgrade",
        vec![ActionPattern::shell("npm install")],
    );
    assert_eq!(
        f64::from(DeterministicRelevance.score(&lesson, &context).0),
        0.8
    );
    let mut strict = lesson.clone();
    strict.context_match.repository = Some(exp.context.repository.path.clone());
    assert_eq!(
        f64::from(DeterministicRelevance.score(&strict, &context).0),
        1.0
    );
    for field in ["markers", "tags", "os", "arch", "repo"] {
        let mut wrong = context.clone();
        match field {
            "markers" => wrong.detected_markers.clear(),
            "tags" => wrong.tags.clear(),
            "os" => wrong.environment.os = "other".into(),
            "arch" => wrong.environment.arch = "other".into(),
            "repo" => wrong.repository.path = "/unrelated".into(),
            _ => unreachable!(),
        }
        assert_eq!(
            f64::from(DeterministicRelevance.score(&strict, &wrong).0),
            0.0,
            "{field}"
        );
    }
    for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert!(RelevanceScore::try_from(value).is_err());
    }
    let options = RetrievalOptions {
        minimum: RelevanceScore::try_from(0.9).unwrap(),
        ..Default::default()
    };
    assert!(options.validate().is_err());
    let result = a.cli(
        &[
            "lesson",
            "search",
            "--action",
            "npm install",
            "--min-relevance",
            "0.85",
            "--recommend-threshold",
            "0.85",
        ],
        0,
    );
    assert!(result["report"]["matches"].as_array().unwrap().is_empty());
}

#[test]
fn candidates_are_debug_searchable_but_never_delivered() {
    let a = Fixture::pnpm();
    let learned = train(&a);
    a.cli(
        &[
            "lesson",
            "retire",
            learned["lesson"]["id"].as_str().unwrap(),
        ],
        0,
    );
    let candidate = a.cli(
        &[
            "lesson",
            "propose",
            "--experience",
            learned["experience"]["id"].as_str().unwrap(),
            "--claim",
            "unverified suggestion",
            "--avoid",
            "./agent-script.sh baseline",
            "--prefer",
            "./agent-script.sh alternative",
        ],
        0,
    );
    let search = a.cli(
        &["lesson", "search", "--action", "./agent-script.sh baseline"],
        0,
    );
    assert!(search["report"]["matches"].as_array().unwrap().is_empty());
    let debug = a.cli(
        &[
            "lesson",
            "search",
            "--include-candidates",
            "--action",
            "./agent-script.sh baseline",
        ],
        0,
    );
    assert_eq!(debug["report"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(
        debug["report"]["matches"][0]["lesson"]["id"],
        candidate["lesson"]["id"]
    );
    let run = a.cli(
        &[
            "run",
            "--with-experience",
            "--script",
            "./agent-script.sh run",
            "--action",
            "./agent-script.sh baseline",
            "--check",
            "./test.sh",
            "test unvalidated suggestion",
        ],
        1,
    );
    assert!(
        run["experience"]["lesson_applications"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    a.assert_source_unchanged();
}

#[test]
fn context_collision_or_symlink_aborts_before_agent_execution() {
    for symlink in [false, true] {
        let a = Fixture::pnpm();
        let outside = a.temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("context.md"), "do not overwrite").unwrap();
        if symlink {
            std::os::unix::fs::symlink(&outside, a.repo.join(".hardknock")).unwrap();
        } else {
            fs::create_dir(a.repo.join(".hardknock")).unwrap();
            fs::write(a.repo.join(".hardknock/context.md"), "repository content").unwrap();
        }
        git(&a.repo, &["add", "."]);
        git(&a.repo, &["commit", "-m", "reserved context collision"]);
        let output = a
            .command()
            .args([
                "--json",
                "run",
                "--agent",
                "test-agent",
                "--check",
                "./test.sh",
                "task",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(5));
        assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
        assert_eq!(a.cli(&["status"], 0)["counts"]["experiences"], 0);
        assert_eq!(
            fs::read_to_string(outside.join("context.md")).unwrap(),
            "do not overwrite"
        );
        a.assert_source_unchanged();
    }
}

#[test]
fn cancellation_during_retry_keeps_lineage_and_stops_further_attempts() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let a = Fixture::pnpm();
    let ready = a.temp.path().join("retry-ready");
    let original = fs::read_to_string(a.repo.join("agent-script.sh")).unwrap();
    let modified = original.replace("  exec \"$0\" \"$strategy\"", &format!("  if [ \"$strategy\" = alternative ]; then\n    touch '{}'\n    sleep 30\n  fi\n  exec \"$0\" \"$strategy\"", ready.display()));
    assert_ne!(original, modified);
    commit(&a, "agent-script.sh", &modified);
    let mut child = a
        .command()
        .args([
            "--json",
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "--max-retries",
            "3",
            "cancel retry",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            panic!("retry did not reach its cancellation point");
        }
        thread::sleep(Duration::from_millis(10));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["retries"].as_array().unwrap().len(), 1);
    let retry = &result["retries"][0]["experience"];
    assert_eq!(retry["outcome"], "interrupted");
    assert_eq!(
        result["retry_stop_reason"],
        "Interrupted; no further attempts"
    );
    assert_eq!(
        retry["relations"][0],
        json!({"kind":"retry_of", "experience_id":result["experience"]["id"]})
    );
    assert_ne!(retry["lesson_applications"][0]["influence"], "applied");
    assert_eq!(result["lesson"]["confidence"], 0.78);
    a.assert_source_unchanged();
}
