mod support;

use std::{
    fs,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use hardknock::{
    core::ExecutionRecord,
    store::{Store, artifact},
};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;
use support::Fixture;

#[test]
fn help_version_empty_listing_and_json_errors() {
    let f = Fixture::new();
    for flag in ["--help", "--version"] {
        let output = f.command().arg(flag).output().unwrap();
        assert!(output.status.success());
        assert!(!f.home.exists());
    }
    assert!(
        f.cli(&["reality", "list"], 0)["realities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = f
        .command()
        .args(["--json", "reality", "show", "../../elsewhere"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["event"], "error");
}

#[test]
fn run_captures_output_diff_and_immutable_metadata_then_discards() {
    let f = Fixture::new();
    let result = f.cli(&["run", "--agent-command", "sh -c '{task}'", "printf 'hello\\n'; printf 'diagnostic\\n' >&2; printf changed > tracked.txt; printf new > new.txt"], 0);
    let record: ExecutionRecord = serde_json::from_value(result["execution"].clone()).unwrap();
    assert_eq!(result["reality"]["status"], "discarded");
    assert_eq!(
        fs::read_to_string(&record.action.stdout.path).unwrap(),
        "hello\n"
    );
    assert_eq!(
        fs::read_to_string(&record.action.stderr.path).unwrap(),
        "diagnostic\n"
    );
    let diff = fs::read_to_string(&record.diff.path).unwrap();
    assert!(diff.contains("+changed") && diff.contains("+new"));
    assert_eq!(
        artifact(&record.diff.path).unwrap().blake3,
        record.diff.blake3
    );
    assert_eq!(artifact(&record.action.stdout.path).unwrap().bytes, 6);
    let stored = f.cli(&["execution", "show", &record.id.to_string()], 0);
    assert_eq!(stored["execution"], result["execution"]);
    let store = Store::open(&f.home).unwrap();
    assert!(store.insert_execution(&record).is_err());
    let connection = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        connection
            .execute("UPDATE executions SET data='{}'", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM executions", []).is_err());
    f.assert_source_unchanged();
}

#[test]
fn literal_argument_substitution_does_not_execute_shell_syntax() {
    let f = Fixture::new();
    let task = "$(touch should-not-exist); \"quoted\" 'single'\nnext";
    let result = f.cli(&["run", "--agent-command", "printf %s {task}", task], 0);
    assert_eq!(
        fs::read_to_string(
            result["execution"]["action"]["stdout"]["path"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        task
    );
    f.assert_source_unchanged();
}

#[test]
fn keep_diff_and_explicit_discard_preserve_saved_execution() {
    let f = Fixture::new();
    let result = f.cli(
        &[
            "run",
            "--keep",
            "--agent-command",
            "sh -c {task}",
            "printf new > new.txt",
        ],
        0,
    );
    let id = result["reality"]["id"].as_str().unwrap();
    let root = Path::new(result["reality"]["root"].as_str().unwrap());
    assert!(root.join("new.txt").exists());
    let diff = f.cli(&["reality", "diff", id], 0);
    assert!(
        fs::read_to_string(diff["artifact"]["path"].as_str().unwrap())
            .unwrap()
            .contains("+new")
    );
    assert_eq!(
        f.cli(&["reality", "discard", id], 0)["reality"]["status"],
        "discarded"
    );
    assert!(!root.exists());
    f.cli(
        &[
            "execution",
            "show",
            result["execution"]["id"].as_str().unwrap(),
        ],
        0,
    );
    f.assert_source_unchanged();
}

#[test]
fn failures_timeouts_and_missing_executable_do_not_leave_worktrees() {
    let f = Fixture::new();
    let failed = f.cli(
        &[
            "run",
            "--agent-command",
            "sh -c {task}",
            "printf failed >&2; exit 7",
        ],
        1,
    );
    assert_eq!(failed["execution"]["action"]["exit_code"], 7);
    assert_eq!(failed["execution"]["status"], "failed");
    let missing = f
        .command()
        .args([
            "--json",
            "run",
            "--agent-command",
            "hardknock-no-such-agent {task}",
            "task",
        ])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    let events: Vec<Value> = String::from_utf8(missing.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["event"], "error");
    assert!(
        events.last().unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("Could not start")
    );
    let timeout = f.cli(
        &[
            "run",
            "--timeout-secs",
            "1",
            "--agent-command",
            "sh -c {task}",
            "printf started; sleep 30",
        ],
        1,
    );
    assert_eq!(timeout["execution"]["status"], "timed_out");
    assert_eq!(timeout["execution"]["action"]["signal"], 9);
    f.assert_source_unchanged();
}

#[test]
fn ctrl_c_stops_process_group_records_interruption_and_cleans_up() {
    let f = Fixture::new();
    let ready = f.temp.path().join("ready");
    let sentinel = f.temp.path().join("must-not-be-written");
    let task = format!(
        "printf ready > '{}'; (sleep 2; touch '{}') & wait",
        ready.display(),
        sentinel.display()
    );
    let mut child = f
        .command()
        .args(["--json", "run", "--agent-command", "sh -c {task}", &task])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("Agent did not start");
        }
        thread::sleep(Duration::from_millis(10));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("Ctrl-C did not finish cleanup");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["execution"]["status"], "interrupted");
    f.assert_source_unchanged();
    thread::sleep(Duration::from_millis(2200));
    assert!(
        !sentinel.exists(),
        "A background descendant survived cancellation"
    );
}

#[test]
fn quiet_keeps_safety_warning_and_verbose_does_not_dump_environment() {
    let f = Fixture::new();
    let quiet = f
        .command()
        .args([
            "--quiet",
            "--no-emoji",
            "run",
            "--agent-command",
            "printf %s {task}",
            "ok",
        ])
        .output()
        .unwrap();
    assert!(quiet.status.success() && quiet.stdout.is_empty());
    assert!(String::from_utf8_lossy(&quiet.stderr).contains("Credentials: shared"));
    let verbose = f
        .command()
        .env_remove("RUST_LOG")
        .env("HK_TEST_SECRET_TOKEN", "never-print-this-value")
        .args([
            "--verbose",
            "--json",
            "run",
            "--agent-command",
            "printf %s {task}",
            "ok",
        ])
        .output()
        .unwrap();
    assert!(verbose.status.success());
    serde_json::from_slice::<Value>(&verbose.stdout).unwrap();
    let stderr = String::from_utf8(verbose.stderr).unwrap();
    assert!(!stderr.contains("never-print-this-value"));
    for line in stderr.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[test]
fn failed_diff_capture_retains_trial_files_for_recovery() {
    let f = Fixture::new();
    // Corrupt only the disposable worktree's Git pointer to force capture to fail.
    let output = f
        .command()
        .args([
            "--json",
            "run",
            "--agent-command",
            "sh -c {task}",
            "printf valuable > recovery.txt; printf invalid > .git",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("retained"));
    let listed = f.cli(&["reality", "list"], 0);
    let reality = &listed["realities"][0];
    let root = Path::new(reality["root"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(root.join("recovery.txt")).unwrap(),
        "valuable"
    );
    assert_eq!(reality["ephemeral"], false);
    assert!(
        f.cli(&["reality", "cleanup"], 0)["discarded"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Restore the worktree's pointer from its source-owned registration before disposal.
    let registrations = f.repo.join(".git/worktrees");
    let registration = fs::read_dir(registrations)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", registration.display()),
    )
    .unwrap();
    f.cli(&["reality", "discard", reality["id"].as_str().unwrap()], 0);
    f.assert_source_unchanged();
}
