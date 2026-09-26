// SPDX-License-Identifier: Apache-2.0

pub mod service;
mod transaction;

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
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self},
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use self::{
    service::{RequestedService, ServicePlan, ServiceReport},
    transaction::{Journal, SetupLock, SnapshotSet},
};

const SETUP_MANIFEST_FORMAT: &str = "hardknock-setup-manifest-v1";
const SETUP_RESULT_SCHEMA: &str = "hardknock-setup-result-v1";
const SETUP_MANIFEST_RELATIVE: &str = "setup/manifest-v1.json";
const MAX_SETUP_MANIFEST_BYTES: u64 = 1024 * 1024;

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

#[derive(Clone, Copy, Debug)]
enum InitialHome {
    Missing,
    Empty { mode: u32 },
    Existing,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Intervention(message.into())
}

fn setup_manifest_path(home: &Path) -> PathBuf {
    home.join(SETUP_MANIFEST_RELATIVE)
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
                })
            } else {
                Ok(InitialHome::Existing)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(InitialHome::Missing),
        Err(error) => Err(error.into()),
    }
}

fn read_setup_manifest(home: &Path) -> Result<Option<SetupManifest>> {
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
    let manifest: SetupManifest = serde_json::from_slice(&fs::read(&path)?)?;
    if manifest.format != SETUP_MANIFEST_FORMAT || manifest.home != home {
        return Err(invalid(
            "Managed setup manifest format or data-home identity does not match",
        ));
    }
    Ok(Some(manifest))
}

fn write_setup_manifest(path: &Path, manifest: &SetupManifest) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid("Refusing symbolic link setup manifest"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Setup manifest has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let bytes = serde_json::to_vec_pretty(manifest)?;
    if bytes.len() as u64 > MAX_SETUP_MANIFEST_BYTES {
        return Err(invalid("Setup manifest exceeds the supported size"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
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
    const CREATED_DIRECTORIES: &[&str] = &[
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
    match initial {
        InitialHome::Existing => Ok(()),
        InitialHome::Missing => {
            for relative in CREATED_DIRECTORIES {
                match fs::remove_dir(home.join(relative)) {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            match fs::remove_dir(home) {
                Ok(()) => Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
        InitialHome::Empty { mode } => {
            for relative in CREATED_DIRECTORIES {
                match fs::remove_dir(home.join(relative)) {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let metadata = fs::symlink_metadata(home)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != geteuid().as_raw()
            {
                return Err(invalid(
                    "Hardknock home changed while setup rollback was running",
                ));
            }
            let current_mode = metadata.mode() & 0o777;
            if current_mode != mode && current_mode != 0o700 {
                return Err(invalid(
                    "Hardknock home permissions changed concurrently; current mode was preserved",
                ));
            }
            fs::set_permissions(home, fs::Permissions::from_mode(mode))?;
            Ok(())
        }
    }
}

fn remove_setup_manifest(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != geteuid().as_raw()
                || metadata.mode() & 0o777 != 0o600
            {
                return Err(invalid("Refusing unsafe managed setup manifest"));
            }
            fs::remove_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
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
    let initial_home = inspect_initial_home(home)?;
    let prior = read_setup_manifest(home)?;
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

    let _setup_lock = SetupLock::acquire(home)?;
    let include_new_store_files = !matches!(initial_home, InitialHome::Existing);
    let mut snapshots = SnapshotSet::capture(managed_snapshot_paths(
        &integrations,
        &service,
        home,
        include_new_store_files,
    ))?;
    let mut journal = Journal::begin(home, operation.name(), &plan_value(&plan)?)?;
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
        let store = Store::open(home)?;
        if include_new_store_files {
            snapshots.checkpoint(store_transaction_paths(home))?;
        }
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
            journal.record("recovery_point_created", serde_json::to_value(&backup)?)?;
            recovery_point = Some(backup);
        }

        let mut applied = Vec::new();
        for integration in &integrations {
            let report = install::apply(integration)?;
            snapshots.checkpoint(integration.managed_paths.clone())?;
            journal.record("integration_applied", report.clone())?;
            applied.push(report);
        }
        let manifest = setup_manifest(home, executable, args.mode, &agents, &service);
        write_setup_manifest(&setup_manifest_path(home), &manifest)?;
        snapshots.checkpoint(vec![setup_manifest_path(home)])?;
        journal.record(
            "setup_manifest_written",
            json!({"path":setup_manifest_path(home)}),
        )?;

        let mut service_report = service.apply(args.start).await?;
        service_applied = service_report.changed || args.start;
        snapshots.checkpoint(service.managed_paths())?;
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
        Ok((applied, recovery_point, service_report, health, exit_code, warnings)) => {
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
            if service_applied && service.uninstall(true).await.is_ok() {
                let _ = snapshots.checkpoint(service.managed_paths());
            }
            let file_rollback = snapshots.rollback();
            let home_rollback = rollback_home(home, initial_home);
            let _ = journal.record(
                "rollback_finished",
                json!({
                    "managed_files":file_rollback.as_ref().map(|_| "restored").unwrap_or("failed"),
                    "home":home_rollback.as_ref().map(|_| "restored").unwrap_or("failed")
                }),
            );
            if let Err(cleanup) = file_rollback.and(home_rollback) {
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

fn remove_data_home(home: &Path, prior: &SetupManifest) -> Result<()> {
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
    fs::remove_dir_all(home)?;
    Ok(())
}

pub async fn uninstall(
    home: &Path,
    user_home: &Path,
    executable: &Path,
    args: &UninstallArgs,
) -> Result<SetupResult> {
    let prior = read_setup_manifest(home)?;
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

    let _setup_lock = SetupLock::acquire(home)?;
    let mut snapshots =
        SnapshotSet::capture(managed_snapshot_paths(&integrations, &service, home, false))?;
    let mut journal = Journal::begin(home, Operation::Uninstall.name(), &plan_value(&plan)?)?;
    let mut bridge_was_running = false;
    let result = async {
        let mut bridge = crate::bridge::transport::BridgeClient::new(home);
        bridge.timeout = Duration::from_secs(2);
        bridge_was_running = bridge
            .request(crate::bridge::protocol::AgentEvent::Status)
            .await
            .is_ok();
        let bridge_stopped = !bridge_was_running
            || bridge
                .request(crate::bridge::protocol::AgentEvent::Shutdown)
                .await
                .is_ok();
        journal.record("bridge_stop_requested", json!({"accepted":bridge_stopped}))?;
        let service_report = service.uninstall(true).await?;
        if !service_report.manager_ready {
            return Err(invalid(service_report.fallback.clone().unwrap_or_else(
                || "Could not stop the managed Bridge service".into(),
            )));
        }
        snapshots.checkpoint(service.managed_paths())?;
        journal.record("service_removed", serde_json::to_value(&service_report)?)?;
        let mut applied = Vec::new();
        for integration in &integrations {
            let report = install::apply(integration)?;
            snapshots.checkpoint(integration.managed_paths.clone())?;
            journal.record("integration_removed", report.clone())?;
            applied.push(report);
        }
        remove_setup_manifest(&setup_manifest_path(home))?;
        snapshots.checkpoint(vec![setup_manifest_path(home)])?;
        journal.record("setup_manifest_removed", json!({}))?;
        if args.remove_data {
            journal.record("data_home_removal_started", json!({"home":home}))?;
            remove_data_home(
                home,
                prior
                    .as_ref()
                    .ok_or_else(|| invalid("Missing managed setup manifest"))?,
            )?;
        }
        Ok::<_, Error>((service_report, applied))
    }
    .await;
    match result {
        Ok((service_report, applied)) => {
            let journal_path = if args.remove_data {
                journal.record("finished", json!({"outcome":"succeeded"}))?;
                Some(journal.path().to_path_buf())
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
            let file_rollback = snapshots.rollback();
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
            let cleanup = match (file_rollback, bridge_restore) {
                (Ok(()), Ok(())) => None,
                (Err(error), Ok(())) | (Ok(()), Err(error)) => Some(error),
                (Err(files), Err(bridge)) => Some(invalid(format!(
                    "Managed-file rollback failed: {files}; Bridge restoration failed: {bridge}"
                ))),
            };
            if let Some(cleanup) = cleanup {
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
    use super::*;

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
        write_setup_manifest(&path, &manifest).unwrap();
        let loaded = read_setup_manifest(&home).unwrap().unwrap();
        assert_eq!(loaded.home, home);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
        assert!(!home.join("setup").exists());
    }
}
