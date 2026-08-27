// SPDX-License-Identifier: Apache-2.0

mod support;
use hardknock::{
    experience::{Experience, Outcome},
    store::{ExperienceQuery, ExperienceStore, Store, artifact},
};
use std::fs;
use support::Fixture;

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
