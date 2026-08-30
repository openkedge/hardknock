// SPDX-License-Identifier: Apache-2.0

mod support;

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use hardknock::{
    core::{RealityId, RealityStatus},
    dojo::{GitRealityProvider, RealityProvider, capture_state},
    store::Store,
};
use support::{Fixture, git};

#[test]
fn worktrees_fork_original_state_and_diff_without_changing_indexes() {
    let f = Fixture::new();
    fs::write(f.repo.join("ignored.txt"), "not part of snapshot").unwrap();
    let state = capture_state(&f.repo).unwrap();
    let store = Store::open(&f.home).unwrap();
    let provider = GitRealityProvider::new(&store);
    let mut reality = provider.create(&state).unwrap();
    assert!(!reality.root.join("ignored.txt").exists());
    fs::write(reality.root.join("tracked.txt"), "staged\n").unwrap();
    git(&reality.root, &["add", "tracked.txt"]);
    let index_before = git(&reality.root, &["show", ":tracked.txt"]).stdout;
    fs::write(reality.root.join("tracked.txt"), "current\n").unwrap();
    fs::write(reality.root.join("new.txt"), "new\n").unwrap();
    let patch = String::from_utf8(provider.diff(&reality).unwrap()).unwrap();
    assert!(patch.contains("+current") && patch.contains("+new"));
    assert_eq!(
        git(&reality.root, &["show", ":tracked.txt"]).stdout,
        index_before
    );
    let mut fork = provider.fork(&reality).unwrap();
    assert_eq!(fork.parent, Some(reality.id.clone()));
    assert_eq!(fork.starting_state, state);
    assert_eq!(
        fs::read_to_string(fork.root.join("tracked.txt")).unwrap(),
        "original\n"
    );
    assert!(!fork.root.join("new.txt").exists());
    provider.discard(&mut reality).unwrap();
    provider.discard(&mut reality).unwrap();
    provider.discard(&mut fork).unwrap();
    assert_eq!(
        store.reality(&reality.id).unwrap().status,
        RealityStatus::Discarded
    );
    f.assert_source_unchanged();
}

#[test]
fn starting_state_rejects_dirty_unborn_non_git_and_submodule_repositories() {
    let f = Fixture::new();
    fs::write(f.repo.join("new"), "untracked").unwrap();
    assert!(
        capture_state(&f.repo)
            .unwrap_err()
            .to_string()
            .contains("clean")
    );
    fs::remove_file(f.repo.join("new")).unwrap();
    fs::write(f.repo.join("tracked.txt"), "dirty").unwrap();
    assert!(capture_state(&f.repo).is_err());
    git(&f.repo, &["add", "."]);
    assert!(capture_state(&f.repo).is_err());
    git(&f.repo, &["commit", "-m", "changed"]);
    let commit = capture_state(&f.repo).unwrap().git_commit;
    git(
        &f.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},nested"),
        ],
    );
    git(&f.repo, &["commit", "-m", "gitlink"]);
    // Populate an empty submodule directory so Git's status is clean.
    fs::create_dir(f.repo.join("nested")).unwrap();
    assert!(
        capture_state(&f.repo)
            .unwrap_err()
            .to_string()
            .contains("Submodule")
    );
    let unborn = f.temp.path().join("unborn");
    fs::create_dir(&unborn).unwrap();
    git(&unborn, &["init"]);
    assert!(
        capture_state(&unborn)
            .unwrap_err()
            .to_string()
            .contains("no commit")
    );
    assert!(capture_state(f.temp.path()).is_err());
}

#[test]
fn discard_refuses_unmanaged_paths_and_symlink_replacements() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let provider = GitRealityProvider::new(&store);
    let mut reality = provider.create(&capture_state(&f.repo).unwrap()).unwrap();
    let mut tampered = reality.clone();
    tampered.root = f.repo.clone();
    assert!(provider.discard(&mut tampered).is_err());
    let saved = f.temp.path().join("saved");
    fs::rename(&reality.root, &saved).unwrap();
    symlink(&f.repo, &reality.root).unwrap();
    assert!(provider.discard(&mut reality).is_err());
    fs::remove_file(&reality.root).unwrap();
    fs::rename(saved, &reality.root).unwrap();
    provider.discard(&mut reality).unwrap();
    f.assert_source_unchanged();
}

#[test]
fn migrations_are_idempotent_and_reality_history_survives_reopen() {
    let f = Fixture::new();
    let id = {
        let store = Store::open(&f.home).unwrap();
        let provider = GitRealityProvider::new(&store);
        let mut reality = provider.create(&capture_state(&f.repo).unwrap()).unwrap();
        provider.discard(&mut reality).unwrap();
        reality.id
    };
    let reopened = Store::open(&f.home).unwrap();
    assert_eq!(
        reopened.reality(&id).unwrap().status,
        RealityStatus::Discarded
    );
    assert!(reopened.reality(&RealityId::new()).is_err());
    let connection = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        11
    );
    assert_eq!(
        fs::metadata(&f.home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    connection
        .execute("INSERT INTO schema_migrations(version) VALUES(99)", [])
        .unwrap();
    assert!(Store::open(&f.home).is_err());
}

#[test]
fn cleanup_only_removes_unlocked_automatic_realities() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let provider = GitRealityProvider::new(&store);
    let state = capture_state(&f.repo).unwrap();
    let (automatic, lease) = provider.create_for_run(&state, false).unwrap();
    let mut manual = provider.create(&state).unwrap();
    let busy = f.cli(&["reality", "cleanup"], 0);
    assert_eq!(busy["skipped_active"][0], automatic.id.to_string());
    let discard = f
        .command()
        .args(["--json", "reality", "discard", &automatic.id.to_string()])
        .output()
        .unwrap();
    assert_eq!(discard.status.code(), Some(5));
    drop(lease);
    let cleaned = f.cli(&["reality", "cleanup"], 0);
    assert_eq!(cleaned["discarded"][0], automatic.id.to_string());
    assert!(!automatic.root.exists());
    assert!(manual.root.exists());
    assert!(
        f.cli(&["reality", "cleanup"], 0)["discarded"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    provider.discard(&mut manual).unwrap();
    f.assert_source_unchanged();
}

#[test]
fn source_repository_and_unrelated_directories_are_not_used_as_data_homes() {
    let f = Fixture::new();
    let path = f.repo.join("state");
    let output = f
        .command()
        .arg("--home")
        .arg(&path)
        .args(["reality", "create"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert!(!path.exists());
    assert!(Store::open(f.temp.path()).is_err());
    assert!(Path::new(&f.repo).join("tracked.txt").exists());
    let _store = Store::open(&f.home).unwrap();
    fs::remove_dir(f.home.join("realities")).unwrap();
    symlink(&f.repo, f.home.join("realities")).unwrap();
    assert!(Store::open(&f.home).is_err());
}

#[test]
fn missing_worktree_cleanup_and_snapshot_validation() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let provider = GitRealityProvider::new(&store);
    let state = capture_state(&f.repo).unwrap();
    let mut invalid = state.clone();
    invalid.git_commit = "HEAD".into();
    assert!(provider.create(&invalid).is_err());
    invalid = state.clone();
    invalid.tree_hash = "0".repeat(40);
    assert!(provider.create(&invalid).is_err());
    let mut reality = provider.create(&state).unwrap();
    fs::remove_dir_all(&reality.root).unwrap();
    provider.discard(&mut reality).unwrap();
    f.assert_source_unchanged();
}
