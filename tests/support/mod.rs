// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    ffi::OsStr,
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

pub fn copy_experience_artifacts(
    db: &rusqlite::Connection,
    source_schema: &str,
    home: &Path,
) -> BTreeMap<(String, String), String> {
    assert!(matches!(source_schema, "source" | "seed"));
    let artifacts = {
        let mut query = db
            .prepare(&format!(
                "SELECT experience_id,path,blake3,bytes,kind
                 FROM {source_schema}.experience_artifacts
                 ORDER BY experience_id,path"
            ))
            .unwrap();
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    let mut relocated = BTreeMap::new();
    for (experience_id, source, blake3, bytes, kind) in artifacts {
        let source_path = PathBuf::from(&source);
        let artifact_component = source_path
            .iter()
            .position(|component| component == OsStr::new("artifacts"))
            .unwrap_or_else(|| panic!("artifact path has no artifacts component: {source}"));
        let relative: PathBuf = source_path.iter().skip(artifact_component).collect();
        let destination = home.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(&source_path, &destination).unwrap();
        let destination = destination.canonicalize().unwrap();
        db.execute(
            "INSERT INTO experience_artifacts(experience_id,path,blake3,bytes,kind)
             VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                experience_id,
                destination.to_string_lossy().into_owned(),
                blake3,
                bytes,
                kind
            ],
        )
        .unwrap();
        relocated.insert(
            (experience_id, source),
            destination.to_string_lossy().into_owned(),
        );
    }
    relocated
}

pub fn copy_trial_artifacts(
    db: &rusqlite::Connection,
    source_schema: &str,
    relocated: &BTreeMap<(String, String), String>,
) {
    assert!(matches!(source_schema, "source" | "seed"));
    let artifacts = {
        let mut query = db
            .prepare(&format!(
                "SELECT trial_id,experience_id,path
                 FROM {source_schema}.trial_artifacts
                 ORDER BY trial_id,path"
            ))
            .unwrap();
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    for (trial_id, experience_id, source) in artifacts {
        let destination = relocated
            .get(&(experience_id.clone(), source.clone()))
            .unwrap_or_else(|| {
                panic!("trial artifact has no relocated experience artifact: {source}")
            });
        db.execute(
            "INSERT INTO trial_artifacts(trial_id,experience_id,path) VALUES(?1,?2,?3)",
            rusqlite::params![trial_id, experience_id, destination],
        )
        .unwrap();
    }
}

impl Fixture {
    pub fn pnpm() -> Self {
        Self::from_fixture("pnpm-workspace-conflict")
    }

    pub fn from_fixture(name: &str) -> Self {
        let fixture = Self::new();
        fn copy_tree(source: &Path, target: &Path) {
            for entry in fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let destination = target.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    fs::create_dir_all(&destination).unwrap();
                    copy_tree(&entry.path(), &destination);
                } else {
                    fs::copy(entry.path(), destination).unwrap();
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        copy_tree(&source, &fixture.repo);
        git(&fixture.repo, &["add", "."]);
        git(&fixture.repo, &["commit", "-m", "deterministic fixture"]);
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
