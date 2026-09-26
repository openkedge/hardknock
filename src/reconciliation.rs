// SPDX-License-Identifier: Apache-2.0

//! Conservative recovery of resources left behind by an interrupted process.

use crate::{
    Error, Result,
    capability::{
        ContainerRealityProvider, ContainerRuntime, ContainerRuntimeLifecycle,
        ContainerRuntimeMetadata,
    },
    core::{RealityId, RealityStatus},
    curriculum::{CurriculumQuery, CurriculumStatus, GoalStatus},
    dojo::{GitRealityProvider, RealityProvider},
    effects::EffectManager,
    experimentation::ExperimentStatus,
    store::{CapabilityStore, CurriculumStore, ExperimentStore, Store},
};
use nix::unistd::geteuid;
use serde::Serialize;
use std::{
    fs::{self, FileType, OpenOptions},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReconciliationReport {
    pub discarded_realities: Vec<RealityId>,
    pub skipped_active_realities: Vec<RealityId>,
    pub failed_realities: Vec<ReconciliationFailure>,
    pub removed_runtime_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconciliationFailure {
    pub reality_id: RealityId,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InterruptedWorkReport {
    pub failed_experiments: Vec<crate::core::ExperimentId>,
    pub partial_curricula: Vec<crate::core::CurriculumId>,
}

fn intervention(message: impl Into<String>) -> Error {
    Error::Intervention(message.into())
}

/// Remove only Bridge-owned runtime endpoints after the caller has acquired the
/// exclusive Bridge lock. Persistent capability and credential files are left
/// for their owning Reality cleanup path.
pub fn reconcile_stale_bridge_runtime(home: &Path) -> Result<Vec<PathBuf>> {
    let run = private_directory(&home.join("run"))?;
    let mut removed = Vec::new();
    remove_runtime_entry(
        &run.join("hardknock.sock"),
        Some(FileType::is_socket),
        &mut removed,
    )?;
    for name in ["bridge-token", "bridge-endpoint.json"] {
        remove_runtime_entry(&run.join(name), None, &mut removed)?;
    }

    let realities = run.join("realities");
    let metadata = match fs::symlink_metadata(&realities) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(removed),
        Err(error) => return Err(error.into()),
    };
    validate_control_root_metadata(&realities, &metadata)?;
    normalize_control_root(&realities)?;
    for entry in fs::read_dir(&realities)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        validate_directory_metadata(&path, &metadata, 0o755)?;
        remove_runtime_entry(
            &path.join("bridge.sock"),
            Some(FileType::is_socket),
            &mut removed,
        )?;
        if fs::read_dir(&path)?.next().is_none() {
            fs::remove_dir(&path)?;
            removed.push(path);
        }
    }
    if fs::read_dir(&realities)?.next().is_none() {
        fs::remove_dir(&realities)?;
        removed.push(realities);
    }
    Ok(removed)
}

fn private_directory(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    validate_directory_metadata(path, &metadata, 0o700)?;
    Ok(path.to_path_buf())
}

fn validate_directory_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    expected_mode: u32,
) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != expected_mode
    {
        return Err(intervention(format!(
            "Refusing unsafe runtime directory during reconciliation: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_control_root_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || !matches!(mode, 0o700 | 0o755)
    {
        return Err(intervention(format!(
            "Refusing unsafe runtime directory during reconciliation: {}",
            path.display()
        )));
    }
    Ok(())
}

fn normalize_control_root(path: &Path) -> Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = directory.metadata()?;
    validate_control_root_metadata(path, &opened)?;
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    let current = fs::symlink_metadata(path)?;
    if current.dev() != opened.dev() || current.ino() != opened.ino() {
        return Err(intervention(format!(
            "Reality control root changed while being normalized: {}",
            path.display()
        )));
    }
    validate_directory_metadata(path, &current, 0o700)
}

/// Create or normalize the host-side control directory mounted read-only into
/// one container Reality. The shared root remains private; the selected child
/// is traversable by the container's unprivileged user.
pub(crate) fn ensure_reality_control_directory(
    home: &Path,
    reality_id: &RealityId,
) -> Result<PathBuf> {
    let run = private_directory(&home.join("run"))?;
    let root = run.join("realities");
    ensure_owned_directory(&root, 0o700)?;
    let directory = root.join(reality_id.to_string());
    ensure_owned_directory(&directory, 0o755)?;
    Ok(directory)
}

fn ensure_owned_directory(path: &Path, mode: u32) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = directory.metadata()?;
    let observed_mode = opened.permissions().mode() & 0o777;
    if !opened.is_dir() || opened.uid() != geteuid().as_raw() || observed_mode & 0o022 != 0 {
        return Err(intervention(format!(
            "Refusing unsafe Reality control directory: {}",
            path.display()
        )));
    }
    directory.set_permissions(fs::Permissions::from_mode(mode))?;
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
    {
        return Err(intervention(format!(
            "Reality control directory changed while being normalized: {}",
            path.display()
        )));
    }
    validate_directory_metadata(path, &current, mode)
}

fn remove_runtime_entry(
    path: &Path,
    expected_type: Option<fn(&FileType) -> bool>,
    removed: &mut Vec<PathBuf>,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let file_type = metadata.file_type();
    let expected = expected_type.map_or_else(
        || metadata.is_file(),
        |expected_type| expected_type(&file_type),
    );
    if file_type.is_symlink()
        || !expected
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(intervention(format!(
            "Refusing unsafe runtime path during reconciliation: {}",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    removed.push(path.to_path_buf());
    Ok(())
}

/// Discard unlocked automatic-run Realities and interrupted container
/// creations. Complete manual Realities and active runs are never selected.
/// Each failure remains visible and does not prevent safe cleanup of another
/// independent Reality.
pub fn reconcile_ephemeral_realities(store: &Store) -> Result<ReconciliationReport> {
    reconcile_ephemeral_realities_with_pre_lock(store, |_, _| Ok(()))
}

fn container_runtime_metadata(
    store: &Store,
    reality_id: &RealityId,
) -> Result<Option<ContainerRuntimeMetadata>> {
    match store.provider_runtime(reality_id) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn interrupted_container_creation(
    provider: &str,
    metadata: Option<&ContainerRuntimeMetadata>,
) -> bool {
    metadata.is_some_and(|metadata| metadata.lifecycle == ContainerRuntimeLifecycle::Pending)
        || (provider == "container" && metadata.is_none())
}

fn reconciliation_candidate(
    reality: &crate::core::Reality,
    metadata: Option<&ContainerRuntimeMetadata>,
) -> bool {
    reality.status != RealityStatus::Discarded
        && (reality.ephemeral
            || interrupted_container_creation(&reality.execution_boundary.provider, metadata))
}

fn reconcile_ephemeral_realities_with_pre_lock<F>(
    store: &Store,
    mut before_lock: F,
) -> Result<ReconciliationReport>
where
    F: FnMut(&Store, &RealityId) -> Result<()>,
{
    let mut report = ReconciliationReport::default();
    for summary in store.realities()? {
        if summary.status == RealityStatus::Discarded {
            continue;
        }
        let observed_runtime = container_runtime_metadata(store, &summary.id)?;
        if !reconciliation_candidate(&summary, observed_runtime.as_ref()) {
            continue;
        }
        before_lock(store, &summary.id)?;
        let _lease = match store.lock_reality(&summary.id) {
            Ok(lease) => lease,
            Err(Error::Intervention(_)) => {
                report.skipped_active_realities.push(summary.id);
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut reality = store.reality(&summary.id)?;
        let container_runtime = container_runtime_metadata(store, &summary.id)?;
        if !reconciliation_candidate(&reality, container_runtime.as_ref()) {
            continue;
        }
        let result = (|| -> Result<()> {
            EffectManager::new(store)?.discard_reality(&reality.id)?;
            if let Some(metadata) = &container_runtime {
                let runtime = ContainerRuntime::named(&metadata.runtime)?;
                ContainerRealityProvider::with_runtime(store, runtime, &metadata.image)?
                    .discard(&mut reality)?;
            } else {
                GitRealityProvider::new(store).discard(&mut reality)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => report.discarded_realities.push(reality.id),
            Err(error) => report.failed_realities.push(ReconciliationFailure {
                reality_id: reality.id,
                reason: error.to_string(),
            }),
        }
    }
    Ok(report)
}

/// Persist terminal truth for Bridge-owned work that cannot be resumed after a
/// process restart. Planned curricula remain planned because planning and
/// explicit start are separate actions.
pub fn reconcile_interrupted_bridge_work(store: &Store) -> Result<InterruptedWorkReport> {
    const REASON: &str = "Bridge restarted before completion; partial evidence is retained and the work cannot be resumed automatically";
    let mut report = InterruptedWorkReport::default();

    for listed in ExperimentStore::list(store, None)? {
        let bridge_owned = listed.request.session_id.starts_with("hk-s-");
        if listed.status != ExperimentStatus::Running
            && !(bridge_owned && listed.status == ExperimentStatus::Accepted)
        {
            continue;
        }
        let _lease = match store.lock_experiment(&listed.id) {
            Ok(lease) => lease,
            Err(Error::Intervention(message))
                if message.contains("is in use by another Hardknock process") =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut experiment = store.strategy_experiment(&listed.id)?;
        let bridge_owned = experiment.request.session_id.starts_with("hk-s-");
        if experiment.status != ExperimentStatus::Running
            && !(bridge_owned && experiment.status == ExperimentStatus::Accepted)
        {
            continue;
        }
        experiment.status = ExperimentStatus::Failed;
        experiment.failure = Some(REASON.into());
        experiment
            .notices
            .push("Create a new experiment request to gather fresh evidence.".into());
        ExperimentStore::update_status(store, &experiment)?;
        if bridge_owned {
            store.bridge_event(
                &experiment.request.session_id,
                "experiment_interrupted",
                &serde_json::json!({"experiment_id":experiment.id,"reason":REASON}),
            )?;
        }
        report.failed_experiments.push(experiment.id);
    }

    for mut curriculum in CurriculumStore::list(store, CurriculumQuery::default())? {
        if curriculum.status != CurriculumStatus::Running
            || curriculum
                .session_id
                .as_deref()
                .is_none_or(|session| !session.starts_with("hk-s-"))
        {
            continue;
        }
        curriculum.status = CurriculumStatus::PartiallyCompleted;
        curriculum.stop_reason = Some(REASON.into());
        curriculum.revision = curriculum.revision.saturating_add(1);
        curriculum.updated_at = chrono::Utc::now();
        for goal in &mut curriculum.goals {
            if goal.status == GoalStatus::Running {
                goal.status = GoalStatus::Inconclusive;
            }
        }
        for trial in &mut curriculum.trials {
            if trial.status == GoalStatus::Running {
                trial.status = GoalStatus::Inconclusive;
            }
        }
        CurriculumStore::update(store, &curriculum)?;
        report.partial_curricula.push(curriculum.id);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::{fs::symlink, net::UnixListener},
        path::{Path, PathBuf},
        process::Command,
    };

    fn git(repo: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {arguments:?}");
    }

    fn manual_reality(temp: &tempfile::TempDir) -> (Store, crate::core::Reality) {
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Hardknock Test"]);
        git(
            &repo,
            &["config", "user.email", "hardknock@example.invalid"],
        );
        fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "base",
            ],
        );

        let store = Store::open(&temp.path().join("home")).unwrap();
        let state = crate::dojo::capture_state(&repo).unwrap();
        let reality = GitRealityProvider::new(&store).create(&state).unwrap();
        (store, reality)
    }

    fn fake_runtime(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let runtime = temp.path().join("fake-runtime");
        let runtime_log = temp.path().join("runtime.log");
        fs::write(
            &runtime,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
                runtime_log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        (runtime, runtime_log)
    }

    fn runtime_metadata(runtime: &Path) -> ContainerRuntimeMetadata {
        ContainerRuntimeMetadata {
            runtime: runtime.display().to_string(),
            container_id: "hk-reality-pending".into(),
            container_name: "hk-reality-pending".into(),
            image: "fixture@sha256:0123456789abcdef".into(),
            image_digest: "unresolved".into(),
            network_name: None,
            attached_fixture_containers: Vec::new(),
            lifecycle: ContainerRuntimeLifecycle::Pending,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn stale_bridge_runtime_removes_only_owned_endpoints() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let run = store.home.join("run");
        fs::write(run.join("bridge-token"), "secret").unwrap();
        fs::write(run.join("bridge-endpoint.json"), "{}").unwrap();
        let _socket = UnixListener::bind(run.join("hardknock.sock")).unwrap();
        let relay = run.join("realities/reality-test");
        fs::create_dir_all(&relay).unwrap();
        fs::set_permissions(run.join("realities"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&relay, fs::Permissions::from_mode(0o755)).unwrap();
        let _relay_socket = UnixListener::bind(relay.join("bridge.sock")).unwrap();

        let removed = reconcile_stale_bridge_runtime(&store.home).unwrap();
        for path in [
            run.join("bridge-token"),
            run.join("bridge-endpoint.json"),
            run.join("hardknock.sock"),
            relay.join("bridge.sock"),
            relay,
            run.join("realities"),
        ] {
            assert!(removed.contains(&path), "missing {}", path.display());
            assert!(!path.exists(), "retained {}", path.display());
        }
    }

    #[test]
    fn stale_bridge_runtime_refuses_symlinks_and_preserves_targets() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, "retain").unwrap();
        symlink(&outside, store.home.join("run/bridge-token")).unwrap();

        let error = reconcile_stale_bridge_runtime(&store.home)
            .expect_err("runtime symlink must be rejected");
        assert!(error.to_string().contains("unsafe runtime path"));
        assert_eq!(fs::read_to_string(outside).unwrap(), "retain");
    }

    #[test]
    fn missing_reality_runtime_tree_is_valid() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        assert!(
            reconcile_stale_bridge_runtime(Path::new(&store.home))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn stale_control_root_is_normalized_without_removing_persistent_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let root = store.home.join("run/realities");
        let control = root.join("reality-test");
        fs::create_dir_all(&control).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&control, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(control.join("capability-token.json"), "{}").unwrap();

        assert!(
            reconcile_stale_bridge_runtime(&store.home)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&control).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(control.join("capability-token.json").exists());
    }

    #[test]
    fn reality_control_directory_rejects_writable_existing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let id = RealityId::new();
        let root = store.home.join("run/realities");
        let control = root.join(id.to_string());
        fs::create_dir_all(&control).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&control, fs::Permissions::from_mode(0o777)).unwrap();

        let error = ensure_reality_control_directory(&store.home, &id)
            .expect_err("writable control directory must fail closed");
        assert!(error.to_string().contains("unsafe Reality control"));
    }

    #[test]
    fn pending_container_runtime_is_reconciled_before_provider_marker_is_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let (store, reality) = manual_reality(&temp);
        let (runtime, runtime_log) = fake_runtime(&temp);
        assert_eq!(reality.execution_boundary.provider, "git-worktree");
        store
            .put_provider_runtime(&reality.id, "container", &runtime_metadata(&runtime))
            .unwrap();

        let report = reconcile_ephemeral_realities(&store).unwrap();
        assert_eq!(report.discarded_realities, vec![reality.id.clone()]);
        assert!(report.failed_realities.is_empty());
        assert!(!reality.root.exists());
        assert!(
            fs::read_to_string(runtime_log)
                .unwrap()
                .contains("rm --force hk-reality-pending")
        );
        assert_eq!(
            store.reality(&reality.id).unwrap().status,
            RealityStatus::Discarded
        );
    }

    #[test]
    fn completed_container_is_reloaded_after_lease_and_not_discarded() {
        let temp = tempfile::tempdir().unwrap();
        let (store, mut reality) = manual_reality(&temp);
        let (runtime, runtime_log) = fake_runtime(&temp);
        reality.execution_boundary.provider = "container".into();
        store.update_reality(&reality).unwrap();
        store
            .put_provider_runtime(&reality.id, "container", &runtime_metadata(&runtime))
            .unwrap();

        let expected_id = reality.id.clone();
        let mut completed_between_scan_and_lock = false;
        let report = reconcile_ephemeral_realities_with_pre_lock(&store, |store, reality_id| {
            assert_eq!(reality_id, &expected_id);
            completed_between_scan_and_lock = true;
            let mut completed = store.reality(reality_id)?;
            completed.status = RealityStatus::Completed;
            completed.execution_boundary.image_digest = Some("sha256:0123456789abcdef".into());
            store.update_reality(&completed)?;
            let mut metadata =
                container_runtime_metadata(store, reality_id)?.expect("pending runtime metadata");
            metadata.lifecycle = ContainerRuntimeLifecycle::Ready;
            metadata.image_digest = "sha256:0123456789abcdef".into();
            store.put_provider_runtime(reality_id, "container", &metadata)
        })
        .unwrap();

        assert!(completed_between_scan_and_lock);
        assert!(report.discarded_realities.is_empty());
        assert!(report.failed_realities.is_empty());
        assert!(report.skipped_active_realities.is_empty());
        assert!(reality.root.exists());
        assert!(!runtime_log.exists());
        let preserved = store.reality(&reality.id).unwrap();
        assert_eq!(preserved.status, RealityStatus::Completed);
        assert_eq!(
            container_runtime_metadata(&store, &reality.id)
                .unwrap()
                .unwrap()
                .lifecycle,
            ContainerRuntimeLifecycle::Ready
        );
    }
}
