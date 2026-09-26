// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use nix::unistd::geteuid;
use rusqlite::{Connection, MAIN_DB, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    dojo::resolve_home,
    store::{LATEST_SCHEMA_VERSION, validate_dedicated_home},
};

pub const BACKUP_FORMAT: &str = "hardknock.backup";
pub const BACKUP_FORMAT_VERSION: u32 = 1;

const MANIFEST_FILE: &str = "manifest.json";
const DATABASE_FILE: &str = "database.sqlite3";
const RESTORED_DATABASE_FILE: &str = "hardknock.db";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BACKUP_FILES: usize = 100_000;
const BACKUP_FIXED_ENTRY_COUNT: usize = 3;
const MAX_BACKUP_ENTRIES: usize = MAX_BACKUP_FILES + BACKUP_FIXED_ENTRY_COUNT;
const MAX_BACKUP_SOURCE_ENTRIES: usize = MAX_BACKUP_ENTRIES - BACKUP_FIXED_ENTRY_COUNT;
const MAX_BACKUP_NESTING_DEPTH: usize = 64;
const MAX_MANIFEST_PATH_BYTES: usize = 4096;
const MAINTENANCE_LOCK_FILE: &str = "maintenance.lock";
const ARTIFACT_CAPACITY_LOCK_FILE: &str = "artifact-capacity.lock";
const ARTIFACT_RESERVATIONS_DIRECTORY: &str = "artifact-reservations";
const ARTIFACT_RESERVATION_FORMAT: &str = "hardknock.artifact-reservation.v1";
const MAX_ARTIFACT_RESERVATION_BYTES: u64 = 4096;
const MAINTENANCE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAINTENANCE_LOCK_RETRY: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupFile {
    pub path: String,
    pub blake3: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format: String,
    pub format_version: u32,
    pub package_version: String,
    pub schema_version: i64,
    pub created_at: DateTime<Utc>,
    pub source_home: String,
    pub database: BackupFile,
    pub artifacts: Vec<BackupFile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackupReport {
    pub destination: PathBuf,
    pub schema_version: i64,
    pub package_version: String,
    pub artifact_count: usize,
    pub total_bytes: u64,
    pub verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RestoreReport {
    pub source: PathBuf,
    pub target: PathBuf,
    pub schema_version: i64,
    pub artifact_count: usize,
    pub verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MigrationPlan {
    pub current_schema: i64,
    pub target_schema: i64,
    pub migration_required: bool,
    pub backup_required: bool,
    pub database_exists: bool,
    pub package_version: String,
}

struct VerifiedBackup {
    root: PathBuf,
    manifest: BackupManifest,
}

#[derive(Clone, Copy)]
struct TraversalLimits {
    max_entries: usize,
    max_depth: usize,
}

struct TraversalBudget {
    label: &'static str,
    limits: TraversalLimits,
    entries: usize,
}

impl TraversalBudget {
    fn new(label: &'static str, limits: TraversalLimits) -> Self {
        Self {
            label,
            limits,
            entries: 0,
        }
    }

    fn record(&mut self, depth: usize) -> Result<()> {
        if depth > self.limits.max_depth {
            return Err(Error::Intervention(format!(
                "{} nesting exceeds the supported depth of {}.",
                self.label, self.limits.max_depth
            )));
        }
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| Error::Intervention("Backup entry count overflowed.".into()))?;
        if self.entries > self.limits.max_entries {
            return Err(Error::Intervention(format!(
                "{} total entry count exceeds the supported limit of {}.",
                self.label, self.limits.max_entries
            )));
        }
        Ok(())
    }
}

struct CopyTraversalEntry {
    source: PathBuf,
    destination: PathBuf,
    depth: usize,
}

struct InventoryTraversalEntry {
    path: PathBuf,
    depth: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReservationRecord {
    format: String,
    usage: crate::storage_policy::StorageUsage,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct ActiveArtifactReservations {
    pub count: u64,
    pub usage: crate::storage_policy::StorageUsage,
    pub removed_stale: u64,
}

#[derive(Clone, Copy)]
enum RestoreTarget {
    Missing,
    Empty { device: u64, inode: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileState {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DatabaseState {
    database: FileState,
    wal: Option<FileState>,
    shared_memory: Option<FileState>,
}

pub(crate) fn current_schema_version(connection: &Connection) -> Result<i64> {
    let table_exists: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if table_exists == 0 {
        return Ok(0);
    }
    let version: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if version < 0 {
        return Err(Error::Intervention(
            "Database schema version must not be negative.".into(),
        ));
    }
    Ok(version)
}

pub fn migration_plan(home: &Path) -> Result<MigrationPlan> {
    let home = resolve_without_mutation(home)?;
    if !home.exists() {
        return Ok(MigrationPlan {
            current_schema: 0,
            target_schema: LATEST_SCHEMA_VERSION,
            migration_required: true,
            backup_required: false,
            database_exists: false,
            package_version: env!("CARGO_PKG_VERSION").into(),
        });
    }
    reject_symlink(&home, "Hardknock data home")?;
    validate_dedicated_home(&home)?;
    let database = home.join(RESTORED_DATABASE_FILE);
    if !database.exists() {
        return Ok(MigrationPlan {
            current_schema: 0,
            target_schema: LATEST_SCHEMA_VERSION,
            migration_required: true,
            backup_required: false,
            database_exists: false,
            package_version: env!("CARGO_PKG_VERSION").into(),
        });
    }
    reject_symlink(&database, "Hardknock database")?;
    let current_schema = schema_version_without_mutation(&database)?;
    if current_schema > LATEST_SCHEMA_VERSION {
        return Err(Error::Intervention(format!(
            "Database schema {current_schema} is newer than the latest supported schema {LATEST_SCHEMA_VERSION}; upgrade the CLI."
        )));
    }
    Ok(MigrationPlan {
        current_schema,
        target_schema: LATEST_SCHEMA_VERSION,
        migration_required: current_schema < LATEST_SCHEMA_VERSION,
        backup_required: current_schema > 0 && current_schema < LATEST_SCHEMA_VERSION,
        database_exists: true,
        package_version: env!("CARGO_PKG_VERSION").into(),
    })
}

pub fn create_backup(home: &Path, destination: &Path) -> Result<BackupReport> {
    let home = canonical_source_home(home)?;
    let _maintenance = acquire_home_maintenance_lock(&home)?;
    let _artifact_capacity = acquire_artifact_capacity_lock(&home)?;
    require_no_active_artifact_reservations_locked(&home, "Backup")?;
    create_backup_internal(&home, destination, false)
}

pub fn verify_backup(path: &Path) -> Result<BackupManifest> {
    let absolute = absolute_path(path)?;
    let _maintenance = acquire_directory_maintenance_lock(&absolute, "Backup bundle")?;
    Ok(verify_backup_internal(path)?.manifest)
}

pub fn restore_backup(path: &Path, target: &Path) -> Result<RestoreReport> {
    let absolute = absolute_path(path)?;
    let _backup_maintenance = acquire_directory_maintenance_lock(&absolute, "Backup bundle")?;
    let verified = verify_backup_internal(path)?;
    let target = resolve_restore_target(target)?;
    let recorded_home = PathBuf::from(&verified.manifest.source_home);
    if target != recorded_home {
        return Err(Error::Intervention(format!(
            "Backup contains absolute evidence paths for {}; restore to that exact empty home. Relocating backups is not supported yet.",
            recorded_home.display()
        )));
    }
    let parent = target
        .parent()
        .ok_or_else(|| Error::InvalidInput("Restore target has no parent directory".into()))?;
    let _target_maintenance =
        acquire_directory_maintenance_lock(parent, "Restore target parent directory")?;
    let target_state = inspect_restore_target(&target)?;
    let staging = tempfile::Builder::new()
        .prefix(".hardknock-restore-")
        .tempdir_in(parent)?;
    set_directory_mode(staging.path())?;
    let staged_artifacts = staging.path().join("artifacts");
    fs::create_dir(&staged_artifacts)?;
    set_directory_mode(&staged_artifacts)?;

    copy_verified_file(
        &verified.root.join(DATABASE_FILE),
        &staging.path().join(RESTORED_DATABASE_FILE),
        &verified.manifest.database,
    )?;
    for artifact in &verified.manifest.artifacts {
        let relative = validate_manifest_path(&artifact.path, true)?;
        let destination = staging.path().join(&relative);
        ensure_private_parent(staging.path(), &destination)?;
        copy_verified_file(&verified.root.join(&relative), &destination, artifact)?;
    }
    verify_restored_stage(staging.path(), &verified.manifest)?;
    sync_directory(staging.path())?;

    let staged_path = staging.keep();
    if let Err(error) = install_staged_home(&staged_path, &target, target_state) {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    Ok(RestoreReport {
        source: verified.root,
        target,
        schema_version: verified.manifest.schema_version,
        artifact_count: verified.manifest.artifacts.len(),
        verified: true,
    })
}

/// Create the recovery snapshot while the caller holds the home maintenance
/// and artifact-capacity locks for the complete migration transaction.
pub(crate) fn backup_before_migration_locked(
    home: &Path,
    current_schema: i64,
    target_schema: i64,
) -> Result<PathBuf> {
    if current_schema <= 0 || current_schema >= target_schema {
        return Err(Error::InvalidInput(
            "Pre-migration backup requires an existing older schema.".into(),
        ));
    }
    let directory = home.join("backups");
    fs::create_dir_all(&directory)?;
    set_directory_mode(&directory)?;
    let home = canonical_source_home(home)?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let destination = directory.join(format!(
        "schema-{current_schema}-to-{target_schema}-{timestamp}-{}.hkbak",
        uuid::Uuid::new_v4()
    ));
    let report = create_backup_internal(&home, &destination, true)?;
    if report.schema_version != current_schema {
        return Err(Error::Intervention(format!(
            "Pre-migration backup captured schema {}, expected {current_schema}.",
            report.schema_version
        )));
    }
    Ok(report.destination)
}

fn create_backup_internal(
    home: &Path,
    destination: &Path,
    allow_inside_home: bool,
) -> Result<BackupReport> {
    let home = canonical_source_home(home)?;
    let database = home.join(RESTORED_DATABASE_FILE);
    require_regular_file(&database, "Hardknock database")?;
    let destination = resolve_new_destination(destination)?;
    if !allow_inside_home && destination.starts_with(&home) {
        return Err(Error::Intervention(
            "Public backups must be written outside HARDKNOCK_HOME.".into(),
        ));
    }
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(Error::Intervention(format!(
            "Backup destination already exists: {}",
            destination.display()
        )));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| Error::InvalidInput("Backup destination has no parent directory".into()))?;
    let staging = tempfile::Builder::new()
        .prefix(".hardknock-backup-")
        .tempdir_in(parent)?;
    set_directory_mode(staging.path())?;
    let staged_artifacts = staging.path().join("artifacts");
    fs::create_dir(&staged_artifacts)?;
    set_directory_mode(&staged_artifacts)?;

    let source = open_read_only(&database)?;
    let schema_version = current_schema_version(&source)?;
    if schema_version == 0 {
        return Err(Error::Intervention(
            "Hardknock database has no applied schema to back up.".into(),
        ));
    }
    if schema_version > LATEST_SCHEMA_VERSION {
        return Err(Error::Intervention(format!(
            "Database schema {schema_version} is newer than the latest supported schema {LATEST_SCHEMA_VERSION}; upgrade the CLI."
        )));
    }
    let staged_database = staging.path().join(DATABASE_FILE);
    source.backup(MAIN_DB, &staged_database, None)?;
    normalize_snapshot_database(&staged_database)?;
    set_file_mode(&staged_database)?;
    let database_file = describe_file(staging.path(), &staged_database)?;

    let mut artifacts = Vec::new();
    copy_artifact_tree(
        &home.join("artifacts"),
        &staged_artifacts,
        staging.path(),
        &mut artifacts,
    )?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    if artifacts.len() > MAX_BACKUP_FILES {
        return Err(Error::Intervention(format!(
            "Backup contains more than {MAX_BACKUP_FILES} artifact files."
        )));
    }

    let source_home = home
        .to_str()
        .ok_or_else(|| Error::Intervention("HARDKNOCK_HOME must be valid UTF-8.".into()))?
        .to_owned();
    let manifest = BackupManifest {
        format: BACKUP_FORMAT.into(),
        format_version: BACKUP_FORMAT_VERSION,
        package_version: env!("CARGO_PKG_VERSION").into(),
        schema_version,
        created_at: Utc::now(),
        source_home,
        database: database_file,
        artifacts,
    };
    write_manifest(staging.path(), &manifest)?;
    sync_directory(staging.path())?;
    let verified = verify_backup_internal(staging.path())?;
    let total_bytes = verified.manifest.artifacts.iter().try_fold(
        verified.manifest.database.bytes,
        |total, file| {
            total
                .checked_add(file.bytes)
                .ok_or_else(|| Error::Intervention("Backup byte count overflowed.".into()))
        },
    )?;

    if fs::symlink_metadata(&destination).is_ok() {
        return Err(Error::Intervention(format!(
            "Backup destination appeared while the backup was being created: {}",
            destination.display()
        )));
    }
    let staged_path = staging.keep();
    if let Err(error) = rename_noreplace(&staged_path, &destination) {
        let _ = fs::remove_dir_all(&staged_path);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(Error::Intervention(format!(
                "Backup destination appeared while the backup was being created: {}",
                destination.display()
            )));
        }
        return Err(error.into());
    }
    sync_directory(parent)?;
    Ok(BackupReport {
        destination,
        schema_version: verified.manifest.schema_version,
        package_version: verified.manifest.package_version,
        artifact_count: verified.manifest.artifacts.len(),
        total_bytes,
        verified: true,
    })
}

fn verify_backup_internal(path: &Path) -> Result<VerifiedBackup> {
    let absolute = absolute_path(path)?;
    reject_symlink(&absolute, "Backup bundle")?;
    let metadata = fs::metadata(&absolute)?;
    if !metadata.is_dir() {
        return Err(Error::Intervention(
            "Backup bundle must be a directory.".into(),
        ));
    }
    require_owner(&metadata, "Backup bundle")?;
    require_mode(&metadata, 0o700, "Backup bundle")?;
    let root = absolute.canonicalize()?;
    let manifest_path = root.join(MANIFEST_FILE);
    require_regular_file(&manifest_path, "Backup manifest")?;
    let manifest_metadata = fs::metadata(&manifest_path)?;
    require_mode(&manifest_metadata, 0o600, "Backup manifest")?;
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Intervention(format!(
            "Backup manifest exceeds {MAX_MANIFEST_BYTES} bytes."
        )));
    }
    let mut manifest_bytes = Vec::new();
    File::open(&manifest_path)?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut manifest_bytes)?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(Error::Intervention(format!(
            "Backup manifest exceeds {MAX_MANIFEST_BYTES} bytes."
        )));
    }
    let manifest: BackupManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    verify_bundle_inventory(&root, &manifest)?;
    verify_manifest_file(&root, &manifest.database)?;
    for artifact in &manifest.artifacts {
        verify_manifest_file(&root, artifact)?;
    }
    verify_database(&root.join(DATABASE_FILE), &manifest, &manifest.artifacts)?;
    Ok(VerifiedBackup { root, manifest })
}

fn validate_manifest(manifest: &BackupManifest) -> Result<()> {
    if manifest.format != BACKUP_FORMAT || manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err(Error::Intervention(format!(
            "Unsupported backup format {} version {}.",
            manifest.format, manifest.format_version
        )));
    }
    semver::Version::parse(&manifest.package_version)
        .map_err(|_| Error::Intervention("Backup package version is invalid.".into()))?;
    if manifest.schema_version <= 0 || manifest.schema_version > LATEST_SCHEMA_VERSION {
        return Err(Error::Intervention(format!(
            "Backup schema {} is not supported by this Hardknock release.",
            manifest.schema_version
        )));
    }
    let source_home = Path::new(&manifest.source_home);
    if !source_home.is_absolute()
        || source_home
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Intervention(
            "Backup source home must be a normalized absolute path.".into(),
        ));
    }
    if manifest.database.path != DATABASE_FILE {
        return Err(Error::Intervention(
            "Backup database path is invalid.".into(),
        ));
    }
    validate_backup_file(&manifest.database, false)?;
    if manifest.artifacts.len() > MAX_BACKUP_FILES {
        return Err(Error::Intervention(format!(
            "Backup contains more than {MAX_BACKUP_FILES} artifact files."
        )));
    }
    let mut previous = None;
    for artifact in &manifest.artifacts {
        validate_backup_file(artifact, true)?;
        if previous.is_some_and(|path| path >= artifact.path.as_str()) {
            return Err(Error::Intervention(
                "Backup artifact paths must be sorted and unique.".into(),
            ));
        }
        previous = Some(artifact.path.as_str());
    }
    Ok(())
}

fn validate_backup_file(file: &BackupFile, artifact: bool) -> Result<()> {
    validate_manifest_path(&file.path, artifact)?;
    if file.blake3.len() != 64
        || !file
            .blake3
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Intervention(format!(
            "Backup file {} has an invalid BLAKE3 hash.",
            file.path
        )));
    }
    Ok(())
}

fn validate_manifest_path(value: &str, artifact: bool) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > MAX_MANIFEST_PATH_BYTES
        || value.contains('\\')
        || value.starts_with('/')
        || value.ends_with('/')
    {
        return Err(Error::Intervention(format!(
            "Backup path is invalid: {value}"
        )));
    }
    let path = Path::new(value);
    let mut normalized = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Intervention(format!(
                "Backup path contains traversal: {value}"
            )));
        };
        normalized.push(
            component
                .to_str()
                .ok_or_else(|| Error::Intervention("Backup paths must be valid UTF-8.".into()))?,
        );
    }
    if normalized.len() > MAX_BACKUP_NESTING_DEPTH {
        return Err(Error::Intervention(format!(
            "Backup path nesting exceeds the supported depth of {MAX_BACKUP_NESTING_DEPTH}: {value}"
        )));
    }
    if normalized.join("/") != value
        || (artifact && normalized.first().copied() != Some("artifacts"))
        || (!artifact && value != DATABASE_FILE)
    {
        return Err(Error::Intervention(format!(
            "Backup path is outside its allowed location: {value}"
        )));
    }
    Ok(path.to_owned())
}

fn verify_bundle_inventory(root: &Path, manifest: &BackupManifest) -> Result<()> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_inventory(root, root, &mut files, &mut directories)?;

    let mut expected_files = BTreeSet::from([MANIFEST_FILE.into(), DATABASE_FILE.into()]);
    expected_files.extend(manifest.artifacts.iter().map(|file| file.path.clone()));
    let mut expected_directories = BTreeSet::from(["artifacts".into()]);
    for file in &manifest.artifacts {
        let mut parent = Path::new(&file.path).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            expected_directories.insert(path_to_manifest(path)?);
            parent = path.parent();
        }
    }
    if files != expected_files || !expected_directories.is_subset(&directories) {
        let missing_files: Vec<_> = expected_files.difference(&files).cloned().collect();
        let unexpected_files: Vec<_> = files.difference(&expected_files).cloned().collect();
        let missing_directories: Vec<_> = expected_directories
            .difference(&directories)
            .cloned()
            .collect();
        return Err(Error::Intervention(format!(
            "Backup contents do not match the manifest inventory (missing files: {missing_files:?}; unexpected files: {unexpected_files:?}; missing directories: {missing_directories:?})."
        )));
    }
    Ok(())
}

fn collect_inventory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    collect_inventory_with_limits(
        root,
        directory,
        files,
        directories,
        TraversalLimits {
            max_entries: MAX_BACKUP_ENTRIES,
            max_depth: MAX_BACKUP_NESTING_DEPTH,
        },
    )
}

fn collect_inventory_with_limits(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
    limits: TraversalLimits,
) -> Result<()> {
    let mut budget = TraversalBudget::new("Backup inventory", limits);
    let mut stack = Vec::new();
    push_inventory_children(&mut stack, directory, 0, &mut budget)?;

    while let Some(entry) = stack.pop() {
        let path = entry.path;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Intervention(format!(
                "Backup bundles must not contain symlinks: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::Intervention("Backup inventory escaped its root.".into()))?;
        let manifest_path = path_to_manifest(relative)?;
        if metadata.is_dir() {
            require_owner(&metadata, "Backup directory")?;
            require_mode(&metadata, 0o700, "Backup directory")?;
            directories.insert(manifest_path);
            push_inventory_children(&mut stack, &path, entry.depth, &mut budget)?;
        } else if metadata.is_file() {
            require_owner(&metadata, "Backup file")?;
            require_single_link(&metadata, "Backup file")?;
            require_mode(&metadata, 0o600, "Backup file")?;
            files.insert(manifest_path);
        } else {
            return Err(Error::Intervention(format!(
                "Backup bundles may contain only regular files and directories: {}",
                path.display()
            )));
        }
        if files.len() > MAX_BACKUP_FILES + 2 {
            return Err(Error::Intervention(
                "Backup inventory exceeds the supported file count.".into(),
            ));
        }
    }
    Ok(())
}

fn push_inventory_children(
    stack: &mut Vec<InventoryTraversalEntry>,
    directory: &Path,
    directory_depth: usize,
    budget: &mut TraversalBudget,
) -> Result<()> {
    let (entries, child_depth) = bounded_sorted_entries(directory, directory_depth, budget)?;
    for entry in entries.into_iter().rev() {
        stack.push(InventoryTraversalEntry {
            path: entry.path(),
            depth: child_depth,
        });
    }
    Ok(())
}

fn bounded_sorted_entries(
    directory: &Path,
    directory_depth: usize,
    budget: &mut TraversalBudget,
) -> Result<(Vec<fs::DirEntry>, usize)> {
    let child_depth = directory_depth
        .checked_add(1)
        .ok_or_else(|| Error::Intervention("Backup nesting depth overflowed.".into()))?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        budget.record(child_depth)?;
        entries.push(entry);
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok((entries, child_depth))
}

fn verify_manifest_file(root: &Path, expected: &BackupFile) -> Result<()> {
    let relative = validate_manifest_path(&expected.path, expected.path != DATABASE_FILE)?;
    let actual = describe_file(root, &root.join(relative))?;
    if actual.blake3 != expected.blake3 || actual.bytes != expected.bytes {
        return Err(Error::Intervention(format!(
            "Backup file failed hash or size verification: {}",
            expected.path
        )));
    }
    Ok(())
}

fn verify_database(
    database: &Path,
    manifest: &BackupManifest,
    artifacts: &[BackupFile],
) -> Result<()> {
    let connection = open_read_only(database)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::Intervention(format!(
            "Backup database integrity check failed: {integrity}"
        )));
    }
    let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign_keys.query([])?.next()?.is_some() {
        return Err(Error::Intervention(
            "Backup database contains foreign-key violations.".into(),
        ));
    }
    let schema_version = current_schema_version(&connection)?;
    if schema_version != manifest.schema_version {
        return Err(Error::Intervention(format!(
            "Backup manifest records schema {}, but the database contains schema {schema_version}.",
            manifest.schema_version
        )));
    }
    verify_database_artifacts(&connection, manifest, artifacts)
}

fn normalize_snapshot_database(database: &Path) -> Result<()> {
    let connection = Connection::open(database)?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(Error::Intervention(format!(
            "Backup database could not be normalized from WAL mode: {journal_mode}"
        )));
    }
    drop(connection);
    for sidecar in [
        database.with_extension("sqlite3-wal"),
        database.with_extension("sqlite3-shm"),
    ] {
        if fs::symlink_metadata(&sidecar).is_ok() {
            return Err(Error::Intervention(format!(
                "Backup database retained an unexpected SQLite sidecar: {}",
                sidecar.display()
            )));
        }
    }
    Ok(())
}

fn schema_version_without_mutation(database: &Path) -> Result<i64> {
    let before = database_state(database)?;
    let staging = tempfile::tempdir()?;
    let snapshot = staging.path().join(RESTORED_DATABASE_FILE);
    copy_regular_file(database, &snapshot)?;
    let source_wal = sqlite_sidecar(database, "-wal")?;
    if before.wal.is_some() {
        copy_regular_file(&source_wal, &sqlite_sidecar(&snapshot, "-wal")?)?;
    }
    if database_state(database)? != before {
        return Err(Error::Intervention(
            "Database changed while preparing the migration dry-run; retry when writes are idle."
                .into(),
        ));
    }
    let connection = open_read_only(&snapshot)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::Intervention(format!(
            "Database integrity check failed during migration dry-run: {integrity}"
        )));
    }
    current_schema_version(&connection)
}

fn database_state(database: &Path) -> Result<DatabaseState> {
    Ok(DatabaseState {
        database: file_state(database, "Hardknock database")?.ok_or_else(|| {
            Error::Intervention(format!(
                "Hardknock database disappeared: {}",
                database.display()
            ))
        })?,
        wal: file_state(&sqlite_sidecar(database, "-wal")?, "SQLite WAL")?,
        shared_memory: file_state(
            &sqlite_sidecar(database, "-shm")?,
            "SQLite shared-memory file",
        )?,
    })
}

fn file_state(path: &Path, label: &str) -> Result<Option<FileState>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate_regular_metadata(&metadata, path, label)?;
    Ok(Some(FileState {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    }))
}

fn sqlite_sidecar(database: &Path, suffix: &str) -> Result<PathBuf> {
    let name = database
        .file_name()
        .ok_or_else(|| Error::InvalidInput("SQLite database path has no file name".into()))?;
    let mut sidecar = name.to_os_string();
    sidecar.push(suffix);
    Ok(database.with_file_name(sidecar))
}

fn verify_database_artifacts(
    connection: &Connection,
    manifest: &BackupManifest,
    artifacts: &[BackupFile],
) -> Result<()> {
    let table_exists: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='experience_artifacts')",
        [],
        |row| row.get(0),
    )?;
    if table_exists == 0 {
        return Ok(());
    }
    let by_path: BTreeMap<_, _> = artifacts
        .iter()
        .map(|artifact| (artifact.path.as_str(), artifact))
        .collect();
    let source_home = Path::new(&manifest.source_home);
    let mut statement =
        connection.prepare("SELECT path,blake3,bytes FROM experience_artifacts ORDER BY path")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let hash: String = row.get(1)?;
        let bytes: i64 = row.get(2)?;
        let path = Path::new(&path);
        let relative = path.strip_prefix(source_home).map_err(|_| {
            Error::Intervention(format!(
                "Database references an artifact outside HARDKNOCK_HOME: {}",
                path.display()
            ))
        })?;
        let manifest_path = path_to_manifest(relative)?;
        validate_manifest_path(&manifest_path, true)?;
        let file = by_path.get(manifest_path.as_str()).ok_or_else(|| {
            Error::Intervention(format!(
                "Database references an artifact missing from the backup: {manifest_path}"
            ))
        })?;
        if bytes < 0 || file.bytes != bytes as u64 || file.blake3 != hash {
            return Err(Error::Intervention(format!(
                "Database artifact metadata does not match the backup: {manifest_path}"
            )));
        }
    }
    Ok(())
}

fn verify_restored_stage(stage: &Path, manifest: &BackupManifest) -> Result<()> {
    let metadata = fs::metadata(stage)?;
    require_owner(&metadata, "Restore staging directory")?;
    require_mode(&metadata, 0o700, "Restore staging directory")?;
    let database = stage.join(RESTORED_DATABASE_FILE);
    let actual_database = describe_file(stage, &database)?;
    if actual_database.blake3 != manifest.database.blake3
        || actual_database.bytes != manifest.database.bytes
    {
        return Err(Error::Intervention(
            "Restored database does not match the verified backup.".into(),
        ));
    }
    for artifact in &manifest.artifacts {
        let relative = validate_manifest_path(&artifact.path, true)?;
        let actual = describe_file(stage, &stage.join(relative))?;
        if actual.blake3 != artifact.blake3 || actual.bytes != artifact.bytes {
            return Err(Error::Intervention(format!(
                "Restored artifact does not match the verified backup: {}",
                artifact.path
            )));
        }
    }
    verify_database(&database, manifest, &manifest.artifacts)
}

fn copy_artifact_tree(
    source: &Path,
    destination: &Path,
    backup_root: &Path,
    files: &mut Vec<BackupFile>,
) -> Result<()> {
    copy_artifact_tree_with_limits(
        source,
        destination,
        backup_root,
        files,
        TraversalLimits {
            max_entries: MAX_BACKUP_SOURCE_ENTRIES,
            max_depth: MAX_BACKUP_NESTING_DEPTH,
        },
    )
}

fn copy_artifact_tree_with_limits(
    source: &Path,
    destination: &Path,
    backup_root: &Path,
    files: &mut Vec<BackupFile>,
    limits: TraversalLimits,
) -> Result<()> {
    reject_symlink(source, "Hardknock artifacts directory")?;
    let metadata = fs::metadata(source)?;
    if !metadata.is_dir() {
        return Err(Error::Intervention(
            "Hardknock artifacts path must be a directory.".into(),
        ));
    }
    require_owner(&metadata, "Hardknock artifacts directory")?;
    require_not_group_or_world_writable(&metadata, "Hardknock artifacts directory")?;
    let mut budget = TraversalBudget::new("Backup source", limits);
    let mut stack = Vec::new();
    push_copy_children(&mut stack, source, destination, 1, &mut budget)?;

    while let Some(entry) = stack.pop() {
        let source_path = entry.source;
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Intervention(format!(
                "Hardknock artifacts must not contain symlinks: {}",
                source_path.display()
            )));
        }
        let destination_path = entry.destination;
        if metadata.is_dir() {
            require_owner(&metadata, "Hardknock artifact directory")?;
            require_not_group_or_world_writable(&metadata, "Hardknock artifact directory")?;
            fs::create_dir(&destination_path)?;
            set_directory_mode(&destination_path)?;
            push_copy_children(
                &mut stack,
                &source_path,
                &destination_path,
                entry.depth,
                &mut budget,
            )?;
        } else if metadata.is_file() {
            let copied = copy_regular_file(&source_path, &destination_path)?;
            files.push(BackupFile {
                path: path_to_manifest(destination_path.strip_prefix(backup_root).map_err(
                    |_| Error::Intervention("Artifact copy escaped backup root.".into()),
                )?)?,
                blake3: copied.0,
                bytes: copied.1,
            });
        } else {
            return Err(Error::Intervention(format!(
                "Hardknock artifacts may contain only regular files and directories: {}",
                source_path.display()
            )));
        }
        if files.len() > MAX_BACKUP_FILES {
            return Err(Error::Intervention(format!(
                "Backup contains more than {MAX_BACKUP_FILES} artifact files."
            )));
        }
    }
    Ok(())
}

fn push_copy_children(
    stack: &mut Vec<CopyTraversalEntry>,
    source: &Path,
    destination: &Path,
    source_depth: usize,
    budget: &mut TraversalBudget,
) -> Result<()> {
    let (entries, child_depth) = bounded_sorted_entries(source, source_depth, budget)?;
    for entry in entries.into_iter().rev() {
        stack.push(CopyTraversalEntry {
            source: entry.path(),
            destination: destination.join(entry.file_name()),
            depth: child_depth,
        });
    }
    Ok(())
}

fn copy_verified_file(source: &Path, destination: &Path, expected: &BackupFile) -> Result<()> {
    let (hash, bytes) = copy_regular_file(source, destination)?;
    if hash != expected.blake3 || bytes != expected.bytes {
        let _ = fs::remove_file(destination);
        return Err(Error::Intervention(format!(
            "Backup changed while it was being restored: {}",
            expected.path
        )));
    }
    Ok(())
}

fn copy_regular_file(source: &Path, destination: &Path) -> Result<(String, u64)> {
    let before = secure_source_file_state(source, "Backup source file")?;
    let mut source_file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(source)?;
    let opened = source_file.metadata()?;
    let after_open = fs::symlink_metadata(source)?;
    validate_source_regular_metadata(&opened, source, "Opened backup source file")?;
    validate_source_regular_metadata(&after_open, source, "Backup source file")?;
    if !same_file_identity(&before, &opened) || !same_file_identity(&opened, &after_open) {
        return Err(Error::Intervention(format!(
            "Source file changed identity while being copied: {}",
            source.display()
        )));
    }
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(destination)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = source_file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination_file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| Error::Intervention("Backup file size overflowed.".into()))?;
    }
    destination_file.sync_all()?;
    destination_file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let after_copy = source_file.metadata()?;
    let current = fs::symlink_metadata(source)?;
    validate_source_regular_metadata(&after_copy, source, "Opened backup source file")?;
    validate_source_regular_metadata(&current, source, "Backup source file")?;
    if !same_file_state(&before, &after_copy) || !same_file_state(&after_copy, &current) {
        let _ = fs::remove_file(destination);
        return Err(Error::Intervention(format!(
            "Source file changed while being copied: {}",
            source.display()
        )));
    }
    let destination_metadata = destination_file.metadata()?;
    validate_regular_metadata(&destination_metadata, destination, "Copied backup file")?;
    Ok((hasher.finalize().to_hex().to_string(), bytes))
}

fn describe_file(root: &Path, path: &Path) -> Result<BackupFile> {
    let metadata = secure_file_state(path, "Backup file")?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    let after_open = fs::symlink_metadata(path)?;
    validate_regular_metadata(&opened, path, "Opened backup file")?;
    validate_regular_metadata(&after_open, path, "Backup file")?;
    if !same_file_identity(&metadata, &opened) || !same_file_identity(&opened, &after_open) {
        return Err(Error::Intervention(format!(
            "Backup file changed identity during verification: {}",
            path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| Error::Intervention("Backup file size overflowed.".into()))?;
    }
    let after_read = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    validate_regular_metadata(&after_read, path, "Opened backup file")?;
    validate_regular_metadata(&current, path, "Backup file")?;
    if !same_file_state(&metadata, &after_read) || !same_file_state(&after_read, &current) {
        return Err(Error::Intervention(format!(
            "Backup file changed during verification: {}",
            path.display()
        )));
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| Error::Intervention("Backup file escaped its root.".into()))?;
    Ok(BackupFile {
        path: path_to_manifest(relative)?,
        blake3: hasher.finalize().to_hex().to_string(),
        bytes,
    })
}

fn write_manifest(root: &Path, manifest: &BackupManifest) -> Result<()> {
    let path = root.join(MANIFEST_FILE);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    writeln!(file)?;
    file.sync_all()?;
    Ok(())
}

fn canonical_source_home(home: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(home)?;
    reject_symlink(&absolute, "Hardknock data home")?;
    let metadata = fs::metadata(&absolute)?;
    if !metadata.is_dir() {
        return Err(Error::Intervention(
            "HARDKNOCK_HOME must be a directory.".into(),
        ));
    }
    require_owner(&metadata, "HARDKNOCK_HOME")?;
    require_mode(&metadata, 0o700, "HARDKNOCK_HOME")?;
    let home = absolute.canonicalize()?;
    validate_dedicated_home(&home)?;
    Ok(home)
}

fn resolve_without_mutation(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    if fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::Intervention(
            "Hardknock data home must not be a symlink.".into(),
        ));
    }
    resolve_home(&absolute)
}

fn resolve_new_destination(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| Error::InvalidInput("Destination has no parent directory".into()))?
        .canonicalize()?;
    let name = absolute.file_name().ok_or_else(|| {
        Error::InvalidInput("Destination must name a new backup directory".into())
    })?;
    if name == "." || name == ".." {
        return Err(Error::InvalidInput(
            "Destination must name a new backup directory".into(),
        ));
    }
    Ok(parent.join(name))
}

fn resolve_restore_target(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    if fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::Intervention(
            "Restore target must not be a symlink.".into(),
        ));
    }
    resolve_home(&absolute)
}

fn inspect_restore_target(target: &Path) -> Result<RestoreTarget> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::Intervention(
                    "Restore target must be a new or explicitly empty directory.".into(),
                ));
            }
            if fs::read_dir(target)?.next().is_some() {
                return Err(Error::Intervention(
                    "Restore target must be empty; existing Hardknock data is never overwritten."
                        .into(),
                ));
            }
            require_owner(&metadata, "Restore target")?;
            require_mode(&metadata, 0o700, "Restore target")?;
            Ok(RestoreTarget::Empty {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RestoreTarget::Missing),
        Err(error) => Err(error.into()),
    }
}

fn install_staged_home(stage: &Path, target: &Path, state: RestoreTarget) -> Result<()> {
    match state {
        RestoreTarget::Missing => {
            if fs::symlink_metadata(target).is_ok() {
                return Err(Error::Intervention(
                    "Restore target appeared while restore was being staged.".into(),
                ));
            }
        }
        RestoreTarget::Empty { device, inode } => {
            let metadata = fs::symlink_metadata(target)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.dev() != device
                || metadata.ino() != inode
                || metadata.uid() != geteuid().as_raw()
                || metadata.permissions().mode() & 0o777 != 0o700
                || fs::read_dir(target)?.next().is_some()
            {
                return Err(Error::Intervention(
                    "Restore target changed while restore was being staged.".into(),
                ));
            }
            fs::remove_dir(target)?;
        }
    }
    if let Err(error) = rename_noreplace(stage, target) {
        if matches!(state, RestoreTarget::Empty { .. }) {
            let _ = fs::create_dir(target);
            let _ = fs::set_permissions(target, fs::Permissions::from_mode(0o700));
        }
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(Error::Intervention(
                "Restore target appeared while restore was being staged.".into(),
            ));
        }
        return Err(error.into());
    }
    if let Some(parent) = target.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn ensure_private_parent(root: &Path, file: &Path) -> Result<()> {
    let parent = file
        .parent()
        .ok_or_else(|| Error::InvalidInput("Backup file has no parent directory".into()))?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| Error::Intervention("Restore path escaped staging directory.".into()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Intervention(
                "Restore path contains invalid components.".into(),
            ));
        };
        current.push(component);
        if !current.exists() {
            fs::create_dir(&current)?;
        }
        reject_symlink(&current, "Restore directory")?;
        set_directory_mode(&current)?;
    }
    Ok(())
}

fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    validate_regular_metadata(&metadata, path, label)
}

fn validate_regular_metadata(metadata: &fs::Metadata, path: &Path, label: &str) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Intervention(format!(
            "{label} must be a regular file, not a symlink or special file: {}",
            path.display()
        )));
    }
    require_owner(metadata, label)?;
    require_single_link(metadata, label)?;
    require_mode(metadata, 0o600, label)?;
    Ok(())
}

fn validate_source_regular_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    label: &str,
) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Intervention(format!(
            "{label} must be a regular file, not a symlink or special file: {}",
            path.display()
        )));
    }
    require_owner(metadata, label)?;
    require_single_link(metadata, label)?;
    require_not_group_or_world_writable(metadata, label)
}

fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Intervention(format!(
            "{label} must not be a symlink: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_mode(metadata: &fs::Metadata, expected: u32, label: &str) -> Result<()> {
    let actual = metadata.permissions().mode() & 0o777;
    if actual != expected {
        return Err(Error::Intervention(format!(
            "{label} permissions must be {expected:04o}, found {actual:04o}."
        )));
    }
    Ok(())
}

fn require_not_group_or_world_writable(metadata: &fs::Metadata, label: &str) -> Result<()> {
    let actual = metadata.permissions().mode() & 0o777;
    if actual & 0o022 != 0 {
        return Err(Error::Intervention(format!(
            "{label} permissions must not allow group or world writes, found {actual:04o}."
        )));
    }
    Ok(())
}

fn require_owner(metadata: &fs::Metadata, label: &str) -> Result<()> {
    let expected = geteuid().as_raw();
    if metadata.uid() != expected {
        return Err(Error::Intervention(format!(
            "{label} must be owned by user {expected}, found owner {}.",
            metadata.uid()
        )));
    }
    Ok(())
}

fn require_single_link(metadata: &fs::Metadata, label: &str) -> Result<()> {
    if metadata.nlink() != 1 {
        return Err(Error::Intervention(format!(
            "{label} must have exactly one hard link, found {}.",
            metadata.nlink()
        )));
    }
    Ok(())
}

fn secure_file_state(path: &Path, label: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    validate_regular_metadata(&metadata, path, label)?;
    Ok(metadata)
}

fn secure_source_file_state(path: &Path, label: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    validate_source_regular_metadata(&metadata, path, label)?;
    Ok(metadata)
}

fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && right.uid() == geteuid().as_raw()
        && right.nlink() == 1
}

fn same_file_state(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    same_file_identity(left, right)
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

pub(crate) fn acquire_home_maintenance_lock(home: &Path) -> Result<File> {
    acquire_home_lock(home, MAINTENANCE_LOCK_FILE, "Hardknock home maintenance")
}

pub(crate) fn acquire_artifact_capacity_lock(home: &Path) -> Result<File> {
    acquire_home_lock(
        home,
        ARTIFACT_CAPACITY_LOCK_FILE,
        "Hardknock artifact capacity",
    )
}

pub(crate) fn active_artifact_reservations_locked(
    home: &Path,
) -> Result<ActiveArtifactReservations> {
    let (directory_path, directory) = artifact_reservation_directory(home)?;
    let mut names = fs::read_dir(&directory_path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();

    let mut result = ActiveArtifactReservations::default();
    for name in names {
        let display_path = directory_path.join(&name);
        let valid_name = name
            .to_str()
            .is_some_and(|name| name.starts_with("reservation-") && name.ends_with(".json"));
        if !valid_name {
            return Err(Error::Intervention(format!(
                "Artifact reservation directory contains an unmanaged entry: {}",
                display_path.display()
            )));
        }
        let named = rustix::fs::statat(&directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        validate_artifact_reservation_stat(&named, &display_path)?;
        let mut file = File::from(
            rustix::fs::openat(
                &directory,
                &name,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let opened = rustix::fs::fstat(&file).map_err(std::io::Error::from)?;
        validate_artifact_reservation_stat(&opened, &display_path)?;
        if !same_artifact_reservation_identity(&named, &opened) {
            return Err(Error::Intervention(format!(
                "Artifact reservation changed while it was being opened: {}",
                display_path.display()
            )));
        }

        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                let current =
                    rustix::fs::statat(&directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(std::io::Error::from)?;
                if !same_artifact_reservation_identity(&opened, &current) {
                    return Err(Error::Intervention(format!(
                        "Stale artifact reservation changed before cleanup: {}",
                        display_path.display()
                    )));
                }
                rustix::fs::unlinkat(&directory, &name, rustix::fs::AtFlags::empty())
                    .map_err(std::io::Error::from)?;
                result.removed_stale = result.removed_stale.saturating_add(1);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut bytes = Vec::new();
                (&mut file)
                    .take(MAX_ARTIFACT_RESERVATION_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_ARTIFACT_RESERVATION_BYTES {
                    return Err(Error::Intervention(format!(
                        "Artifact reservation exceeds {} bytes: {}",
                        MAX_ARTIFACT_RESERVATION_BYTES,
                        display_path.display()
                    )));
                }
                let record: ArtifactReservationRecord = serde_json::from_slice(&bytes)?;
                if record.format != ARTIFACT_RESERVATION_FORMAT {
                    return Err(Error::Intervention(format!(
                        "Artifact reservation has an unsupported format: {}",
                        display_path.display()
                    )));
                }
                let after = rustix::fs::fstat(&file).map_err(std::io::Error::from)?;
                let current =
                    rustix::fs::statat(&directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(std::io::Error::from)?;
                if !same_artifact_reservation_state(&opened, &after)
                    || !same_artifact_reservation_state(&after, &current)
                {
                    return Err(Error::Intervention(format!(
                        "Artifact reservation changed while it was being read: {}",
                        display_path.display()
                    )));
                }
                result.count = result.count.checked_add(1).ok_or_else(|| {
                    Error::Intervention("Artifact reservation count overflowed.".into())
                })?;
                result.usage = result.usage.checked_add(record.usage).map_err(|error| {
                    Error::Intervention(format!("Artifact reservation usage is invalid: {error}"))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    directory.sync_all()?;
    Ok(result)
}

pub(crate) fn create_artifact_reservation_locked(
    home: &Path,
    usage: crate::storage_policy::StorageUsage,
) -> Result<(File, OsString)> {
    let (directory_path, directory) = artifact_reservation_directory(home)?;
    let record = serde_json::to_vec(&ArtifactReservationRecord {
        format: ARTIFACT_RESERVATION_FORMAT.into(),
        usage,
    })?;
    if record.len() as u64 > MAX_ARTIFACT_RESERVATION_BYTES {
        return Err(Error::Intervention(
            "Artifact reservation record exceeded its internal bound.".into(),
        ));
    }
    for _ in 0..8 {
        let name = OsString::from(format!(
            "reservation-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let descriptor = match rustix::fs::openat(
            &directory,
            &name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        let mut file = File::from(descriptor);
        FileExt::try_lock_exclusive(&file)?;
        file.write_all(&record)?;
        file.sync_all()?;
        let opened = rustix::fs::fstat(&file).map_err(std::io::Error::from)?;
        let named = rustix::fs::statat(&directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        validate_artifact_reservation_stat(&opened, &directory_path.join(&name))?;
        if !same_artifact_reservation_identity(&opened, &named) {
            return Err(Error::Intervention(
                "Artifact reservation changed while it was being created.".into(),
            ));
        }
        directory.sync_all()?;
        return Ok((file, name));
    }
    Err(Error::Intervention(
        "Could not allocate a unique artifact reservation record.".into(),
    ))
}

pub(crate) fn release_artifact_reservation(
    home: &Path,
    name: &OsString,
    lease: &File,
) -> Result<()> {
    let _capacity = acquire_artifact_capacity_lock(home)?;
    let (directory_path, directory) = artifact_reservation_directory(home)?;
    let opened = rustix::fs::fstat(lease).map_err(std::io::Error::from)?;
    let named = match rustix::fs::statat(&directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(named) => named,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    validate_artifact_reservation_stat(&opened, &directory_path.join(name))?;
    if !same_artifact_reservation_identity(&opened, &named) {
        return Err(Error::Intervention(
            "Artifact reservation changed before release.".into(),
        ));
    }
    rustix::fs::unlinkat(&directory, name, rustix::fs::AtFlags::empty())
        .map_err(std::io::Error::from)?;
    directory.sync_all()?;
    Ok(())
}

pub(crate) fn require_no_active_artifact_reservations_locked(
    home: &Path,
    operation: &str,
) -> Result<ActiveArtifactReservations> {
    let reservations = active_artifact_reservations_locked(home)?;
    if reservations.count > 0 {
        return Err(Error::Intervention(format!(
            "{operation} requires artifact writers to be idle; {} active operation(s) reserve {} bytes and {} files.",
            reservations.count, reservations.usage.bytes, reservations.usage.files
        )));
    }
    Ok(reservations)
}

fn artifact_reservation_directory(home: &Path) -> Result<(PathBuf, File)> {
    let path = home.join("locks").join(ARTIFACT_RESERVATIONS_DIRECTORY);
    match fs::create_dir(&path) {
        Ok(()) => set_directory_mode(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let before = fs::symlink_metadata(&path)?;
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(Error::Intervention(format!(
            "Artifact reservation path must be a directory: {}",
            path.display()
        )));
    }
    require_owner(&before, "Artifact reservation directory")?;
    require_mode(&before, 0o700, "Artifact reservation directory")?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let opened = directory.metadata()?;
    let current = fs::symlink_metadata(&path)?;
    for metadata in [&opened, &current] {
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != geteuid().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(Error::Intervention(
                "Artifact reservation directory is not an owned private directory.".into(),
            ));
        }
    }
    if before.dev() != opened.dev()
        || before.ino() != opened.ino()
        || opened.dev() != current.dev()
        || opened.ino() != current.ino()
    {
        return Err(Error::Intervention(
            "Artifact reservation directory changed while it was being opened.".into(),
        ));
    }
    Ok((path, directory))
}

fn validate_artifact_reservation_stat(stat: &rustix::fs::Stat, path: &Path) -> Result<()> {
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || stat.st_uid != geteuid().as_raw()
        || stat.st_nlink != 1
        || u32::from(stat.st_mode) & 0o777 != 0o600
    {
        return Err(Error::Intervention(format!(
            "Artifact reservation must be an owned private regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn same_artifact_reservation_identity(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn same_artifact_reservation_state(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    same_artifact_reservation_identity(left, right)
        && left.st_mode == right.st_mode
        && left.st_uid == right.st_uid
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn acquire_home_lock(home: &Path, name: &str, label: &str) -> Result<File> {
    let directory = home.join("locks");
    match fs::create_dir(&directory) {
        Ok(()) => set_directory_mode(&directory)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::Intervention(format!(
            "Hardknock lock path must be a directory, not a symlink or special file: {}",
            directory.display()
        )));
    }
    require_owner(&metadata, "Hardknock lock directory")?;
    require_mode(&metadata, 0o700, "Hardknock lock directory")?;

    let path = directory.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(&path)?;
    validate_regular_metadata(&opened, &path, label)?;
    validate_regular_metadata(&current, &path, label)?;
    if !same_file_identity(&opened, &current) {
        return Err(Error::Intervention(format!(
            "{label} lock changed identity while being opened."
        )));
    }
    acquire_bounded_lock(file, label)
}

fn acquire_directory_maintenance_lock(path: &Path, label: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(Error::Intervention(format!(
            "{label} must be a directory: {}",
            path.display()
        )));
    }
    acquire_bounded_lock(file, label)
}

fn acquire_bounded_lock(file: File, label: &str) -> Result<File> {
    let started = Instant::now();
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < MAINTENANCE_LOCK_TIMEOUT =>
            {
                thread::sleep(MAINTENANCE_LOCK_RETRY);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(Error::Intervention(format!(
                    "{label} is busy; another conflicting operation is in progress."
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn set_directory_mode(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn set_file_mode(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let before = secure_file_state(path, "SQLite database")?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let after = secure_file_state(path, "SQLite database")?;
    if !same_file_identity(&before, &after) {
        return Err(Error::Intervention(format!(
            "SQLite database changed identity while being opened: {}",
            path.display()
        )));
    }
    Ok(connection)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn path_to_manifest(path: &Path) -> Result<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Intervention(
                "Backup path contains traversal or an absolute component.".into(),
            ));
        };
        components.push(
            component
                .to_str()
                .ok_or_else(|| Error::Intervention("Backup paths must be valid UTF-8.".into()))?,
        );
    }
    if components.len() > MAX_BACKUP_NESTING_DEPTH {
        return Err(Error::Intervention(format!(
            "Backup path nesting exceeds the supported depth of {MAX_BACKUP_NESTING_DEPTH}."
        )));
    }
    let value = components.join("/");
    if value.is_empty() || value.len() > MAX_MANIFEST_PATH_BYTES {
        return Err(Error::Intervention(
            "Backup path is empty or too long.".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_private_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn no_replace_rename_preserves_both_existing_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("source-sentinel"), "source").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("destination-sentinel"), "destination").unwrap();

        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(source.join("source-sentinel")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_to_string(destination.join("destination-sentinel")).unwrap(),
            "destination"
        );
    }

    #[test]
    fn source_traversal_entry_limit_counts_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("artifacts");
        let backup_root = temp.path().join("backup");
        let destination = backup_root.join("artifacts");
        create_private_directory(&source);
        create_private_directory(&backup_root);
        create_private_directory(&destination);
        for name in ["a", "b", "c"] {
            create_private_directory(&source.join(name));
        }

        let error = copy_artifact_tree_with_limits(
            &source,
            &destination,
            &backup_root,
            &mut Vec::new(),
            TraversalLimits {
                max_entries: 2,
                max_depth: 8,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("total entry count"), "{error}");
    }

    #[test]
    fn bundle_inventory_entry_limit_counts_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["a", "b", "c"] {
            create_private_directory(&temp.path().join(name));
        }

        let error = collect_inventory_with_limits(
            temp.path(),
            temp.path(),
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            TraversalLimits {
                max_entries: 2,
                max_depth: 8,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("total entry count"), "{error}");
    }
}
