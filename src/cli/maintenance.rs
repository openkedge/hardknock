// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};

use clap::Subcommand;
use fs2::FileExt;
use rusqlite::OpenFlags;
use serde_json::{Value, json};

use crate::{
    Error, Result,
    bridge::{config::Config, protocol::AgentEvent, transport::BridgeClient},
    capability::{ContainerRuntimeLifecycle, ContainerRuntimeMetadata},
    curriculum::{CurriculumQuery, CurriculumStatus},
    doctor::{
        self, BackupInput, BackupVerification, Check, CheckStatus, DiskInput, DiskThresholds,
        Input, PrivatePathInput, ReleaseIntegrityInput, Report, Severity,
    },
    integrations, storage,
    storage_policy::{PruneMode, StoragePolicyError},
    store::{CapabilityStore, CurriculumStore, ExperimentStore, Store},
};

#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    /// Inspect whether opening this home would migrate it; never mutates the home.
    DryRun,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Inspect artifact usage, reclaimable data, and configured limits.
    Status,
    /// Plan retention, or explicitly apply the bounded plan.
    Prune {
        #[arg(long)]
        apply: bool,
    },
    /// Check whether a prospective artifact write fits without deleting data.
    CheckCapacity {
        #[arg(long)]
        bytes: u64,
        #[arg(long, default_value_t = 1)]
        files: u64,
    },
}

fn storage_error(error: StoragePolicyError) -> Error {
    Error::Intervention(error.to_string())
}

pub fn backup(home: &Path, destination: &Path) -> Result<Value> {
    let report = storage::create_backup(home, destination)?;
    Ok(json!({"kind":"backup","backup":report}))
}

pub fn restore(backup: &Path, target: &Path, verify: bool) -> Result<Value> {
    if !verify {
        return Err(Error::Intervention(
            "Restore requires --verify and always verifies before changing the target home.".into(),
        ));
    }
    let report = storage::restore_backup(backup, target)?;
    Ok(json!({"kind":"restore","restore":report}))
}

pub fn migration(command: &MigrationCommand, home: &Path) -> Result<Value> {
    match command {
        MigrationCommand::DryRun => {
            let plan = storage::migration_plan(home)?;
            Ok(json!({"kind":"migration_dry_run","plan":plan}))
        }
    }
}

pub fn storage(command: &StorageCommand, store: &Store) -> Result<Value> {
    let policy = Config::load(&store.home)?.storage;
    let artifacts = store.home.join("artifacts");
    let _capacity = storage::acquire_artifact_capacity_lock(&store.home)?;
    let reservations = storage::active_artifact_reservations_locked(&store.home)?;
    match command {
        StorageCommand::Status => {
            let inventory = policy.inventory(&artifacts).map_err(storage_error)?;
            Ok(
                json!({"kind":"storage_status","policy":policy,"inventory":inventory,"active_reservations":reservations}),
            )
        }
        StorageCommand::Prune { apply } => {
            if *apply && reservations.count > 0 {
                return Err(Error::Intervention(format!(
                    "Retention apply requires artifact writers to be idle; {} active operation(s) reserve {} bytes and {} files.",
                    reservations.count, reservations.usage.bytes, reservations.usage.files
                )));
            }
            let mode = if *apply {
                PruneMode::Apply
            } else {
                PruneMode::DryRun
            };
            let report = policy.prune(&artifacts, mode).map_err(storage_error)?;
            Ok(
                json!({"kind":"storage_prune","policy":policy,"report":report,"active_reservations":reservations}),
            )
        }
        StorageCommand::CheckCapacity { bytes, files } => {
            let report = policy
                .ensure_capacity_with_reservations(&artifacts, *bytes, *files, reservations.usage)
                .map_err(storage_error)?;
            Ok(json!({"kind":"storage_capacity","policy":policy,"report":report}))
        }
    }
}

fn check(
    id: &str,
    status: CheckStatus,
    severity: Severity,
    required: bool,
    summary: impl Into<String>,
) -> Check {
    Check {
        id: id.into(),
        status,
        severity,
        required,
        summary: summary.into(),
        details: BTreeMap::new(),
    }
}

fn latest_backup(home: &Path, strict: bool) -> BackupInput {
    const MAXIMUM_AGE: u64 = 7 * 24 * 60 * 60;
    let directory = home.join("backups");
    let latest = fs::read_dir(&directory).ok().and_then(|entries| {
        entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .max_by_key(|(modified, _)| *modified)
    });
    let Some((modified, path)) = latest else {
        return BackupInput {
            age_seconds: None,
            maximum_age_seconds: MAXIMUM_AGE,
            bundle_verification: BackupVerification::Unavailable {
                reason: "No backup exists under the managed backups directory".into(),
            },
            staged_restore: BackupVerification::Unavailable {
                reason: "No backup exists to stage for a restore drill".into(),
            },
            required: strict,
        };
    };
    let age_seconds = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    match storage::verify_backup(&path) {
        Ok(manifest) => {
            let staged_restore = if strict {
                match staged_restore_drill(&path, &manifest) {
                    Ok(()) => BackupVerification::Verified,
                    Err(error) => BackupVerification::Failed {
                        reason: error.to_string(),
                    },
                }
            } else {
                BackupVerification::Unavailable {
                    reason:
                        "Run doctor --strict to copy the verified backup into an isolated staging directory and reopen its database"
                            .into(),
                }
            };
            BackupInput {
                age_seconds: Some(
                    chrono::Utc::now()
                        .signed_duration_since(manifest.created_at)
                        .num_seconds()
                        .max(0) as u64,
                ),
                maximum_age_seconds: MAXIMUM_AGE,
                bundle_verification: BackupVerification::Verified,
                staged_restore,
                required: strict,
            }
        }
        Err(error) => BackupInput {
            age_seconds: Some(age_seconds),
            maximum_age_seconds: MAXIMUM_AGE,
            bundle_verification: BackupVerification::Failed {
                reason: error.to_string(),
            },
            staged_restore: BackupVerification::Unavailable {
                reason:
                    "The staged restore drill was not attempted because bundle verification failed"
                        .into(),
            },
            required: strict,
        },
    }
}

fn staged_restore_drill(path: &Path, manifest: &storage::BackupManifest) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix("hardknock-restore-drill-")
        .tempdir()?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;

    let database_source = path.join(validated_backup_relative_path(&manifest.database.path)?);
    let database_target = staging.path().join("hardknock.db");
    copy_backup_file_for_drill(
        &database_source,
        &database_target,
        &manifest.database,
        staging.path(),
    )?;

    for artifact in &manifest.artifacts {
        let relative = validated_backup_relative_path(&artifact.path)?;
        let destination = staging.path().join(&relative);
        copy_backup_file_for_drill(&path.join(relative), &destination, artifact, staging.path())?;
    }

    let connection = rusqlite::Connection::open_with_flags(
        &database_target,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::Intervention(format!(
            "Staged restore database integrity check failed: {integrity}"
        )));
    }
    let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign_keys.query([])?.next()?.is_some() {
        return Err(Error::Intervention(
            "Staged restore database contains foreign-key violations.".into(),
        ));
    }
    Ok(())
}

fn validated_backup_relative_path(value: &str) -> Result<PathBuf> {
    if value.is_empty() {
        return Err(Error::Intervention(
            "Verified backup contains an empty file path.".into(),
        ));
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Intervention(format!(
            "Verified backup contains a non-relative file path: {value}"
        )));
    }
    Ok(path.to_path_buf())
}

fn copy_backup_file_for_drill(
    source: &Path,
    destination: &Path,
    expected: &storage::BackupFile,
    staging: &Path,
) -> Result<()> {
    create_private_staging_parents(staging, destination)?;
    let before = fs::symlink_metadata(source)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(Error::Intervention(format!(
            "Verified backup file is no longer a regular non-symlink: {}",
            source.display()
        )));
    }
    if before.uid() != nix::unistd::geteuid().as_raw()
        || before.nlink() != 1
        || before.mode() & 0o777 != 0o600
    {
        return Err(Error::Intervention(format!(
            "Verified backup file ownership, links, or mode changed before the staged restore drill: {}",
            source.display()
        )));
    }

    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(source)?;
    let opened = input.metadata()?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() || opened.nlink() != 1 {
        return Err(Error::Intervention(format!(
            "Verified backup file changed while opening it for the staged restore drill: {}",
            source.display()
        )));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(destination)?;
    output.set_permissions(fs::Permissions::from_mode(0o600))?;

    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| Error::Intervention("Staged restore byte count overflowed.".into()))?;
    }
    output.flush()?;
    output.sync_all()?;
    let digest = hasher.finalize().to_hex().to_string();
    if bytes != expected.bytes || digest != expected.blake3 {
        return Err(Error::Intervention(format!(
            "Staged restore copy does not match verified backup metadata: {}",
            expected.path
        )));
    }

    let after = input.metadata()?;
    let current = fs::symlink_metadata(source)?;
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.len() != before.len()
        || after.mtime() != before.mtime()
        || after.mtime_nsec() != before.mtime_nsec()
        || current.file_type().is_symlink()
        || current.dev() != before.dev()
        || current.ino() != before.ino()
        || current.nlink() != 1
    {
        return Err(Error::Intervention(format!(
            "Verified backup file changed during the staged restore drill: {}",
            source.display()
        )));
    }
    Ok(())
}

fn create_private_staging_parents(staging: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Intervention("Staged restore destination has no parent.".into()))?;
    let relative = parent
        .strip_prefix(staging)
        .map_err(|_| Error::Intervention("Staged restore destination escaped staging.".into()))?;
    let mut current = staging.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Intervention(
                "Staged restore destination is not normalized.".into(),
            ));
        };
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_dir()
                    || metadata.uid() != nix::unistd::geteuid().as_raw()
                    || metadata.mode() & 0o777 != 0o700
                {
                    return Err(Error::Intervention(format!(
                        "Staged restore directory is unsafe: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn release_integrity_input() -> ReleaseIntegrityInput {
    if let Some(path) = std::env::var_os("HARDKNOCK_INSTALL_MANIFEST") {
        return ReleaseIntegrityInput::Managed {
            manifest_path: PathBuf::from(path),
        };
    }
    let executable = std::env::current_exe().ok();
    if let Some(executable) = &executable
        && let Some(binary_directory) = executable.parent()
        && let Some(prefix) = binary_directory.parent()
    {
        let manifest_path = prefix.join("share/hardknock/install-manifest-v1");
        let managed_layout = binary_directory
            .file_name()
            .is_some_and(|name| name == "bin");
        if managed_layout || fs::symlink_metadata(&manifest_path).is_ok() {
            return ReleaseIntegrityInput::Managed { manifest_path };
        }
    }
    ReleaseIntegrityInput::SourceBuild {
        executable_path: executable,
    }
}

fn stale_resources(store: &Store, bridge_reachable: bool) -> Result<Vec<String>> {
    let mut stale = Vec::new();
    if !bridge_reachable {
        for name in ["hardknock.sock", "bridge-token", "bridge-endpoint.json"] {
            let path = store.home.join("run").join(name);
            if fs::symlink_metadata(&path).is_ok() {
                stale.push(format!("Bridge runtime path remains: {}", path.display()));
            }
        }
    }

    for reality in store.realities()? {
        if reality.status == crate::core::RealityStatus::Discarded {
            continue;
        }
        if reality.execution_boundary.provider == "container" {
            match store.provider_runtime::<ContainerRuntimeMetadata>(&reality.id) {
                Ok(metadata) if metadata.lifecycle == ContainerRuntimeLifecycle::Pending => {
                    stale.push(format!(
                        "Container Reality runtime metadata remains pending: {} ({})",
                        reality.id, metadata.container_name
                    ));
                }
                Ok(_) => {}
                Err(Error::NotFound(_)) => stale.push(format!(
                    "Container Reality runtime metadata is missing: {}",
                    reality.id
                )),
                Err(error) => return Err(error),
            }
        }
        if !reality.ephemeral {
            continue;
        }
        let lock_path = store
            .home
            .join("locks")
            .join(format!("{}.lock", reality.id));
        let active = lease_is_active(&lock_path, "Reality", &mut stale)?;
        if !active {
            stale.push(format!(
                "Unlocked ephemeral Reality requires reconciliation: {} ({})",
                reality.id,
                reality.root.display()
            ));
        }
    }

    for experiment in ExperimentStore::list(store, None)? {
        if !experiment.status.terminal()
            && !lease_is_active(
                &store
                    .home
                    .join("locks")
                    .join(format!("{}.lock", experiment.id)),
                "experiment",
                &mut stale,
            )?
        {
            stale.push(format!(
                "Nonterminal experiment is not demonstrably active: {} ({:?})",
                experiment.id, experiment.status
            ));
        }
    }
    let curricula = CurriculumStore::list(store, CurriculumQuery::default())?;
    let running_curricula = curricula
        .iter()
        .filter(|curriculum| curriculum.status == CurriculumStatus::Running)
        .count();
    let curriculum_executor_active = if running_curricula > 0 {
        lease_is_active(
            &store.home.join("locks").join("curriculum-executor.lock"),
            "curriculum executor",
            &mut stale,
        )?
    } else {
        false
    };
    for curriculum in curricula {
        if curriculum.status == CurriculumStatus::Running
            && !(running_curricula == 1 && curriculum_executor_active)
        {
            stale.push(format!(
                "Running curriculum is not demonstrably active: {}",
                curriculum.id
            ));
        }
    }
    Ok(stale)
}

fn lease_is_active(path: &Path, label: &str, stale: &mut Vec<String>) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let expected_uid = nix::unistd::geteuid().as_raw();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != expected_uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        stale.push(format!("Unsafe {label} lease path: {}", path.display()));
        return Ok(false);
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !opened.is_file()
        || opened.uid() != expected_uid
        || opened.nlink() != 1
        || opened.mode() & 0o777 != 0o600
        || current.file_type().is_symlink()
        || !current.is_file()
        || current.uid() != expected_uid
        || current.nlink() != 1
        || current.mode() & 0o777 != 0o600
        || opened.dev() != current.dev()
        || opened.ino() != current.ino()
    {
        stale.push(format!("Unsafe {label} lease path: {}", path.display()));
        return Ok(false);
    }
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn bridge_check(status: &Value) -> Check {
    if status["status"] == "running" && status["persistence_error"].is_null() {
        return check(
            "bridge.liveness",
            CheckStatus::Passed,
            Severity::Informational,
            false,
            "Bridge is reachable and reports healthy persistence",
        );
    }
    if let Some(error) = status["persistence_error"].as_str() {
        let mut result = check(
            "bridge.liveness",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "Bridge reports a persistence failure",
        );
        result.details.insert("error".into(), error.into());
        return result;
    }
    let mut result = check(
        "bridge.liveness",
        CheckStatus::Warning,
        Severity::Warning,
        false,
        "Bridge is not reachable; start it or configure an on-demand workflow",
    );
    if let Some(reason) = status["reason"].as_str() {
        result.details.insert("reason".into(), reason.into());
    }
    result
}

fn adapter_check(status: &Value, config: &Config) -> Check {
    if status["configuration"]["valid"] == false {
        let mut result = check(
            "integrations.compatibility",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "Integration configuration is invalid",
        );
        if let Some(error) = status["configuration"]["error"].as_str() {
            result.details.insert("error".into(), error.into());
        }
        return result;
    }

    let installed: Vec<_> = status["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|agent| {
            let name = agent["agent"].as_str().unwrap_or_default();
            if name == "codex" {
                config
                    .integrations
                    .get(name)
                    .is_some_and(|adapter| adapter.enabled)
            } else {
                agent["installed"] == true
            }
        })
        .collect();
    let unsupported: Vec<_> = installed
        .iter()
        .filter(|agent| {
            agent["executable_found"] == false || agent["compatibility"]["supported"] == false
        })
        .filter_map(|agent| agent["agent"].as_str())
        .collect();
    if !unsupported.is_empty() {
        let mut result = check(
            "integrations.compatibility",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "One or more installed adapters are incompatible",
        );
        result
            .details
            .insert("agents".into(), unsupported.join(","));
        return result;
    }
    if installed
        .iter()
        .any(|agent| agent["agent"] != "codex" && agent["native_host_load_verified"].is_null())
    {
        return check(
            "integrations.compatibility",
            CheckStatus::Unavailable,
            Severity::Warning,
            false,
            "Managed adapter files are valid; live host loading requires an agent session",
        );
    }
    check(
        "integrations.compatibility",
        CheckStatus::Passed,
        Severity::Informational,
        false,
        if installed.is_empty() {
            "No installed adapters require compatibility validation"
        } else {
            "Installed adapters pass available compatibility checks"
        },
    )
}

pub async fn doctor(store: &Store, mut base: Value, strict: bool) -> Result<Value> {
    let config = Config::load(&store.home)?;
    let mut client = BridgeClient::new(&store.home);
    client.timeout = Duration::from_secs(2);
    let bridge_status = client
        .request(AgentEvent::Status)
        .await
        .unwrap_or_else(|error| json!({"status":"unavailable","reason":error.to_string()}));
    let bridge_reachable = bridge_status["status"] == "running";
    let integration_status = integrations::status(&store.home, true).await?;
    let stale = stale_resources(store, bridge_reachable)?;
    let _capacity = storage::acquire_artifact_capacity_lock(&store.home)?;
    let reservations = storage::active_artifact_reservations_locked(&store.home)?;
    let inventory = config
        .storage
        .inventory(store.home.join("artifacts"))
        .map_err(storage_error)?;
    let reserved_capacity = config.storage.ensure_capacity_with_reservations(
        store.home.join("artifacts"),
        0,
        0,
        reservations.usage,
    );
    drop(_capacity);

    let mut private_paths = vec![
        PrivatePathInput::home(&store.home),
        PrivatePathInput::database(store.home.join("hardknock.db")),
        PrivatePathInput::configuration(store.home.join("config.toml"), false),
        PrivatePathInput::runtime_directory("runtime", store.home.join("run"), true),
    ];
    for directory in ["artifacts", "backups", "locks", "logs"] {
        private_paths.push(PrivatePathInput::runtime_directory(
            directory,
            store.home.join(directory),
            true,
        ));
    }
    for name in ["bridge-token", "bridge-endpoint.json", "bridge.lock"] {
        let path = store.home.join("run").join(name);
        if fs::symlink_metadata(&path).is_ok() {
            private_paths.push(PrivatePathInput::runtime_file(name, path, false));
        }
    }
    let warning_available_bytes = config
        .storage
        .min_free_bytes
        .saturating_add(512 * 1024 * 1024);
    let input = Input {
        expected_schema: crate::store::LATEST_SCHEMA_VERSION,
        actual_schema: Some(store.applied_schema_version()?),
        private_paths,
        disk: Some(DiskInput {
            path: store.home.clone(),
            thresholds: DiskThresholds {
                minimum_available_bytes: config.storage.min_free_bytes,
                warning_available_bytes,
            },
        }),
        stale_runtime_paths: Some(stale),
        backup: Some(latest_backup(&store.home, strict)),
        release: release_integrity_input(),
    };
    let mut checks = doctor::run(&input).checks;
    checks.push(bridge_check(&bridge_status));
    checks.push(adapter_check(&integration_status, &config));
    checks.push(if inventory.unsafe_entries > 0 {
        let mut result = check(
            "storage.artifact_policy",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "Artifact inventory contains unsafe entries",
        );
        result.details.insert(
            "unsafe_entries".into(),
            inventory.unsafe_entries.to_string(),
        );
        result
    } else if let Err(error) = reserved_capacity {
        let mut result = check(
            "storage.artifact_policy",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "Artifact usage plus active reservations exceeds configured quotas or free-space limits",
        );
        result.details.insert("error".into(), error.to_string());
        result
    } else if !inventory.limits.within_limits {
        check(
            "storage.artifact_policy",
            CheckStatus::Failed,
            Severity::Error,
            true,
            "Artifact usage exceeds configured quotas or free-space limits",
        )
    } else {
        check(
            "storage.artifact_policy",
            CheckStatus::Passed,
            Severity::Informational,
            true,
            "Artifact inventory is within configured limits",
        )
    });
    let report = Report::from_checks(checks);

    base["strict"] = json!(strict);
    base["report"] = serde_json::to_value(report)?;
    base["bridge"] = bridge_status;
    base["integrations"] = integration_status;
    base["storage"] =
        json!({"policy":config.storage,"inventory":inventory,"active_reservations":reservations});
    Ok(base)
}
