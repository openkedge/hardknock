// SPDX-License-Identifier: Apache-2.0

pub mod service;
pub(crate) mod transaction;

use crate::{
    Error, Result,
    cli::{
        integrations::AdapterCommand,
        setup::{SetupAgent, SetupArgs, SetupMode, UninstallArgs},
    },
    integrations::install::{self, IntegrationPlan},
    storage,
    store::Store,
};
use chrono::Utc;
use fs2::FileExt;
use nix::unistd::geteuid;
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, fchmod, fstat, fsync, open, openat, statat,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use self::{
    service::{RequestedService, ServicePlan, ServiceReport},
    transaction::{
        DirectoryReceipt, InitialHome, Journal, MutationObserver, PreparedRemoval, PreparedWrite,
        PrivateCleanup, QuarantineRecovery, QuarantineState, SetupLock, SnapshotSet, WriteReceipt,
        ensure_directory_tree, file_identity, unfinished,
    },
};

const SETUP_MANIFEST_FORMAT: &str = "hardknock-setup-manifest-v1";
const SETUP_RESULT_SCHEMA: &str = "hardknock-setup-result-v1";
const SETUP_MANIFEST_RELATIVE: &str = "setup/manifest-v1.json";
const MAX_SETUP_MANIFEST_BYTES: u64 = 1024 * 1024;
const BRIDGE_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);
const BRIDGE_QUIESCENCE_POLL: Duration = Duration::from_millis(25);
const TRANSACTION_DIRECTORIES: &[&str] = &[
    "artifacts/transient",
    "setup/transactions",
    "artifacts",
    "backups",
    "realities",
    "logs",
    "locks",
    "fixtures",
    "run",
    "integrations",
    "identity",
    "federation",
    "effects",
    "tools",
    "setup",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Setup,
    Upgrade,
    Repair,
    Uninstall,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Upgrade => "upgrade",
            Self::Repair => "repair",
            Self::Uninstall => "uninstall",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDetection {
    pub name: String,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostDetection {
    pub operating_system: String,
    pub architecture: String,
    pub tools: Vec<ToolDetection>,
    pub agents: Vec<ToolDetection>,
    pub service_manager: service::ServiceManager,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlannedChange {
    pub id: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupPlan {
    pub operation: Operation,
    pub home: PathBuf,
    pub executable: PathBuf,
    pub mode: String,
    pub agents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration: Option<storage::MigrationPlan>,
    pub detection: HostDetection,
    pub changes: Vec<PlannedChange>,
    pub integrations: Vec<Value>,
    pub service: ServicePlan,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupResult {
    pub schema: &'static str,
    pub operation: Operation,
    pub dry_run: bool,
    pub changed: bool,
    pub ready: bool,
    pub exit_code: u8,
    pub home: PathBuf,
    pub executable: PathBuf,
    pub plan: SetupPlan,
    pub applied: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_point: Option<storage::ManagedRecoveryPointReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupManifest {
    format: String,
    package_version: String,
    home: PathBuf,
    executable: PathBuf,
    mode: String,
    agents: Vec<String>,
    service_target: Option<PathBuf>,
    service_content_blake3: Option<String>,
    updated_at: chrono::DateTime<Utc>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Intervention(message.into())
}

fn setup_manifest_path(home: &Path) -> PathBuf {
    home.join(SETUP_MANIFEST_RELATIVE)
}

fn data_removal_quarantine(home: &Path) -> Result<PathBuf> {
    if !home.is_absolute() {
        return Err(invalid(
            "Hardknock home must be absolute for managed data removal",
        ));
    }
    let parent = home
        .parent()
        .ok_or_else(|| invalid("Hardknock home has no parent for managed data removal"))?;
    let identity = blake3::hash(home.as_os_str().as_bytes()).to_hex();
    let identity = &identity[..24];
    Ok(parent.join(format!(".hardknock-remove-{identity}.quarantine")))
}

fn path_exists_without_following(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn detect_executable(name: &str) -> ToolDetection {
    let path = install::find_executable(name);
    ToolDetection {
        name: name.into(),
        found: path.is_some(),
        path,
    }
}

fn detect_host(service: &ServicePlan) -> HostDetection {
    HostDetection {
        operating_system: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        tools: ["git", "docker", "podman"]
            .into_iter()
            .map(detect_executable)
            .collect(),
        agents: ["claude", "codex", "hermes", "openclaw"]
            .into_iter()
            .map(detect_executable)
            .collect(),
        service_manager: service.manager.clone(),
    }
}

fn inspect_initial_home(home: &Path) -> Result<InitialHome> {
    match fs::symlink_metadata(home) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(invalid(format!(
                    "Hardknock home must be a regular directory: {}",
                    home.display()
                )));
            }
            if metadata.uid() != geteuid().as_raw() {
                return Err(invalid(format!(
                    "Hardknock home is owned by another user: {}",
                    home.display()
                )));
            }
            if metadata.mode() & 0o022 != 0 {
                return Err(invalid(format!(
                    "Hardknock home is group/world-writable: {}",
                    home.display()
                )));
            }
            if fs::read_dir(home)?.next().is_none() {
                Ok(InitialHome::Empty {
                    mode: metadata.mode() & 0o777,
                    identity: file_identity(&metadata),
                })
            } else {
                Ok(InitialHome::Existing)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(InitialHome::Missing),
        Err(error) => Err(error.into()),
    }
}

fn read_setup_manifest_with_bytes(home: &Path) -> Result<Option<(SetupManifest, Vec<u8>)>> {
    read_setup_manifest_with_bytes_and_hook(home, || {})
}

fn read_setup_manifest_with_bytes_and_hook(
    home: &Path,
    after_read: impl FnOnce(),
) -> Result<Option<(SetupManifest, Vec<u8>)>> {
    let path = setup_manifest_path(home);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > MAX_SETUP_MANIFEST_BYTES
    {
        return Err(invalid(format!(
            "Managed setup manifest is unsafe: {}",
            path.display()
        )));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let opened = file.metadata()?;
    if metadata.dev() != opened.dev()
        || metadata.ino() != opened.ino()
        || metadata.len() != opened.len()
    {
        return Err(invalid(
            "Managed setup manifest changed while it was opened",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(64 * 1024) as usize);
    Read::by_ref(&mut file)
        .take(MAX_SETUP_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    after_read();
    let after = file.metadata()?;
    let current = fs::symlink_metadata(&path)?;
    if bytes.len() as u64 > MAX_SETUP_MANIFEST_BYTES
        || !same_manifest_metadata(&metadata, &after)
        || !same_manifest_metadata(&after, &current)
    {
        return Err(invalid("Managed setup manifest changed while it was read"));
    }
    let manifest: SetupManifest = serde_json::from_slice(&bytes)?;
    if manifest.format != SETUP_MANIFEST_FORMAT || manifest.home != home {
        return Err(invalid(
            "Managed setup manifest format or data-home identity does not match",
        ));
    }
    Ok(Some((manifest, bytes)))
}

#[cfg(test)]
fn read_setup_manifest(home: &Path) -> Result<Option<SetupManifest>> {
    Ok(read_setup_manifest_with_bytes(home)?.map(|(manifest, _)| manifest))
}

fn setup_manifest_bytes(manifest: &SetupManifest) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    if bytes.len() as u64 > MAX_SETUP_MANIFEST_BYTES {
        return Err(invalid("Setup manifest exceeds the supported size"));
    }
    Ok(bytes)
}

fn write_setup_manifest(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
    observer: &mut impl MutationObserver,
) -> Result<()> {
    if bytes.len() as u64 > MAX_SETUP_MANIFEST_BYTES {
        return Err(invalid("Setup manifest exceeds the supported size"));
    }
    install::write_managed_bytes_observed(path, bytes, true, expected, observer)
}

fn same_manifest_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.nlink() == right.nlink()
        && left.mode() == right.mode()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

fn prior_or_detected_agents(
    operation: Operation,
    requested: &[SetupAgent],
    prior: Option<&SetupManifest>,
) -> Result<Vec<String>> {
    if requested.contains(&SetupAgent::None) && requested.len() != 1 {
        return Err(Error::InvalidInput(
            "--agent none cannot be combined with another agent selection".into(),
        ));
    }
    if requested.contains(&SetupAgent::Auto) && requested.len() != 1 {
        return Err(Error::InvalidInput(
            "--agent auto cannot be combined with explicit agent selections".into(),
        ));
    }
    if requested == [SetupAgent::None] {
        return Ok(Vec::new());
    }
    let mut selected = BTreeSet::new();
    if requested == [SetupAgent::Auto] {
        if matches!(operation, Operation::Upgrade | Operation::Repair)
            && let Some(prior) = prior
        {
            selected.extend(prior.agents.iter().cloned());
        } else {
            for agent in ["claude", "codex", "hermes", "openclaw"] {
                if install::find_executable(agent).is_some() {
                    selected.insert(agent.to_owned());
                }
            }
        }
    } else {
        selected.extend(
            requested
                .iter()
                .filter(|agent| !matches!(agent, SetupAgent::Auto | SetupAgent::None))
                .map(|agent| agent.name().to_owned()),
        );
    }
    Ok(selected.into_iter().collect())
}

fn integration_plans(
    agents: &[String],
    home: &Path,
    executable: &Path,
    install_adapters: bool,
) -> Result<Vec<IntegrationPlan>> {
    let mut plans = Vec::new();
    for agent in agents {
        if agent == "codex" {
            continue;
        }
        let command = if install_adapters {
            AdapterCommand::Install { config: None }
        } else {
            AdapterCommand::Uninstall { config: None }
        };
        plans.push(install::plan(agent, home, &command, executable)?);
    }
    Ok(plans)
}

fn service_request(mode: SetupMode) -> RequestedService {
    match mode {
        SetupMode::Workstation => RequestedService::Auto,
        SetupMode::Ci => RequestedService::OnDemand,
    }
}

fn service_content_hash(plan: &ServicePlan) -> Option<String> {
    plan.content
        .as_ref()
        .map(|content| blake3::hash(content.as_bytes()).to_hex().to_string())
}

fn planned_changes(
    operation: Operation,
    home: &Path,
    integrations: &[IntegrationPlan],
    service: &ServicePlan,
    recovery: bool,
    remove_data: bool,
) -> Vec<PlannedChange> {
    let mut changes = Vec::new();
    if operation != Operation::Uninstall {
        changes.push(PlannedChange {
            id: "home".into(),
            action: "create_or_validate".into(),
            path: Some(home.to_path_buf()),
            summary: "Create or validate an owner-private Hardknock data home".into(),
        });
        if recovery {
            changes.push(PlannedChange {
                id: "recovery_point".into(),
                action: "create".into(),
                path: Some(home.join("backups")),
                summary: "Create and verify a managed recovery point".into(),
            });
        }
    }
    for integration in integrations {
        for path in &integration.managed_paths {
            changes.push(PlannedChange {
                id: format!("integration.{}", integration.agent),
                action: match integration.action {
                    install::IntegrationAction::Install => "create_or_update",
                    install::IntegrationAction::Uninstall => "remove_managed",
                }
                .into(),
                path: Some(path.clone()),
                summary: integration.action_summary.clone(),
            });
        }
    }
    if let Some(path) = &service.target {
        changes.push(PlannedChange {
            id: "bridge_service".into(),
            action: if operation == Operation::Uninstall {
                "remove_managed"
            } else {
                "create_or_update"
            }
            .into(),
            path: Some(path.clone()),
            summary: "Manage the per-user Hardknock Bridge service definition".into(),
        });
    }
    changes.push(PlannedChange {
        id: "setup_manifest".into(),
        action: if operation == Operation::Uninstall {
            "remove_managed"
        } else {
            "create_or_update"
        }
        .into(),
        path: Some(setup_manifest_path(home)),
        summary: "Record the exact files owned by managed setup".into(),
    });
    if remove_data {
        changes.push(PlannedChange {
            id: "data_home".into(),
            action: "remove_explicit".into(),
            path: Some(home.to_path_buf()),
            summary: "Remove the verified managed data home after owned integrations".into(),
        });
    }
    changes
}

fn plan_value(plan: &SetupPlan) -> Result<Value> {
    Ok(serde_json::to_value(plan)?)
}

fn verified_backup_exists(home: &Path) -> bool {
    let latest = fs::read_dir(home.join("backups"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified);
    latest.is_some_and(|(_, path)| storage::verify_backup(&path).is_ok())
}

fn rollback_home(home: &Path, initial: InitialHome) -> Result<()> {
    match initial {
        InitialHome::Existing => Ok(()),
        InitialHome::Missing => Ok(()),
        InitialHome::Empty { mode, identity } => {
            let descriptor = open(
                home,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| Error::Io(std::io::Error::from(error)))?;
            let opened =
                fstat(&descriptor).map_err(|error| Error::Io(std::io::Error::from(error)))?;
            let named = statat(CWD, home, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| Error::Io(std::io::Error::from(error)))?;
            let current_mode = opened.st_mode as u32 & 0o777;
            if opened.st_dev as u64 != identity.dev
                || opened.st_ino != identity.ino
                || opened.st_uid != identity.uid
                || opened.st_dev != named.st_dev
                || opened.st_ino != named.st_ino
                || (current_mode != mode && current_mode != 0o700)
            {
                return Err(invalid(
                    "Hardknock home identity or permissions changed concurrently; current data was preserved",
                ));
            }
            fchmod(&descriptor, Mode::from_raw_mode(mode as _))
                .map_err(|error| Error::Io(std::io::Error::from(error)))?;
            fsync(&descriptor).map_err(|error| Error::Io(std::io::Error::from(error)))?;
            let after =
                fstat(&descriptor).map_err(|error| Error::Io(std::io::Error::from(error)))?;
            let named_after = statat(CWD, home, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| Error::Io(std::io::Error::from(error)))?;
            if after.st_dev as u64 != identity.dev
                || after.st_ino != identity.ino
                || after.st_uid != identity.uid
                || after.st_mode as u32 & 0o777 != mode
                || after.st_dev != named_after.st_dev
                || after.st_ino != named_after.st_ino
            {
                return Err(invalid(
                    "Hardknock home changed while its original mode was restored",
                ));
            }
            Ok(())
        }
    }
}

fn create_missing_transaction_home(
    home: &Path,
    initial: InitialHome,
    observer: &mut impl MutationObserver,
) -> Result<()> {
    if initial != InitialHome::Missing {
        return Ok(());
    }
    ensure_directory_tree(home, true, observer)
}

#[derive(Debug)]
struct BridgeQuiescenceGuard {
    _lock: Option<fs::File>,
}

fn setup_errno(error: rustix::io::Errno) -> Error {
    Error::Io(std::io::Error::from(error))
}

fn try_bridge_quiescence(home: &Path) -> Result<Option<BridgeQuiescenceGuard>> {
    let home_metadata = match fs::symlink_metadata(home) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(BridgeQuiescenceGuard { _lock: None }));
        }
        Err(error) => return Err(error.into()),
    };
    if home_metadata.file_type().is_symlink()
        || !home_metadata.is_dir()
        || home_metadata.uid() != geteuid().as_raw()
        || home_metadata.mode() & 0o022 != 0
    {
        return Err(invalid(
            "Hardknock home is unsafe while waiting for Bridge shutdown",
        ));
    }
    let run_path = home.join("run");
    let run_metadata = match fs::symlink_metadata(&run_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(BridgeQuiescenceGuard { _lock: None }));
        }
        Err(error) => return Err(error.into()),
    };
    if run_metadata.file_type().is_symlink()
        || !run_metadata.is_dir()
        || run_metadata.uid() != geteuid().as_raw()
        || run_metadata.mode() & 0o777 != 0o700
    {
        return Err(invalid(
            "Bridge runtime directory is unsafe while waiting for shutdown",
        ));
    }
    let run = fs::File::from(
        open(
            &run_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(setup_errno)?,
    );
    let opened_run = fstat(&run).map_err(setup_errno)?;
    let named_run = statat(CWD, &run_path, AtFlags::SYMLINK_NOFOLLOW).map_err(setup_errno)?;
    if FileType::from_raw_mode(opened_run.st_mode) != FileType::Directory
        || opened_run.st_dev != named_run.st_dev
        || opened_run.st_ino != named_run.st_ino
        || opened_run.st_uid != geteuid().as_raw()
        || opened_run.st_mode as u32 & 0o777 != 0o700
    {
        return Err(invalid(
            "Bridge runtime directory changed while waiting for shutdown",
        ));
    }

    let lock = match statat(&run, "bridge.lock", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(observed) => {
            if FileType::from_raw_mode(observed.st_mode) != FileType::RegularFile
                || observed.st_uid != geteuid().as_raw()
                || observed.st_nlink != 1
                || observed.st_mode as u32 & 0o777 != 0o600
            {
                return Err(invalid(
                    "Bridge runtime lock is unsafe while waiting for shutdown",
                ));
            }
            match openat(
                &run,
                "bridge.lock",
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(descriptor) => {
                    let lock = fs::File::from(descriptor);
                    let opened = fstat(&lock).map_err(setup_errno)?;
                    let named = statat(&run, "bridge.lock", AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(setup_errno)?;
                    if opened.st_dev != observed.st_dev
                        || opened.st_ino != observed.st_ino
                        || opened.st_dev != named.st_dev
                        || opened.st_ino != named.st_ino
                        || opened.st_uid != observed.st_uid
                        || opened.st_nlink != 1
                        || opened.st_mode != observed.st_mode
                    {
                        return Err(invalid(
                            "Bridge runtime lock changed while waiting for shutdown",
                        ));
                    }
                    lock
                }
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(setup_errno(error)),
            }
        }
        Err(rustix::io::Errno::NOENT) => {
            for name in ["hardknock.sock", "bridge-token", "bridge-endpoint.json"] {
                match statat(&run, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Err(rustix::io::Errno::NOENT) => {}
                    Ok(_) => {
                        return Err(invalid(
                            "Bridge runtime endpoints exist without a lock; managed files and data were preserved",
                        ));
                    }
                    Err(error) => return Err(setup_errno(error)),
                }
            }
            return Ok(Some(BridgeQuiescenceGuard { _lock: None }));
        }
        Err(error) => return Err(setup_errno(error)),
    };

    match FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let opened_lock = fstat(&lock).map_err(setup_errno)?;
    let named_lock = statat(&run, "bridge.lock", AtFlags::SYMLINK_NOFOLLOW).map_err(setup_errno)?;
    if FileType::from_raw_mode(opened_lock.st_mode) != FileType::RegularFile
        || opened_lock.st_dev != named_lock.st_dev
        || opened_lock.st_ino != named_lock.st_ino
        || opened_lock.st_uid != geteuid().as_raw()
        || opened_lock.st_nlink != 1
        || opened_lock.st_mode as u32 & 0o777 != 0o600
    {
        return Err(invalid(
            "Bridge runtime lock changed after shutdown quiescence",
        ));
    }

    crate::reconciliation::reconcile_stale_bridge_runtime(home)?;
    fsync(&run).map_err(setup_errno)?;
    let current_run = statat(CWD, &run_path, AtFlags::SYMLINK_NOFOLLOW).map_err(setup_errno)?;
    if opened_run.st_dev != current_run.st_dev || opened_run.st_ino != current_run.st_ino {
        return Err(invalid(
            "Bridge runtime directory changed after shutdown quiescence",
        ));
    }
    for name in ["hardknock.sock", "bridge-token", "bridge-endpoint.json"] {
        match statat(&run, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => {}
            Ok(_) => {
                return Err(invalid(
                    "Bridge runtime endpoints remain after shutdown quiescence",
                ));
            }
            Err(error) => return Err(setup_errno(error)),
        }
    }
    Ok(Some(BridgeQuiescenceGuard { _lock: Some(lock) }))
}

async fn wait_for_bridge_quiescence(
    home: &Path,
    timeout: Duration,
) -> Result<BridgeQuiescenceGuard> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(guard) = try_bridge_quiescence(home)? {
            return Ok(guard);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(invalid(format!(
                "Bridge did not release its runtime lock within {} ms; managed files and data were preserved",
                timeout.as_millis()
            )));
        }
        tokio::time::sleep(BRIDGE_QUIESCENCE_POLL.min(deadline.saturating_duration_since(now)))
            .await;
    }
}

async fn recover_before_operation(
    operation: Operation,
    home: &Path,
    dry_run: bool,
) -> Result<Option<PathBuf>> {
    let Some(mut recovery) = unfinished(home)? else {
        return Ok(None);
    };
    if operation != Operation::Repair || dry_run {
        return Err(invalid(format!(
            "An unfinished Hardknock {} transaction exists. Run `hardknock repair --non-interactive` without --dry-run before continuing.",
            recovery.operation()
        )));
    }
    let interrupted_operation = recovery.operation().to_owned();
    let initial_home = recovery.initial_home();
    let quiescence_home = if let Some(action) = recovery.quarantine_action() {
        if !path_exists_without_following(&action.original)?
            && path_exists_without_following(&action.quarantine)?
        {
            action.quarantine.clone()
        } else {
            home.to_path_buf()
        }
    } else {
        home.to_path_buf()
    };
    let mut bridge = crate::bridge::transport::BridgeClient::new(&quiescence_home);
    bridge.timeout = Duration::from_millis(500);
    let _ = bridge
        .request(crate::bridge::protocol::AgentEvent::Shutdown)
        .await;
    let _bridge_quiescence =
        wait_for_bridge_quiescence(&quiescence_home, BRIDGE_QUIESCENCE_TIMEOUT).await?;

    if let Some(action) = recovery.quarantine_action().cloned() {
        if interrupted_operation != Operation::Uninstall.name() {
            return Err(invalid(
                "Only an interrupted uninstall may contain a data-home quarantine",
            ));
        }
        let original_exists = path_exists_without_following(&action.original)?;
        let quarantine_exists = path_exists_without_following(&action.quarantine)?;
        if action.state == QuarantineState::Planned {
            match (original_exists, quarantine_exists) {
                (true, false) => recovery.cancel_planned_quarantine()?,
                (false, true) => recovery.checkpoint_quarantine_applied()?,
                _ => {
                    return Err(invalid(
                        "Planned data-home quarantine is ambiguous; inspect the original and quarantine paths before retrying",
                    ));
                }
            }
        }

        if let Some(action) = recovery.quarantine_action().cloned() {
            match action.recovery {
                QuarantineRecovery::ResumeDeletion => {
                    return Ok(Some(
                        recovery.finish_resumed_deletion("data_removal_completed_by_repair")?,
                    ));
                }
                QuarantineRecovery::Restore => {
                    let original_exists = path_exists_without_following(&action.original)?;
                    let quarantine_exists = path_exists_without_following(&action.quarantine)?;
                    match (original_exists, quarantine_exists) {
                        (false, true) => recovery.restore_quarantine()?,
                        (true, false) => recovery.checkpoint_quarantine_resolved()?,
                        _ => {
                            return Err(invalid(
                                "Data-home quarantine restoration is ambiguous; inspect the original and quarantine paths before retrying",
                            ));
                        }
                    }
                }
            }
        }
    }

    recovery.rollback_files()?;
    if interrupted_operation != Operation::Uninstall.name() {
        rollback_home(home, initial_home)?;
    }
    let journal = recovery.finish(home, "rolled_back_by_repair")?;
    Ok(Some(journal))
}

fn remove_setup_manifest(
    path: &Path,
    expected: Option<&[u8]>,
    observer: &mut impl MutationObserver,
) -> Result<bool> {
    match expected {
        Some(bytes) => {
            install::remove_managed_bytes_observed(path, bytes, observer)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn setup_manifest(
    home: &Path,
    executable: &Path,
    mode: SetupMode,
    agents: &[String],
    service: &ServicePlan,
) -> SetupManifest {
    SetupManifest {
        format: SETUP_MANIFEST_FORMAT.into(),
        package_version: env!("CARGO_PKG_VERSION").into(),
        home: home.to_path_buf(),
        executable: executable.to_path_buf(),
        mode: match mode {
            SetupMode::Workstation => "workstation",
            SetupMode::Ci => "ci",
        }
        .into(),
        agents: agents.to_vec(),
        service_target: service.target.clone(),
        service_content_blake3: service_content_hash(service),
        updated_at: Utc::now(),
    }
}

fn doctor_base(store: &Store) -> Result<Value> {
    Ok(json!({
        "kind": "doctor",
        "database": store.database_health()?,
        "schema_version": store.applied_schema_version()?,
        "package_version": env!("CARGO_PKG_VERSION")
    }))
}

fn doctor_exit_code(health: &Value) -> u8 {
    health["report"]["exit_code"]
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(2)
}

fn managed_snapshot_paths(
    integrations: &[IntegrationPlan],
    service: &ServicePlan,
    home: &Path,
    include_new_store_files: bool,
) -> Vec<PathBuf> {
    let mut paths = integrations
        .iter()
        .flat_map(|plan| plan.managed_paths.iter().cloned())
        .collect::<Vec<_>>();
    paths.extend(service.managed_paths());
    paths.push(setup_manifest_path(home));
    paths.push(home.join("run/bridge.lock"));
    if include_new_store_files {
        paths.extend(store_transaction_paths(home));
    }
    paths
}

fn store_transaction_paths(home: &Path) -> Vec<PathBuf> {
    ["hardknock.db", "hardknock.db-shm", "hardknock.db-wal"]
        .into_iter()
        .map(|name| home.join(name))
        .collect()
}

fn combine_cleanup_steps(steps: Vec<(&'static str, Result<()>)>) -> Result<()> {
    let failures = steps
        .into_iter()
        .filter_map(|(name, result)| result.err().map(|error| format!("{name}: {error}")))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(invalid(failures.join("; ")))
    }
}

struct SetupMutationObserver<'a> {
    snapshots: &'a mut SnapshotSet,
    journal: &'a Journal,
}

impl MutationObserver for SetupMutationObserver<'_> {
    fn prepare_write(&mut self, intent: PreparedWrite) -> Result<()> {
        self.snapshots.prepare_write(self.journal, intent)
    }

    fn record_write(&mut self, receipt: WriteReceipt) -> Result<()> {
        self.snapshots.checkpoint_receipt(self.journal, receipt)
    }

    fn prepare_removal(&mut self, intent: PreparedRemoval) -> Result<()> {
        self.snapshots.prepare_removal(self.journal, intent)
    }

    fn record_removal(&mut self, receipt: WriteReceipt) -> Result<()> {
        self.snapshots.checkpoint_receipt(self.journal, receipt)
    }

    fn abort_mutation(&mut self, path: &Path, cleanup: PrivateCleanup) -> Result<()> {
        self.snapshots.abort_mutation(self.journal, path, cleanup)
    }

    fn record_directory(&mut self, receipt: DirectoryReceipt) -> Result<()> {
        self.snapshots.record_directory(self.journal, receipt)
    }
}

pub async fn apply(
    operation: Operation,
    home: &Path,
    user_home: &Path,
    executable: &Path,
    args: &SetupArgs,
) -> Result<SetupResult> {
    if operation == Operation::Uninstall {
        return Err(Error::InvalidInput(
            "Use the uninstall operation with UninstallArgs".into(),
        ));
    }
    let _setup_lock = SetupLock::acquire(home)?;
    let recovered_journal = recover_before_operation(operation, home, args.dry_run).await?;
    let initial_home = inspect_initial_home(home)?;
    let (prior, prior_manifest_bytes) = match read_setup_manifest_with_bytes(home)? {
        Some((manifest, bytes)) => (Some(manifest), Some(bytes)),
        None => (None, None),
    };
    let agents = prior_or_detected_agents(operation, &args.agent, prior.as_ref())?;
    let service = ServicePlan::detect(
        user_home.to_path_buf(),
        home.to_path_buf(),
        executable.to_path_buf(),
        service_request(args.mode),
    )?;
    let integrations = integration_plans(&agents, home, executable, true)?;
    let migration = storage::migration_plan(home)?;
    let recovery_required = operation == Operation::Upgrade || !verified_backup_exists(home);
    let detection = detect_host(&service);
    let plan = SetupPlan {
        operation,
        home: home.to_path_buf(),
        executable: executable.to_path_buf(),
        mode: match args.mode {
            SetupMode::Workstation => "workstation",
            SetupMode::Ci => "ci",
        }
        .into(),
        agents: agents.clone(),
        migration: Some(migration.clone()),
        detection,
        changes: planned_changes(
            operation,
            home,
            &integrations,
            &service,
            recovery_required,
            false,
        ),
        integrations: integrations
            .iter()
            .map(IntegrationPlan::description)
            .collect::<Result<Vec<_>>>()?,
        service: service.clone(),
    };
    if args.dry_run {
        return Ok(SetupResult {
            schema: SETUP_RESULT_SCHEMA,
            operation,
            dry_run: true,
            changed: false,
            ready: false,
            exit_code: 0,
            home: home.to_path_buf(),
            executable: executable.to_path_buf(),
            plan,
            applied: Vec::new(),
            recovery_point: None,
            service: None,
            health: None,
            journal: None,
            warnings: vec![
                "Dry-run inspected current paths and made no filesystem or service changes".into(),
            ],
        });
    }

    let include_new_store_files = !migration.database_exists;
    let mut snapshots = SnapshotSet::capture(managed_snapshot_paths(
        &integrations,
        &service,
        home,
        include_new_store_files,
    ))?;
    snapshots.track_directories(
        TRANSACTION_DIRECTORIES
            .iter()
            .map(|relative| home.join(relative)),
    )?;
    let mut journal = Journal::begin(
        home,
        operation.name(),
        &plan_value(&plan)?,
        initial_home,
        &snapshots,
    )?;
    journal.record(
        "rollback_snapshot",
        serde_json::to_value(snapshots.descriptions())?,
    )?;
    let mut service_applied = false;
    let result = async {
        let mut recovery_point =
            if operation == Operation::Upgrade && migration.database_exists && !migration.migration_required
            {
                let backup = storage::create_managed_recovery_point(home, "pre-upgrade")?;
                journal.record("recovery_point_created", serde_json::to_value(&backup)?)?;
                Some(backup)
            } else {
                None
        };
        {
            let mut observer = SetupMutationObserver {
                snapshots: &mut snapshots,
                journal: &journal,
            };
            if include_new_store_files {
                create_missing_transaction_home(home, initial_home, &mut observer)?;
            }
            for relative in TRANSACTION_DIRECTORIES {
                ensure_directory_tree(&home.join(relative), true, &mut observer)?;
            }
        }
        if include_new_store_files {
            for path in store_transaction_paths(home) {
                drop(snapshots.create_mutable_file(&journal, &path)?);
            }
        }
        let bridge_lock = home.join("run/bridge.lock");
        if !path_exists_without_following(&bridge_lock)? {
            drop(snapshots.create_mutable_file(&journal, &bridge_lock)?);
        }
        let store = Store::open(home)?;
        snapshots.record_existing_created_directories(&journal)?;
        journal.record(
            "store_ready",
            json!({"schema_version":store.applied_schema_version()?}),
        )?;
        if recovery_point.is_none() && !verified_backup_exists(home) {
            let label = match operation {
                Operation::Setup => "setup",
                Operation::Upgrade => "upgrade",
                Operation::Repair => "repair",
                Operation::Uninstall => unreachable!(),
            };
            let backup = storage::create_managed_recovery_point(home, label)?;
            snapshots.record_existing_created_directories(&journal)?;
            journal.record("recovery_point_created", serde_json::to_value(&backup)?)?;
            recovery_point = Some(backup);
        }

        let mut applied = Vec::new();
        for integration in &integrations {
            let report = {
                let mut observer = SetupMutationObserver {
                    snapshots: &mut snapshots,
                    journal: &journal,
                };
                install::apply_observed(integration, &mut observer)?
            };
            journal.record("integration_applied", report.clone())?;
            applied.push(report);
        }
        let manifest = setup_manifest(home, executable, args.mode, &agents, &service);
        let manifest_path = setup_manifest_path(home);
        let manifest_bytes = setup_manifest_bytes(&manifest)?;
        {
            let mut observer = SetupMutationObserver {
                snapshots: &mut snapshots,
                journal: &journal,
            };
            write_setup_manifest(
                &manifest_path,
                &manifest_bytes,
                prior_manifest_bytes.as_deref(),
                &mut observer,
            )?;
        }
        journal.record(
            "setup_manifest_written",
            json!({"path":manifest_path}),
        )?;

        let mut service_report = {
            let mut observer = SetupMutationObserver {
                snapshots: &mut snapshots,
                journal: &journal,
            };
            service.apply_observed(args.start, &mut observer).await?
        };
        service_applied = service_report.changed || args.start;
        journal.record(
            "service_applied",
            serde_json::to_value(&service_report)?,
        )?;
        let mut warnings = Vec::new();
        if args.start
            && (matches!(
                service_report.manager,
                service::ServiceManager::OnDemand { .. }
            ) || !service_report.manager_ready)
        {
            crate::cli::integrations::start(home, None).await?;
            warnings.push(service_report.fallback.clone().unwrap_or_else(|| {
                "Native service manager was unavailable; Bridge started on demand".into()
            }));
            service_report.manager_ready = true;
        }

        let health = crate::cli::maintenance::doctor(&store, doctor_base(&store)?, true).await?;
        snapshots.record_existing_created_directories(&journal)?;
        let exit_code = doctor_exit_code(&health);
        journal.record(
            "strict_doctor",
            json!({"exit_code":exit_code,"ready":exit_code == 0}),
        )?;
        if exit_code != 0 {
            warnings.push(
                "Strict doctor reported degraded or unavailable production checks; inspect the embedded health report"
                    .into(),
            );
        }
        Ok::<_, Error>((
            applied,
            recovery_point,
            service_report,
            health,
            exit_code,
            warnings,
        ))
    }
    .await;

    match result {
        Ok((applied, recovery_point, service_report, health, exit_code, mut warnings)) => {
            if let Some(path) = recovered_journal {
                warnings.push(format!(
                    "Recovered and rolled back an unfinished setup transaction; recovery journal: {}",
                    path.display()
                ));
            }
            let journal_path = journal.finish(home, "succeeded")?;
            Ok(SetupResult {
                schema: SETUP_RESULT_SCHEMA,
                operation,
                dry_run: false,
                changed: true,
                ready: exit_code == 0,
                exit_code,
                home: home.to_path_buf(),
                executable: executable.to_path_buf(),
                plan,
                applied,
                recovery_point,
                service: Some(service_report),
                health: Some(health),
                journal: Some(journal_path),
                warnings,
            })
        }
        Err(primary) => {
            let _ = journal.record(
                "apply_failed",
                json!({"error":"setup step failed; command diagnostics were not copied into the journal"}),
            );
            let shutdown_accepted = if service_applied {
                let mut bridge = crate::bridge::transport::BridgeClient::new(home);
                bridge.timeout = Duration::from_millis(500);
                bridge
                    .request(crate::bridge::protocol::AgentEvent::Shutdown)
                    .await
                    .is_ok()
            } else {
                false
            };
            if service_applied {
                let _ = journal.record(
                    "bridge_stop_requested_for_rollback",
                    json!({"accepted":shutdown_accepted}),
                );
            }
            let service_rollback = if service_applied {
                let mut observer = SetupMutationObserver {
                    snapshots: &mut snapshots,
                    journal: &journal,
                };
                service
                    .uninstall_observed(true, &mut observer)
                    .await
                    .map(|_| ())
            } else {
                Ok(())
            };
            if service_rollback.is_err() {
                let _ = journal.record(
                    "service_rollback_failed",
                    json!({"error":"managed service could not be removed before file rollback"}),
                );
            }
            let bridge_quiescence = if service_applied {
                wait_for_bridge_quiescence(home, BRIDGE_QUIESCENCE_TIMEOUT).await
            } else {
                Ok(BridgeQuiescenceGuard { _lock: None })
            };
            if bridge_quiescence.is_err() {
                let _ = journal.record(
                    "bridge_shutdown_incomplete_for_rollback",
                    json!({"error":"Bridge runtime lock remained held; managed-file rollback was deferred"}),
                );
            }
            let file_rollback = if bridge_quiescence.is_ok() {
                snapshots.rollback(&journal)
            } else {
                Err(invalid(
                    "Managed-file rollback was deferred because the Bridge did not become quiescent",
                ))
            };
            let home_rollback = if bridge_quiescence.is_ok() {
                rollback_home(home, initial_home)
            } else {
                Err(invalid(
                    "Data-home rollback was deferred because the Bridge did not become quiescent",
                ))
            };
            let _ = journal.record(
                "rollback_finished",
                json!({
                    "service":service_rollback.as_ref().map(|_| "restored").unwrap_or("failed"),
                    "managed_files":file_rollback.as_ref().map(|_| "restored").unwrap_or("failed"),
                    "home":home_rollback.as_ref().map(|_| "restored").unwrap_or("failed")
                }),
            );
            let rollback = combine_cleanup_steps(vec![
                ("service rollback failed", service_rollback),
                ("managed-file rollback failed", file_rollback),
                ("home rollback failed", home_rollback),
            ]);
            if let Err(rollback_error) = rollback {
                let preservation = journal
                    .preserve_for_repair("setup rollback did not complete")
                    .map(|_| ());
                let cleanup = combine_cleanup_steps(vec![
                    ("rollback compensation failed", Err(rollback_error)),
                    ("recovery preservation failed", preservation),
                ])
                .expect_err("failed rollback must produce a cleanup error");
                return Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            if let Err(cleanup) = journal.finish_after_rollback(home) {
                return Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            Err(primary)
        }
    }
}

fn uninstall_agents(prior: Option<&SetupManifest>, home: &Path) -> Vec<String> {
    if let Some(prior) = prior {
        return prior.agents.clone();
    }
    ["claude", "hermes", "openclaw"]
        .into_iter()
        .filter(|agent| {
            home.join("integrations")
                .join(format!("{agent}.json"))
                .is_file()
        })
        .map(str::to_owned)
        .collect()
}

fn prior_mode(prior: Option<&SetupManifest>) -> SetupMode {
    if prior.is_some_and(|manifest| manifest.mode == "ci") {
        SetupMode::Ci
    } else {
        SetupMode::Workstation
    }
}

fn managed_installation_present(
    prior: Option<&SetupManifest>,
    integrations: &[IntegrationPlan],
    service: &ServicePlan,
    home: &Path,
) -> bool {
    prior.is_some()
        || setup_manifest_path(home).exists()
        || service.target.as_deref().is_some_and(Path::exists)
        || integrations
            .iter()
            .flat_map(|plan| plan.managed_paths.iter())
            .any(|path| path.exists())
}

async fn restore_bridge_after_failed_uninstall(
    was_running: bool,
    home: &Path,
    user_home: &Path,
    executable: &Path,
    mode: SetupMode,
    previous_service: &ServicePlan,
) -> Result<()> {
    if !was_running {
        return Ok(());
    }
    async fn wait_until_ready(home: &Path) -> bool {
        let mut client = crate::bridge::transport::BridgeClient::new(home);
        client.timeout = Duration::from_millis(250);
        for _ in 0..20 {
            if client
                .request(crate::bridge::protocol::AgentEvent::Status)
                .await
                .is_ok()
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    if matches!(
        previous_service.manager,
        service::ServiceManager::OnDemand { .. }
    ) {
        crate::cli::integrations::start(home, None).await?;
        return if wait_until_ready(home).await {
            Ok(())
        } else {
            Err(invalid(
                "Bridge did not become ready after uninstall rollback",
            ))
        };
    }
    let restored = ServicePlan::detect(
        user_home.to_path_buf(),
        home.to_path_buf(),
        executable.to_path_buf(),
        service_request(mode),
    )?;
    let report = restored.apply(true).await?;
    if report.manager_ready && wait_until_ready(home).await {
        return Ok(());
    }
    crate::cli::integrations::start(home, None).await?;
    if wait_until_ready(home).await {
        Ok(())
    } else {
        Err(invalid(
            "Bridge did not become ready after native and on-demand rollback attempts",
        ))
    }
}

fn validate_data_home_removal(home: &Path, prior: &SetupManifest) -> Result<()> {
    if prior.home != home {
        return Err(invalid(
            "Setup manifest does not authorize this data-home removal",
        ));
    }
    let metadata = fs::symlink_metadata(home)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(invalid(
            "Data removal requires the exact owner-private managed Hardknock home",
        ));
    }
    Ok(())
}

pub async fn uninstall(
    home: &Path,
    user_home: &Path,
    executable: &Path,
    args: &UninstallArgs,
) -> Result<SetupResult> {
    let _setup_lock = SetupLock::acquire(home)?;
    let _ = recover_before_operation(Operation::Uninstall, home, args.dry_run).await?;
    let (prior, prior_manifest_bytes) = match read_setup_manifest_with_bytes(home)? {
        Some((manifest, bytes)) => (Some(manifest), Some(bytes)),
        None => (None, None),
    };
    if args.remove_data && prior.is_none() {
        return Err(invalid(
            "--remove-data requires a valid managed setup manifest",
        ));
    }
    let agents = uninstall_agents(prior.as_ref(), home);
    let mode = prior_mode(prior.as_ref());
    let stable_executable = prior
        .as_ref()
        .map(|manifest| manifest.executable.as_path())
        .unwrap_or(executable);
    let service = ServicePlan::detect(
        user_home.to_path_buf(),
        home.to_path_buf(),
        stable_executable.to_path_buf(),
        service_request(mode),
    )?;
    let integrations = integration_plans(&agents, home, stable_executable, false)?;
    let plan = SetupPlan {
        operation: Operation::Uninstall,
        home: home.to_path_buf(),
        executable: stable_executable.to_path_buf(),
        mode: match mode {
            SetupMode::Workstation => "workstation",
            SetupMode::Ci => "ci",
        }
        .into(),
        agents: agents.clone(),
        migration: None,
        detection: detect_host(&service),
        changes: planned_changes(
            Operation::Uninstall,
            home,
            &integrations,
            &service,
            false,
            args.remove_data,
        ),
        integrations: integrations
            .iter()
            .map(IntegrationPlan::description)
            .collect::<Result<Vec<_>>>()?,
        service: service.clone(),
    };
    if args.dry_run {
        return Ok(SetupResult {
            schema: SETUP_RESULT_SCHEMA,
            operation: Operation::Uninstall,
            dry_run: true,
            changed: false,
            ready: false,
            exit_code: 0,
            home: home.to_path_buf(),
            executable: stable_executable.to_path_buf(),
            plan,
            applied: Vec::new(),
            recovery_point: None,
            service: None,
            health: None,
            journal: None,
            warnings: vec![
                "Dry-run inspected managed ownership and made no filesystem or service changes"
                    .into(),
            ],
        });
    }

    if !args.remove_data
        && !managed_installation_present(prior.as_ref(), &integrations, &service, home)
    {
        return Ok(SetupResult {
            schema: SETUP_RESULT_SCHEMA,
            operation: Operation::Uninstall,
            dry_run: false,
            changed: false,
            ready: false,
            exit_code: 0,
            home: home.to_path_buf(),
            executable: stable_executable.to_path_buf(),
            plan,
            applied: Vec::new(),
            recovery_point: None,
            service: None,
            health: None,
            journal: None,
            warnings: vec!["No managed Hardknock installation was found".into()],
        });
    }

    let mut snapshots =
        SnapshotSet::capture(managed_snapshot_paths(&integrations, &service, home, false))?;
    let mut journal = Journal::begin(
        home,
        Operation::Uninstall.name(),
        &plan_value(&plan)?,
        InitialHome::Existing,
        &snapshots,
    )?;
    let data_quarantine = args
        .remove_data
        .then(|| data_removal_quarantine(home))
        .transpose()?;
    let mut bridge_was_running = false;
    let mut data_removal_prepared = false;
    let mut data_removal_committed = false;
    let result = async {
        let mut bridge = crate::bridge::transport::BridgeClient::new(home);
        bridge.timeout = Duration::from_secs(2);
        bridge_was_running = bridge
            .request(crate::bridge::protocol::AgentEvent::Status)
            .await
            .is_ok();
        let shutdown_accepted = bridge
            .request(crate::bridge::protocol::AgentEvent::Shutdown)
            .await
            .is_ok();
        journal.record(
            "bridge_stop_requested",
            json!({"accepted":shutdown_accepted}),
        )?;
        let service_report = {
            let mut observer = SetupMutationObserver {
                snapshots: &mut snapshots,
                journal: &journal,
            };
            service.uninstall_observed(true, &mut observer).await?
        };
        if !service_report.manager_ready {
            return Err(invalid(service_report.fallback.clone().unwrap_or_else(
                || "Could not stop the managed Bridge service".into(),
            )));
        }
        journal.record("service_removed", serde_json::to_value(&service_report)?)?;
        let _bridge_quiescence =
            wait_for_bridge_quiescence(home, BRIDGE_QUIESCENCE_TIMEOUT).await?;
        journal.record("bridge_shutdown_complete", json!({"quiescent":true}))?;
        let mut applied = Vec::new();
        for integration in &integrations {
            let report = {
                let mut observer = SetupMutationObserver {
                    snapshots: &mut snapshots,
                    journal: &journal,
                };
                install::apply_observed(integration, &mut observer)?
            };
            journal.record("integration_removed", report.clone())?;
            applied.push(report);
        }
        let manifest_path = setup_manifest_path(home);
        let removed_manifest = {
            let mut observer = SetupMutationObserver {
                snapshots: &mut snapshots,
                journal: &journal,
            };
            remove_setup_manifest(
                &manifest_path,
                prior_manifest_bytes.as_deref(),
                &mut observer,
            )?
        };
        if !removed_manifest {
            snapshots.checkpoint(&journal, vec![manifest_path])?;
        }
        journal.record("setup_manifest_removed", json!({}))?;
        if args.remove_data {
            validate_data_home_removal(
                home,
                prior
                    .as_ref()
                    .ok_or_else(|| invalid("Missing managed setup manifest"))?,
            )?;
            let quarantine = data_quarantine
                .as_ref()
                .ok_or_else(|| invalid("Missing managed data-removal quarantine path"))?;
            journal.prepare_quarantine(home, quarantine, QuarantineRecovery::ResumeDeletion)?;
            data_removal_prepared = true;
            journal.record(
                "data_home_removal_prepared",
                json!({"home":home,"quarantine":quarantine}),
            )?;
            if let Err(error) = journal.apply_quarantine() {
                if !path_exists_without_following(home)?
                    && path_exists_without_following(quarantine)?
                {
                    journal.checkpoint_quarantine_applied()?;
                    data_removal_committed = true;
                }
                return Err(error);
            }
            data_removal_committed = true;
            journal.record(
                "data_home_removal_committed",
                json!({"home":home,"quarantine":quarantine}),
            )?;
        }
        Ok::<_, Error>((service_report, applied))
    }
    .await;
    match result {
        Ok((service_report, applied)) => {
            let journal_path = if args.remove_data {
                Some(journal.finish_resumed_deletion("succeeded")?)
            } else {
                Some(journal.finish(home, "succeeded")?)
            };
            Ok(SetupResult {
                schema: SETUP_RESULT_SCHEMA,
                operation: Operation::Uninstall,
                dry_run: false,
                changed: service_report.changed || !applied.is_empty() || prior.is_some(),
                ready: false,
                exit_code: 0,
                home: home.to_path_buf(),
                executable: stable_executable.to_path_buf(),
                plan,
                applied,
                recovery_point: None,
                service: Some(service_report),
                health: None,
                journal: journal_path,
                warnings: if args.remove_data {
                    vec![
                        "The explicit data-home removal completed; the final journal remains beside the former home"
                            .into(),
                    ]
                } else {
                    vec!["The Hardknock data home and evidence were preserved".into()]
                },
            })
        }
        Err(primary) => {
            let _ = journal.record(
                "uninstall_failed",
                json!({"error":"uninstall step failed; command diagnostics were not copied into the journal"}),
            );
            if data_removal_prepared && !data_removal_committed {
                let quarantine_resolution = (|| {
                    let quarantine = data_quarantine
                        .as_ref()
                        .ok_or_else(|| invalid("Missing managed data-removal quarantine path"))?;
                    match (
                        path_exists_without_following(home)?,
                        path_exists_without_following(quarantine)?,
                    ) {
                        (true, false) => journal.clear_quarantine(),
                        (false, true) => {
                            journal.checkpoint_quarantine_applied()?;
                            data_removal_committed = true;
                            Ok(())
                        }
                        _ => Err(invalid(
                            "Prepared data-home quarantine is ambiguous; recovery was preserved",
                        )),
                    }
                })();
                if let Err(quarantine_error) = quarantine_resolution {
                    let preservation = journal
                        .preserve_for_repair("data-home quarantine could not be reconciled")
                        .map(|_| ());
                    let cleanup = combine_cleanup_steps(vec![
                        (
                            "data-home quarantine reconciliation failed",
                            Err(quarantine_error),
                        ),
                        ("recovery preservation failed", preservation),
                    ])
                    .expect_err("failed quarantine reconciliation must produce a cleanup error");
                    return Err(Error::Cleanup {
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    });
                }
            }
            if data_removal_committed {
                let _ = journal.record(
                    "data_home_removal_incomplete",
                    json!({"recovery":"run hardknock repair --non-interactive to resume committed removal"}),
                );
                let preservation = journal
                    .preserve_for_repair("committed data-home removal requires repair")
                    .map(|_| ());
                let cleanup = combine_cleanup_steps(vec![
                    (
                        "committed data-home removal requires repair",
                        Err(invalid(
                            "Managed data removal crossed its atomic commit point; repair will resume deletion",
                        )),
                    ),
                    ("recovery preservation failed", preservation),
                ])
                .expect_err("committed data removal must produce a cleanup error");
                return Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            let file_rollback = snapshots.rollback(&journal);
            let bridge_restore = restore_bridge_after_failed_uninstall(
                bridge_was_running,
                home,
                user_home,
                stable_executable,
                mode,
                &service,
            )
            .await;
            let _ = journal.record(
                "rollback_finished",
                json!({
                    "managed_files":file_rollback.as_ref().map(|_| "restored").unwrap_or("failed"),
                    "bridge":bridge_restore.as_ref().map(|_| "restored").unwrap_or("failed")
                }),
            );
            let cleanup = combine_cleanup_steps(vec![
                ("managed-file rollback failed", file_rollback),
                ("Bridge restoration failed", bridge_restore),
            ]);
            if let Err(rollback_error) = cleanup {
                let preservation = journal
                    .preserve_for_repair("uninstall rollback did not complete")
                    .map(|_| ());
                let cleanup = combine_cleanup_steps(vec![
                    ("rollback compensation failed", Err(rollback_error)),
                    ("recovery preservation failed", preservation),
                ])
                .expect_err("failed rollback must produce a cleanup error");
                return Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            if let Err(cleanup) = journal.finish_after_rollback(home) {
                return Err(Error::Cleanup {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                });
            }
            Err(primary)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::transaction::UnobservedMutation;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn bridge_quiescence_waits_for_lock_release_and_clears_stale_endpoints() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let run = home.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let lock_path = run.join("bridge.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
            .unwrap();
        FileExt::try_lock_exclusive(&lock).unwrap();

        let started = std::time::Instant::now();
        let error = wait_for_bridge_quiescence(&home, Duration::from_millis(60))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("did not release"));
        assert!(started.elapsed() >= Duration::from_millis(50));
        fs::write(run.join("bridge-token"), "stale").unwrap();
        fs::write(run.join("bridge-endpoint.json"), "{}").unwrap();
        fs::set_permissions(run.join("bridge-token"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(
            run.join("bridge-endpoint.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        drop(lock);

        let guard = wait_for_bridge_quiescence(&home, Duration::from_millis(500))
            .await
            .unwrap();

        assert!(!run.join("bridge-token").exists());
        assert!(!run.join("bridge-endpoint.json").exists());
        drop(guard);
    }

    #[test]
    fn auto_selection_is_stable_for_upgrade_and_repair() {
        let manifest = SetupManifest {
            format: SETUP_MANIFEST_FORMAT.into(),
            package_version: "test".into(),
            home: PathBuf::from("/tmp/hardknock-test"),
            executable: PathBuf::from("/tmp/hardknock"),
            mode: "workstation".into(),
            agents: vec!["claude".into(), "codex".into()],
            service_target: None,
            service_content_blake3: None,
            updated_at: Utc::now(),
        };
        assert_eq!(
            prior_or_detected_agents(Operation::Upgrade, &[SetupAgent::Auto], Some(&manifest))
                .unwrap(),
            vec!["claude", "codex"]
        );
    }

    #[test]
    fn agent_selection_rejects_ambiguous_auto_and_none() {
        assert!(
            prior_or_detected_agents(
                Operation::Setup,
                &[SetupAgent::Auto, SetupAgent::Claude],
                None
            )
            .is_err()
        );
        assert!(
            prior_or_detected_agents(
                Operation::Setup,
                &[SetupAgent::None, SetupAgent::Claude],
                None
            )
            .is_err()
        );
    }

    #[test]
    fn setup_manifest_round_trip_is_private_and_bound_to_home() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let service = ServicePlan {
            manager: service::ServiceManager::OnDemand {
                reason: "test".into(),
            },
            target: None,
            security_root: None,
            content: None,
            change: service::ServiceChange::OnDemand,
            on_demand_command: "hardknock bridge start".into(),
            fallback: "test".into(),
        };
        let manifest = setup_manifest(
            &home,
            Path::new("/tmp/hardknock"),
            SetupMode::Ci,
            &["codex".into()],
            &service,
        );
        let path = setup_manifest_path(&home);
        let bytes = setup_manifest_bytes(&manifest).unwrap();
        write_setup_manifest(&path, &bytes, None, &mut UnobservedMutation).unwrap();
        let loaded = read_setup_manifest(&home).unwrap().unwrap();
        assert_eq!(loaded.home, home);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn setup_manifest_read_rejects_post_read_metadata_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let service = ServicePlan {
            manager: service::ServiceManager::OnDemand {
                reason: "test".into(),
            },
            target: None,
            security_root: None,
            content: None,
            change: service::ServiceChange::OnDemand,
            on_demand_command: "hardknock bridge start".into(),
            fallback: "test".into(),
        };
        let manifest = setup_manifest(
            &home,
            Path::new("/tmp/hardknock"),
            SetupMode::Ci,
            &["codex".into()],
            &service,
        );
        let path = setup_manifest_path(&home);
        let bytes = setup_manifest_bytes(&manifest).unwrap();
        write_setup_manifest(&path, &bytes, None, &mut UnobservedMutation).unwrap();

        let error = read_setup_manifest_with_bytes_and_hook(&home, || {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        })
        .unwrap_err();

        assert!(error.to_string().contains("changed while it was read"));
    }

    #[test]
    fn rollback_of_new_home_preserves_concurrent_content() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join("setup")).unwrap();
        fs::set_permissions(home.join("setup"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(home.join("concurrent.txt"), "keep").unwrap();

        rollback_home(&home, InitialHome::Missing).unwrap();

        assert_eq!(
            fs::read_to_string(home.join("concurrent.txt")).unwrap(),
            "keep"
        );
        assert!(home.is_dir());
        assert!(home.join("setup").is_dir());
    }

    #[test]
    fn committed_data_removal_resumes_from_the_sibling_quarantine() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(home.join("hardknock.db"), b"managed data").unwrap();

        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            Operation::Uninstall.name(),
            &json!({"operation":"uninstall"}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let quarantine = data_removal_quarantine(&home).unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        drop(journal);

        assert!(!home.exists());
        assert!(quarantine.is_dir());

        let recovery = unfinished(&home).unwrap().unwrap();
        let final_journal = recovery
            .finish_resumed_deletion("data_removal_completed_by_repair")
            .unwrap();

        assert!(!home.exists());
        assert!(!quarantine.exists());
        assert!(final_journal.is_file());
        assert!(unfinished(&home).unwrap().is_none());
    }
}
