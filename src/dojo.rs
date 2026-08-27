// SPDX-License-Identifier: Apache-2.0

use std::{env, ffi::OsStr, fs, path::Path, process::Command};

use chrono::Utc;

use crate::{
    Error, Result,
    core::{Reality, RealityId, RealityStatus, StateRef},
    store::Store,
};

pub trait RealityProvider {
    fn create(&self, state: &StateRef) -> Result<Reality>;
    /// Fork the recorded starting state, not the parent's current dirty files.
    fn fork(&self, reality: &Reality) -> Result<Reality>;
    fn diff(&self, reality: &Reality) -> Result<Vec<u8>>;
    fn discard(&self, reality: &mut Reality) -> Result<()>;
}

pub struct GitRealityProvider<'a> {
    store: &'a Store,
}

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    // The caller's Git routing variables must not redirect our bookkeeping.
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(repo);
    command
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }
    Ok(result.stdout)
}

fn text(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|_| Error::InvalidInput("Git metadata must be UTF-8 in this version".into()))
}

pub fn capture_state(repo: &Path) -> Result<StateRef> {
    let root = text(git(repo).args(["rev-parse", "--show-toplevel"]))
        .map_err(|_| Error::Intervention("Select a non-bare Git repository with --repo.".into()))?;
    let repo_path = Path::new(&root).canonicalize()?;
    let git_commit = text(git(&repo_path).args(["rev-parse", "--verify", "HEAD^{commit}"]))
        .map_err(|_| {
            Error::Intervention(
                "The repository has no commit. Commit a starting snapshot first.".into(),
            )
        })?;
    if !output(git(&repo_path).args(["status", "--porcelain=v1", "--untracked-files=all"]))?
        .is_empty()
    {
        return Err(Error::Intervention("The repository must be clean. Commit or move staged, unstaged, and untracked changes before creating a Reality; Hardknock will not stash them.".into()));
    }
    let entries = output(git(&repo_path).args(["ls-files", "--stage", "-z"]))?;
    if entries
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        return Err(Error::Intervention(
            "Submodule snapshots are not supported by the V0.1 worktree backend.".into(),
        ));
    }
    let tree_hash = text(git(&repo_path).args(["rev-parse", &format!("{git_commit}^{{tree}}")]))?;
    Ok(StateRef {
        repo_path,
        git_commit,
        tree_hash,
    })
}

impl<'a> GitRealityProvider<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn create_for_run(&self, state: &StateRef, keep: bool) -> Result<(Reality, fs::File)> {
        self.create_with_options(state, None, !keep)
    }

    pub fn verify_start(&self, reality: &Reality) -> Result<()> {
        let head = text(git(&reality.root).args(["rev-parse", "HEAD^{commit}"]))?;
        if head != reality.starting_state.git_commit || !self.diff(reality)?.is_empty() {
            return Err(Error::Intervention("Counterfactual experiment cannot guarantee equivalent starting state: worktree does not match the recorded commit/tree.".into()));
        }
        Ok(())
    }

    fn create_with_options(
        &self,
        state: &StateRef,
        parent: Option<RealityId>,
        ephemeral: bool,
    ) -> Result<(Reality, fs::File)> {
        for oid in [&state.git_commit, &state.tree_hash] {
            if !matches!(oid.len(), 40 | 64)
                || !oid
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(Error::InvalidInput("StateRef requires full lowercase Git object hashes, not branch names or revision expressions".into()));
            }
        }
        if state.repo_path.canonicalize()? != state.repo_path {
            return Err(Error::InvalidInput(
                "StateRef requires a canonical absolute repository path".into(),
            ));
        }
        if self.store.home.starts_with(&state.repo_path) {
            return Err(Error::Intervention(
                "HARDKNOCK_HOME must be outside the source repository.".into(),
            ));
        }
        let actual_tree = text(
            git(&state.repo_path).args(["rev-parse", &format!("{}^{{tree}}", state.git_commit)]),
        )?;
        if actual_tree != state.tree_hash {
            return Err(Error::InvalidInput(
                "Starting commit does not match its recorded tree hash".into(),
            ));
        }
        let id = RealityId::new();
        let guard = self.store.lock_reality(&id)?;
        let mut reality = Reality {
            root: self.store.home.join("realities").join(id.to_string()),
            id,
            parent,
            starting_state: state.clone(),
            created_at: Utc::now(),
            status: RealityStatus::Created,
            ephemeral,
        };
        // Record intent before Git creates state, so an interrupted creation is inspectable.
        self.store.insert_reality(&reality)?;
        let result = output(
            git(&state.repo_path)
                .args(["worktree", "add", "--detach", "--"])
                .arg(&reality.root)
                .arg(&state.git_commit),
        );
        if let Err(primary) = result {
            reality.status = RealityStatus::Failed;
            self.store.update_reality(&reality)?;
            return match self.discard(&mut reality) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        tracing::debug!(reality_id = %reality.id, "Created detached worktree");
        Ok((reality, guard))
    }

    fn validate_path(&self, reality: &Reality) -> Result<()> {
        let expected = self
            .store
            .home
            .join("realities")
            .join(reality.id.to_string());
        if reality.root != expected {
            return Err(Error::Intervention(
                "Refusing an unmanaged Reality path.".into(),
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(&reality.root) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::Intervention("Reality root was replaced with a symlink or non-directory; refusing to follow it.".into()));
            }
            if reality.root.canonicalize()? != expected {
                return Err(Error::Intervention(
                    "Reality path resolves outside its managed directory.".into(),
                ));
            }
        }
        Ok(())
    }

    fn registered(&self, reality: &Reality) -> Result<bool> {
        let listing = output(git(&reality.starting_state.repo_path).args([
            "worktree",
            "list",
            "--porcelain",
            "-z",
        ]))?;
        let expected = format!("worktree {}", reality.root.display());
        Ok(listing
            .split(|b| *b == 0)
            .any(|line| line == expected.as_bytes()))
    }
}

impl RealityProvider for GitRealityProvider<'_> {
    fn create(&self, state: &StateRef) -> Result<Reality> {
        self.create_with_options(state, None, false)
            .map(|(reality, _)| reality)
    }

    fn fork(&self, reality: &Reality) -> Result<Reality> {
        self.create_with_options(&reality.starting_state, Some(reality.id.clone()), false)
            .map(|(reality, _)| reality)
    }

    fn diff(&self, reality: &Reality) -> Result<Vec<u8>> {
        self.validate_path(reality)?;
        if reality.status == RealityStatus::Discarded
            || !reality.root.exists()
            || !self.registered(reality)?
        {
            return Err(Error::Intervention(
                "Reality is absent or discarded; use its saved execution diff artifact.".into(),
            ));
        }
        let common = text(git(&reality.root).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))?;
        let source_common = text(git(&reality.starting_state.repo_path).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))?;
        if Path::new(&common).canonicalize()? != Path::new(&source_common).canonicalize()? {
            return Err(Error::Intervention(
                "Reality Git metadata no longer refers to the starting repository.".into(),
            ));
        }
        // A private index includes untracked files without changing the agent's index.
        let scratch = tempfile::tempdir_in(self.store.home.join("artifacts"))?;
        let index = scratch.path().join("index");
        output(
            git(&reality.root)
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", &reality.starting_state.git_commit]),
        )?;
        output(
            git(&reality.root)
                .env("GIT_INDEX_FILE", &index)
                .args(["add", "-A", "--", "."]),
        )?;
        output(git(&reality.root).env("GIT_INDEX_FILE", &index).args([
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            &reality.starting_state.git_commit,
            "--",
        ]))
    }

    fn discard(&self, reality: &mut Reality) -> Result<()> {
        self.validate_path(reality)?;
        let registered = self.registered(reality)?;
        if registered {
            output(
                git(&reality.starting_state.repo_path)
                    .args(["worktree", "remove", "--force", "--"])
                    .arg(&reality.root),
            )?;
        } else if reality.root.exists() {
            return Err(Error::Intervention("Reality directory is not registered to its source repository; refusing to delete it.".into()));
        }
        reality.status = RealityStatus::Discarded;
        self.store.update_reality(reality)?;
        tracing::debug!(reality_id = %reality.id, "Discarded managed worktree");
        Ok(())
    }
}

/// Resolve a not-yet-created path without mutating its parents.
pub fn resolve_home(path: &Path) -> Result<std::path::PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    };
    if absolute.exists() {
        return Ok(absolute.canonicalize()?);
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| Error::InvalidInput("Invalid data directory".into()))?;
    let name = absolute
        .file_name()
        .filter(|s| *s != OsStr::new(".."))
        .ok_or_else(|| Error::InvalidInput("Invalid data directory".into()))?;
    Ok(resolve_home(parent)?.join(name))
}
