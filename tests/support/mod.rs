// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

pub struct Fixture {
    pub temp: TempDir,
    pub repo: PathBuf,
    pub home: PathBuf,
}

pub fn git(repo: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let output = cmd
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

impl Fixture {
    pub fn pnpm() -> Self {
        let fixture = Self::new();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/pnpm-workspace-conflict");
        fs::create_dir_all(fixture.repo.join("packages/demo")).unwrap();
        for name in [
            "package.json",
            "pnpm-workspace.yaml",
            "pnpm-lock.yaml",
            "hardknock-fixture.json",
            "agent-script.sh",
            "test.sh",
            "packages/demo/package.json",
        ] {
            fs::copy(source.join(name), fixture.repo.join(name)).unwrap();
        }
        git(&fixture.repo, &["add", "."]);
        git(
            &fixture.repo,
            &["commit", "-m", "deterministic pnpm fixture"],
        );
        fixture
    }

    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("fixture repo");
        let home = temp.path().join("data");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Hardknock Test"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        fs::write(repo.join("tracked.txt"), "original\n").unwrap();
        fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fixture starting state"]);
        Self { temp, repo, home }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hardknock"));
        command
            .env("HARDKNOCK_HOME", &self.home)
            .env("RUST_LOG", "error")
            .arg("--repo")
            .arg(&self.repo);
        command
    }

    pub fn cli(&self, args: &[&str], expected: i32) -> Value {
        let output = self.command().arg("--json").args(args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn assert_source_unchanged(&self) {
        assert_eq!(
            fs::read_to_string(self.repo.join("tracked.txt")).unwrap(),
            "original\n"
        );
        assert!(
            git(&self.repo, &["status", "--porcelain"])
                .stdout
                .is_empty()
        );
        assert_eq!(
            String::from_utf8(git(&self.repo, &["worktree", "list", "--porcelain"]).stdout)
                .unwrap()
                .lines()
                .filter(|l| l.starts_with("worktree "))
                .count(),
            1
        );
    }
}
