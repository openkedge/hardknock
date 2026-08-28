// SPDX-License-Identifier: Apache-2.0
use super::FixtureKind;
use crate::{Error, Result, core::StateRef, dojo::capture_state, store::Store};
use fs2::FileExt;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

pub const RUNTIME_VERSION: &str = "local-resilience-v1";
const FILES: &[(&str, &str)] = &[
    (
        "operation.sh",
        include_str!("../../fixtures/retry-resilience/operation.sh"),
    ),
    (
        "replan.sh",
        include_str!("../../fixtures/retry-resilience/replan.sh"),
    ),
    (
        "test.sh",
        include_str!("../../fixtures/retry-resilience/test.sh"),
    ),
    (
        "refresh-token.sh",
        include_str!("../../fixtures/retry-resilience/refresh-token.sh"),
    ),
    (
        "read-state.sh",
        include_str!("../../fixtures/retry-resilience/read-state.sh"),
    ),
    ("generation", "1\n"),
    ("plan-generation", "1\n"),
    ("token", "VALID_TOKEN\n"),
];
/// Persistent, versioned source for replay. Never copy or edit the user's checkout.
pub fn materialize(store: &Store, kind: FixtureKind) -> Result<StateRef> {
    let mut files: Vec<(String, String)> = FILES
        .iter()
        .map(|(p, b)| ((*p).into(), (*b).into()))
        .collect();
    if matches!(
        kind,
        FixtureKind::SkillHardening | FixtureKind::SkillHardeningTransfer
    ) {
        files = hardening_files(kind);
    }
    files.push(("fixture-kind".into(), format!("{}\n", kind.name())));
    files.push((
        "hardknock-fixture.json".into(),
        format!(
            "{{\"kind\":\"{}\",\"version\":1,\"runtime\":\"{RUNTIME_VERSION}\"}}\n",
            kind.name()
        ),
    ));
    let hash = blake3::hash(&serde_json::to_vec(&files)?)
        .to_hex()
        .to_string();
    let root = store
        .home
        .join("fixtures")
        .join(format!("{}-{}", kind.name(), &hash[..16]));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(
            store
                .home
                .join("locks")
                .join(format!("fixture-{}.lock", kind.name())),
        )?;
    lock.lock_exclusive()?;
    if !root.exists() {
        let tmp = tempfile::tempdir_in(store.home.join("fixtures"))?;
        for (path, body) in &files {
            fs::write(tmp.path().join(path), body)?;
            if path.ends_with(".sh") {
                fs::set_permissions(tmp.path().join(path), fs::Permissions::from_mode(0o755))?;
            }
        }
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Hardknock",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-qm",
                "Local deterministic resilience fixture",
            ],
        ] {
            let output = Command::new("git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", tmp.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(tmp.path())
                .output()?;
            if !output.status.success() {
                return Err(Error::InvalidInput(format!(
                    "Cannot initialize bundled fixture: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }
        fs::rename(tmp.path(), &root)?;
    }
    for (path, body) in &files {
        let path = root.join(path);
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            || fs::read(path)? != body.as_bytes()
        {
            return Err(Error::Intervention(
                "Bundled fixture source changed; refusing unverified replay".into(),
            ));
        }
    }
    capture_state(&root)
}

pub fn hardening_files(kind: FixtureKind) -> Vec<(String, String)> {
    let generation = if kind == FixtureKind::SkillHardeningTransfer {
        "7\n"
    } else {
        "1\n"
    };
    [
        (
            "operation.sh",
            include_str!("../../fixtures/skill-hardening/operation.sh"),
        ),
        (
            "replan.sh",
            include_str!("../../fixtures/skill-hardening/replan.sh"),
        ),
        (
            "test.sh",
            include_str!("../../fixtures/skill-hardening/test.sh"),
        ),
        (
            "refresh-token.sh",
            include_str!("../../fixtures/skill-hardening/refresh-token.sh"),
        ),
        (
            "read-state.sh",
            include_str!("../../fixtures/skill-hardening/read-state.sh"),
        ),
        ("generation", generation),
        ("plan-generation", generation),
        ("input-generation", generation),
        ("dependency", "up\n"),
        ("token", "VALID_TOKEN\n"),
    ]
    .iter()
    .map(|(p, b)| ((*p).into(), (*b).into()))
    .collect()
}
