// SPDX-License-Identifier: Apache-2.0

//! Local, reversible changes. Command effects are explicit inputs to the runner,
//! never host PATH replacements or process-global environment changes.
use crate::{
    Error, Result,
    core::{PerturbationId, Reality, RealityStatus},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerturbationKind {
    EnvironmentVariable,
    FileMutation,
    CommandFailure,
    CommandDelay,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PerturbationParameters {
    EnvironmentVariable { key: String, value: String },
    FileMutation { path: PathBuf, content: String },
    CommandFailure { failures: u32, exit_code: u8 },
    CommandDelay { milliseconds: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Perturbation {
    pub id: PerturbationId,
    pub kind: PerturbationKind,
    pub parameters: PerturbationParameters,
    pub description: String,
}
impl Perturbation {
    pub fn new(parameters: PerturbationParameters) -> Self {
        let kind = match parameters {
            PerturbationParameters::EnvironmentVariable { .. } => {
                PerturbationKind::EnvironmentVariable
            }
            PerturbationParameters::FileMutation { .. } => PerturbationKind::FileMutation,
            PerturbationParameters::CommandFailure { .. } => PerturbationKind::CommandFailure,
            PerturbationParameters::CommandDelay { .. } => PerturbationKind::CommandDelay,
        };
        Self {
            id: PerturbationId::new(),
            kind,
            description: format!("Local deterministic {kind:?}"),
            parameters,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if Self::new(self.parameters.clone()).kind != self.kind {
            return Err(Error::InvalidInput(
                "Perturbation kind/parameters mismatch".into(),
            ));
        }
        match &self.parameters {
            PerturbationParameters::EnvironmentVariable { key, value } => {
                validate_environment(key, value)?
            }
            PerturbationParameters::FileMutation { path, content } => {
                validate_relative(path)?;
                if content.len() > 65536 {
                    return Err(Error::InvalidInput(
                        "File perturbations are limited to 64 KiB".into(),
                    ));
                }
            }
            PerturbationParameters::CommandFailure {
                failures,
                exit_code,
            } if *failures == 0 || *failures > 100 || *exit_code == 0 => {
                return Err(Error::InvalidInput(
                    "Command failure requires 1..100 failures and a nonzero exit code".into(),
                ));
            }
            PerturbationParameters::CommandDelay { milliseconds } if *milliseconds > 60_000 => {
                return Err(Error::InvalidInput("Delay is limited to 60000ms".into()));
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn validate_environment(key: &str, value: &str) -> Result<()> {
    let valid = !key.is_empty()
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()));
    // Preserve the controlled runner and avoid loading code through environment hooks.
    if !valid
        || ["HOME", "PWD", "PATH", "SHELL", "ENV", "BASH_ENV", "IFS"].contains(&key)
        || [
            "HK_ATTEMPT",
            "HK_DELAY_MS",
            "HK_FAILURES",
            "HK_FAILURE_EXIT",
        ]
        .contains(&key)
        || key.starts_with("LD_")
        || key.starts_with("DYLD_")
        || key.starts_with("GIT_")
        || value.contains('\0')
        || value.len() > 4096
    {
        return Err(Error::InvalidInput(
            "Invalid or reserved perturbation environment variable".into(),
        ));
    }
    Ok(())
}
fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        return Err(Error::InvalidInput(
            "Perturbation path must be relative, without traversal or .git components".into(),
        ));
    }
    Ok(())
}
pub fn scoped_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative(relative)?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Intervention(
                    "Perturbation cannot traverse symlinks".into(),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && path == root.join(relative) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}

pub trait PerturbationProvider {
    fn apply(&self, reality: &Reality, perturbation: &Perturbation) -> Result<PerturbationHandle>;
    fn remove(&self, handle: PerturbationHandle) -> Result<()>;
}
pub struct LocalPerturbationProvider;
pub struct PerturbationHandle {
    root: PathBuf,
    restore: Option<(PathBuf, Option<Vec<u8>>)>,
    pub environment: BTreeMap<String, String>,
    pub command_failure: Option<(u32, u8)>,
    pub command_delay_ms: u64,
}
impl PerturbationHandle {
    fn restore(&mut self) -> Result<()> {
        if let Some((relative, bytes)) = &self.restore {
            let path = scoped_path(&self.root, relative)?;
            if fs::symlink_metadata(&path).is_ok_and(|m| m.nlink() > 1) {
                return Err(Error::Intervention(
                    "Cannot restore a mutation through a hard link".into(),
                ));
            }
            match bytes {
                Some(bytes) => fs::write(path, bytes)?,
                None => match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                },
            }
        }
        self.restore = None;
        Ok(())
    }
}
impl Drop for PerturbationHandle {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::error!(%error, "Perturbation cleanup failed; Reality cleanup is still required");
        }
    }
}
impl PerturbationProvider for LocalPerturbationProvider {
    fn apply(&self, reality: &Reality, perturbation: &Perturbation) -> Result<PerturbationHandle> {
        perturbation.validate()?;
        let root = reality.root.canonicalize()?;
        // Worktree marker must be a regular file. Never accept the source checkout.
        if root == reality.starting_state.repo_path.canonicalize()?
            || !matches!(
                reality.status,
                RealityStatus::Created | RealityStatus::Running
            )
            || !fs::symlink_metadata(root.join(".git"))
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        {
            return Err(Error::Intervention(
                "Perturbations require a live detached Reality, not the source repository".into(),
            ));
        }
        let mut handle = PerturbationHandle {
            root: root.clone(),
            restore: None,
            environment: BTreeMap::new(),
            command_failure: None,
            command_delay_ms: 0,
        };
        match &perturbation.parameters {
            PerturbationParameters::EnvironmentVariable { key, value } => {
                handle.environment.insert(key.clone(), value.clone());
            }
            PerturbationParameters::FileMutation { path, content } => {
                let target = scoped_path(&root, path)?;
                let backup = match fs::metadata(&target) {
                    Ok(meta) if meta.is_file() && meta.len() <= 65536 && meta.nlink() == 1 => {
                        Some(fs::read(&target)?)
                    }
                    Ok(_) => {
                        return Err(Error::InvalidInput(
                            "Mutation target must be a regular file of at most 64 KiB".into(),
                        ));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.into()),
                };
                handle.restore = Some((path.clone(), backup));
                fs::write(target, content)?;
            }
            PerturbationParameters::CommandFailure {
                failures,
                exit_code,
            } => handle.command_failure = Some((*failures, *exit_code)),
            PerturbationParameters::CommandDelay { milliseconds } => {
                handle.command_delay_ms = *milliseconds
            }
        }
        Ok(handle)
    }
    fn remove(&self, mut handle: PerturbationHandle) -> Result<()> {
        handle.restore()
    }
}

/// Ordered handles unwind in reverse, including partial application failures.
#[derive(Default)]
pub struct AppliedPerturbations(pub Vec<PerturbationHandle>);
impl AppliedPerturbations {
    pub fn remove(&mut self) -> Result<()> {
        let mut failure = None;
        while let Some(handle) = self.0.pop() {
            if let Err(error) = LocalPerturbationProvider.remove(handle) {
                failure = Some(error);
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
impl Drop for AppliedPerturbations {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}
