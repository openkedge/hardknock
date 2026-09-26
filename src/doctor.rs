// SPDX-License-Identifier: Apache-2.0

//! Synchronous production-readiness checks for the strict doctor command.
//!
//! This module is intentionally independent of the CLI and store. Callers
//! supply observations that require application context, while filesystem and
//! release checks are performed directly and fail closed.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

pub const STRICT_READY_EXIT_CODE: u8 = 0;
pub const STRICT_WARNING_EXIT_CODE: u8 = 1;
pub const STRICT_FAILURE_EXIT_CODE: u8 = 2;
pub const RELEASE_MANIFEST_FORMAT: &str = "hardknock-install-manifest-v1";

const MAX_RELEASE_MANIFEST_BYTES: u64 = 1024 * 1024;
const RELEASE_MANIFEST_RELATIVE_PATH: &str = "share/hardknock/install-manifest-v1";
const MANAGED_RELEASE_FILES: [(&str, bool); 4] = [
    ("bin/hardknock", true),
    ("bin/hk-effect", true),
    ("share/doc/hardknock/LICENSE", false),
    ("share/doc/hardknock/NOTICE", false),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Informational,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Warning,
    Failed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Ready,
    Degraded,
    NotReady,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub severity: Severity,
    pub required: bool,
    pub summary: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl Check {
    fn passed(id: impl Into<String>, summary: impl Into<String>, required: bool) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Passed,
            severity: Severity::Informational,
            required,
            summary: summary.into(),
            details: BTreeMap::new(),
        }
    }

    fn warning(id: impl Into<String>, summary: impl Into<String>, required: bool) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Warning,
            severity: Severity::Warning,
            required,
            summary: summary.into(),
            details: BTreeMap::new(),
        }
    }

    fn failed(id: impl Into<String>, summary: impl Into<String>, required: bool) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Failed,
            severity: Severity::Error,
            required,
            summary: summary.into(),
            details: BTreeMap::new(),
        }
    }

    fn unavailable(id: impl Into<String>, summary: impl Into<String>, required: bool) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Unavailable,
            severity: if required {
                Severity::Error
            } else {
                Severity::Warning
            },
            required,
            summary: summary.into(),
            details: BTreeMap::new(),
        }
    }

    fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    pub fn blocks_strict_readiness(&self) -> bool {
        matches!(self.status, CheckStatus::Warning | CheckStatus::Failed)
            || (self.required && self.status == CheckStatus::Unavailable)
    }

    pub fn strict_exit_code(&self) -> u8 {
        match self.status {
            CheckStatus::Failed => STRICT_FAILURE_EXIT_CODE,
            CheckStatus::Unavailable if self.required => STRICT_FAILURE_EXIT_CODE,
            CheckStatus::Warning => STRICT_WARNING_EXIT_CODE,
            CheckStatus::Passed | CheckStatus::Unavailable => STRICT_READY_EXIT_CODE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub status: ReportStatus,
    pub strict_ready: bool,
    pub exit_code: u8,
    pub highest_severity: Severity,
    pub passed: usize,
    pub warnings: usize,
    pub failures: usize,
    pub unavailable: usize,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn from_checks(checks: Vec<Check>) -> Self {
        let exit_code = checks
            .iter()
            .map(Check::strict_exit_code)
            .max()
            .unwrap_or(STRICT_READY_EXIT_CODE);
        let status = match exit_code {
            STRICT_READY_EXIT_CODE => ReportStatus::Ready,
            STRICT_WARNING_EXIT_CODE => ReportStatus::Degraded,
            _ => ReportStatus::NotReady,
        };
        let highest_severity = checks
            .iter()
            .map(|check| check.severity)
            .max()
            .unwrap_or_default();
        let passed = checks
            .iter()
            .filter(|check| check.status == CheckStatus::Passed)
            .count();
        let warnings = checks
            .iter()
            .filter(|check| check.status == CheckStatus::Warning)
            .count();
        let failures = checks
            .iter()
            .filter(|check| check.status == CheckStatus::Failed)
            .count();
        let unavailable = checks
            .iter()
            .filter(|check| check.status == CheckStatus::Unavailable)
            .count();

        Self {
            status,
            strict_ready: checks.iter().all(|check| !check.blocks_strict_readiness()),
            exit_code,
            highest_severity,
            passed,
            warnings,
            failures,
            unavailable,
            checks,
        }
    }

    pub fn strict_exit_code(&self) -> u8 {
        self.exit_code
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivatePathRole {
    HomeDirectory,
    DatabaseFile,
    ConfigurationFile,
    RuntimeDirectory,
    RuntimeFile,
}

impl PrivatePathRole {
    fn label(self) -> &'static str {
        match self {
            Self::HomeDirectory => "home",
            Self::DatabaseFile => "database",
            Self::ConfigurationFile => "configuration",
            Self::RuntimeDirectory => "runtime_directory",
            Self::RuntimeFile => "runtime_file",
        }
    }

    fn expected_mode(self) -> u32 {
        match self {
            Self::HomeDirectory | Self::RuntimeDirectory => 0o700,
            Self::DatabaseFile | Self::ConfigurationFile | Self::RuntimeFile => 0o600,
        }
    }

    fn expects_directory(self) -> bool {
        matches!(self, Self::HomeDirectory | Self::RuntimeDirectory)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivatePathInput {
    pub name: String,
    pub path: PathBuf,
    pub role: PrivatePathRole,
    pub required: bool,
}

impl PrivatePathInput {
    pub fn home(path: impl Into<PathBuf>) -> Self {
        Self {
            name: "home".into(),
            path: path.into(),
            role: PrivatePathRole::HomeDirectory,
            required: true,
        }
    }

    pub fn database(path: impl Into<PathBuf>) -> Self {
        Self {
            name: "database".into(),
            path: path.into(),
            role: PrivatePathRole::DatabaseFile,
            required: true,
        }
    }

    pub fn configuration(path: impl Into<PathBuf>, required: bool) -> Self {
        Self {
            name: "configuration".into(),
            path: path.into(),
            role: PrivatePathRole::ConfigurationFile,
            required,
        }
    }

    pub fn runtime_file(name: impl Into<String>, path: impl Into<PathBuf>, required: bool) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            role: PrivatePathRole::RuntimeFile,
            required,
        }
    }

    pub fn runtime_directory(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        required: bool,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            role: PrivatePathRole::RuntimeDirectory,
            required,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskThresholds {
    pub minimum_available_bytes: u64,
    pub warning_available_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskInput {
    pub path: PathBuf,
    pub thresholds: DiskThresholds,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BackupVerification {
    Verified,
    Failed { reason: String },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInput {
    pub age_seconds: Option<u64>,
    pub maximum_age_seconds: u64,
    pub bundle_verification: BackupVerification,
    pub staged_restore: BackupVerification,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "installation", rename_all = "snake_case")]
pub enum ReleaseIntegrityInput {
    Managed { manifest_path: PathBuf },
    SourceBuild { executable_path: Option<PathBuf> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub expected_schema: i64,
    pub actual_schema: Option<i64>,
    #[serde(default)]
    pub private_paths: Vec<PrivatePathInput>,
    pub disk: Option<DiskInput>,
    pub stale_runtime_paths: Option<Vec<String>>,
    pub backup: Option<BackupInput>,
    pub release: ReleaseIntegrityInput,
}

pub fn run(input: &Input) -> Report {
    let mut checks = Vec::new();
    checks.push(check_schema(input.expected_schema, input.actual_schema));

    append_private_path_checks(&mut checks, &input.private_paths);
    checks.push(check_available_disk(input.disk.as_ref()));
    checks.push(check_stale_runtime_paths(
        input.stale_runtime_paths.as_deref(),
    ));
    checks.extend(check_backup(input.backup.as_ref()));
    checks.push(check_release_integrity(&input.release));

    Report::from_checks(checks)
}

pub fn check_schema(expected: i64, actual: Option<i64>) -> Check {
    let id = "storage.schema";
    if expected < 0 {
        return Check::failed(id, "Expected schema version is invalid", true)
            .with_detail("expected", expected.to_string());
    }

    match actual {
        None => Check::unavailable(id, "Applied schema version was not supplied", true)
            .with_detail("expected", expected.to_string()),
        Some(actual) if actual == expected => {
            Check::passed(id, "Database schema matches the release", true)
                .with_detail("expected", expected.to_string())
                .with_detail("actual", actual.to_string())
        }
        Some(actual) => Check::failed(id, "Database schema does not match the release", true)
            .with_detail("expected", expected.to_string())
            .with_detail("actual", actual.to_string()),
    }
}

fn append_private_path_checks(checks: &mut Vec<Check>, inputs: &[PrivatePathInput]) {
    let has_home = inputs
        .iter()
        .any(|input| input.role == PrivatePathRole::HomeDirectory);
    let has_database = inputs
        .iter()
        .any(|input| input.role == PrivatePathRole::DatabaseFile);
    let has_configuration = inputs
        .iter()
        .any(|input| input.role == PrivatePathRole::ConfigurationFile);
    let has_runtime = inputs.iter().any(|input| {
        matches!(
            input.role,
            PrivatePathRole::RuntimeDirectory | PrivatePathRole::RuntimeFile
        )
    });

    if !has_home {
        checks.push(Check::unavailable(
            "filesystem.home",
            "Home-directory path was not supplied",
            true,
        ));
    }
    if !has_database {
        checks.push(Check::unavailable(
            "filesystem.database",
            "Database path was not supplied",
            true,
        ));
    }
    if !has_configuration {
        checks.push(Check::unavailable(
            "filesystem.configuration",
            "Configuration path was not supplied",
            false,
        ));
    }
    if !has_runtime {
        checks.push(Check::unavailable(
            "filesystem.runtime",
            "Runtime paths were not supplied",
            false,
        ));
    }

    checks.extend(inputs.iter().map(check_private_path));
}

pub fn check_private_path(input: &PrivatePathInput) -> Check {
    check_private_path_for_uid(input, nix::unistd::geteuid().as_raw())
}

fn check_private_path_for_uid(input: &PrivatePathInput, expected_uid: u32) -> Check {
    let id = format!("filesystem.{}", input.name);
    let expected_mode = input.role.expected_mode();
    let metadata = match fs::symlink_metadata(&input.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Check::unavailable(
                id,
                format!("{} path does not exist", input.role.label()),
                input.required,
            )
            .with_detail("path", input.path.display().to_string());
        }
        Err(error) => {
            return Check::failed(
                id,
                format!("Cannot inspect {} path", input.role.label()),
                input.required,
            )
            .with_detail("path", input.path.display().to_string())
            .with_detail("error", error.to_string());
        }
    };

    if metadata.file_type().is_symlink() {
        return Check::failed(
            id,
            format!("{} path is a symlink", input.role.label()),
            input.required,
        )
        .with_detail("path", input.path.display().to_string());
    }

    if input.role.expects_directory() && !metadata.is_dir() {
        return Check::failed(
            id,
            format!("{} path is not a directory", input.role.label()),
            input.required,
        )
        .with_detail("path", input.path.display().to_string());
    }
    if !input.role.expects_directory() && !metadata.is_file() {
        return Check::failed(
            id,
            format!("{} path is not a regular file", input.role.label()),
            input.required,
        )
        .with_detail("path", input.path.display().to_string());
    }
    if !input.role.expects_directory() && metadata.nlink() != 1 {
        return Check::failed(
            id,
            format!("{} file has multiple hard links", input.role.label()),
            input.required,
        )
        .with_detail("path", input.path.display().to_string())
        .with_detail("links", metadata.nlink().to_string());
    }

    let opened = match securely_open_metadata(&input.path, input.role.expects_directory()) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Check::failed(
                id,
                format!(
                    "{} path changed or could not be opened safely",
                    input.role.label()
                ),
                input.required,
            )
            .with_detail("path", input.path.display().to_string())
            .with_detail("error", error);
        }
    };

    if opened.uid() != expected_uid {
        return Check::failed(
            id,
            format!(
                "{} path is not owned by the expected user",
                input.role.label()
            ),
            input.required,
        )
        .with_detail("path", input.path.display().to_string())
        .with_detail("expected_uid", expected_uid.to_string())
        .with_detail("actual_uid", opened.uid().to_string());
    }

    let actual_mode = opened.mode() & 0o7777;
    if actual_mode != expected_mode {
        return Check::failed(
            id,
            format!("{} path permissions are not private", input.role.label()),
            input.required,
        )
        .with_detail("path", input.path.display().to_string())
        .with_detail("expected_mode", format!("{expected_mode:04o}"))
        .with_detail("actual_mode", format!("{actual_mode:04o}"));
    }

    Check::passed(
        id,
        format!(
            "{} ownership and permissions are private",
            input.role.label()
        ),
        input.required,
    )
    .with_detail("path", input.path.display().to_string())
    .with_detail("uid", opened.uid().to_string())
    .with_detail("mode", format!("{actual_mode:04o}"))
}

fn securely_open_metadata(path: &Path, directory: bool) -> Result<fs::Metadata, String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if before.file_type().is_symlink() {
        return Err("path is a symlink".into());
    }

    let flags = nix::libc::O_NOFOLLOW | if directory { nix::libc::O_DIRECTORY } else { 0 };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err("path identity changed while it was inspected".into());
    }
    Ok(opened)
}

pub fn check_available_disk(input: Option<&DiskInput>) -> Check {
    let Some(input) = input else {
        return Check::unavailable(
            "storage.available_disk",
            "Disk thresholds and inspection path were not supplied",
            true,
        );
    };

    if input.thresholds.warning_available_bytes < input.thresholds.minimum_available_bytes {
        return Check::failed(
            "storage.available_disk",
            "Disk warning threshold is below the minimum threshold",
            true,
        )
        .with_detail(
            "minimum_available_bytes",
            input.thresholds.minimum_available_bytes.to_string(),
        )
        .with_detail(
            "warning_available_bytes",
            input.thresholds.warning_available_bytes.to_string(),
        );
    }

    let metadata = match fs::symlink_metadata(&input.path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Check::failed(
                "storage.available_disk",
                "Cannot inspect the filesystem used for storage",
                true,
            )
            .with_detail("path", input.path.display().to_string())
            .with_detail("error", error.to_string());
        }
    };
    if metadata.file_type().is_symlink() {
        return Check::failed(
            "storage.available_disk",
            "Disk inspection path is a symlink",
            true,
        )
        .with_detail("path", input.path.display().to_string());
    }

    match fs2::statvfs(&input.path) {
        Ok(stats) => evaluate_disk(
            &input.path,
            input.thresholds,
            stats.available_space(),
            stats.total_space(),
        ),
        Err(error) => Check::failed(
            "storage.available_disk",
            "Cannot read available disk capacity",
            true,
        )
        .with_detail("path", input.path.display().to_string())
        .with_detail("error", error.to_string()),
    }
}

fn evaluate_disk(
    path: &Path,
    thresholds: DiskThresholds,
    available_bytes: u64,
    total_bytes: u64,
) -> Check {
    let base = if available_bytes < thresholds.minimum_available_bytes {
        Check::failed(
            "storage.available_disk",
            "Available disk capacity is below the required minimum",
            true,
        )
    } else if available_bytes < thresholds.warning_available_bytes {
        Check::warning(
            "storage.available_disk",
            "Available disk capacity is below the warning threshold",
            true,
        )
    } else {
        Check::passed(
            "storage.available_disk",
            "Available disk capacity meets configured thresholds",
            true,
        )
    };

    base.with_detail("path", path.display().to_string())
        .with_detail("available_bytes", available_bytes.to_string())
        .with_detail("total_bytes", total_bytes.to_string())
        .with_detail(
            "minimum_available_bytes",
            thresholds.minimum_available_bytes.to_string(),
        )
        .with_detail(
            "warning_available_bytes",
            thresholds.warning_available_bytes.to_string(),
        )
}

pub fn check_stale_runtime_paths(descriptions: Option<&[String]>) -> Check {
    let Some(descriptions) = descriptions else {
        return Check::unavailable(
            "runtime.stale_paths",
            "Stale runtime path discovery was not supplied",
            false,
        );
    };
    if descriptions.is_empty() {
        return Check::passed(
            "runtime.stale_paths",
            "No stale runtime paths were reported",
            true,
        )
        .with_detail("count", "0");
    }

    let mut check = Check::failed(
        "runtime.stale_paths",
        "Stale runtime paths require cleanup",
        true,
    )
    .with_detail("count", descriptions.len().to_string());
    for (index, description) in descriptions.iter().enumerate() {
        check = check.with_detail(format!("stale_path_{index}"), description);
    }
    check
}

pub fn check_backup(input: Option<&BackupInput>) -> Vec<Check> {
    let Some(input) = input else {
        return vec![
            Check::unavailable(
                "backup.recency",
                "Strict readiness requires a recent backup, but no recency observation was supplied",
                true,
            ),
            Check::unavailable(
                "backup.bundle_verification",
                "Strict readiness requires cryptographic backup bundle verification, but no observation was supplied",
                true,
            ),
            Check::unavailable(
                "backup.staged_restore",
                "Strict readiness requires an isolated staged restore drill, but no observation was supplied",
                true,
            ),
        ];
    };

    let recency = if input.maximum_age_seconds == 0 {
        Check::failed(
            "backup.recency",
            "Maximum backup age must be greater than zero",
            input.required,
        )
    } else if let Some(age_seconds) = input.age_seconds {
        let base = if age_seconds <= input.maximum_age_seconds {
            Check::passed(
                "backup.recency",
                "Latest backup is within the configured age",
                input.required,
            )
        } else {
            Check::failed(
                "backup.recency",
                "Latest backup is older than the configured maximum",
                input.required,
            )
        };
        base.with_detail("age_seconds", age_seconds.to_string())
            .with_detail("maximum_age_seconds", input.maximum_age_seconds.to_string())
    } else {
        Check::unavailable(
            "backup.recency",
            "Latest backup age was not supplied",
            input.required,
        )
        .with_detail("maximum_age_seconds", input.maximum_age_seconds.to_string())
    };

    let bundle_verification = match &input.bundle_verification {
        BackupVerification::Verified => Check::passed(
            "backup.bundle_verification",
            "Latest backup passed cryptographic bundle and database integrity verification",
            input.required,
        ),
        BackupVerification::Failed { reason } => Check::failed(
            "backup.bundle_verification",
            "Latest backup failed cryptographic bundle verification",
            input.required,
        )
        .with_detail("reason", reason),
        BackupVerification::Unavailable { reason } => Check::unavailable(
            "backup.bundle_verification",
            "Cryptographic backup bundle verification is unavailable",
            input.required,
        )
        .with_detail("reason", reason),
    };

    let staged_restore = match &input.staged_restore {
        BackupVerification::Verified => Check::passed(
            "backup.staged_restore",
            "Latest backup completed an isolated staged restore drill",
            input.required,
        ),
        BackupVerification::Failed { reason } => Check::failed(
            "backup.staged_restore",
            "Latest backup failed an isolated staged restore drill",
            input.required,
        )
        .with_detail("reason", reason),
        BackupVerification::Unavailable { reason } => Check::unavailable(
            "backup.staged_restore",
            "An isolated staged restore drill is unavailable",
            input.required,
        )
        .with_detail("reason", reason),
    };

    vec![recency, bundle_verification, staged_restore]
}

pub fn check_release_integrity(input: &ReleaseIntegrityInput) -> Check {
    let manifest_path = match input {
        ReleaseIntegrityInput::Managed { manifest_path } => manifest_path,
        ReleaseIntegrityInput::SourceBuild { executable_path } => {
            let mut check = Check::warning(
                "release.managed_integrity",
                "Source/development execution has no managed release manifest; strict readiness is degraded while non-strict diagnostics remain available",
                false,
            )
            .with_detail("installation", "source_build");
            if let Some(executable_path) = executable_path {
                check = check.with_detail("executable", executable_path.display().to_string());
            }
            return check;
        }
    };

    match fs::symlink_metadata(manifest_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Check::unavailable(
                "release.managed_integrity",
                "Managed installation requires a trustworthy release manifest, but it is missing",
                true,
            )
            .with_detail("installation", "managed")
            .with_detail("manifest", manifest_path.display().to_string());
        }
        Err(error) => {
            return Check::failed(
                "release.managed_integrity",
                "Managed installation manifest cannot be inspected",
                true,
            )
            .with_detail("installation", "managed")
            .with_detail("manifest", manifest_path.display().to_string())
            .with_detail("error", error.to_string());
        }
        Ok(_) => {}
    }

    match verify_release_manifest(manifest_path) {
        Ok(count) => Check::passed(
            "release.managed_integrity",
            "Managed release manifest, prefix, directories, and files are trusted and file hashes match",
            true,
        )
        .with_detail("installation", "managed")
        .with_detail("manifest", manifest_path.display().to_string())
        .with_detail("trusted_owners", "effective user or root")
        .with_detail("verified_files", count.to_string()),
        Err(error) => Check::failed(
            "release.managed_integrity",
            "Managed release integrity verification failed",
            true,
        )
        .with_detail("installation", "managed")
        .with_detail("manifest", manifest_path.display().to_string())
        .with_detail("error", error),
    }
}

fn verify_release_manifest(manifest_path: &Path) -> Result<usize, String> {
    let prefix = installation_prefix(manifest_path)?;
    let expected_uid = nix::unistd::geteuid().as_raw();
    let directories = verify_release_directories(&prefix, expected_uid)?;
    let bytes = read_stable_trusted_regular_file(
        manifest_path,
        Some(MAX_RELEASE_MANIFEST_BYTES),
        expected_uid,
        false,
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("managed manifest is not valid UTF-8: {error}"))?;
    let hashes = parse_release_manifest(text)?;

    for (relative, executable) in MANAGED_RELEASE_FILES {
        let expected = &hashes[relative];
        let path = resolve_managed_release_path(&prefix, Path::new(relative))?;
        let actual = hash_stable_managed_file(&path, executable, expected_uid)
            .map_err(|error| format!("managed file '{relative}': {error}"))?;
        if actual != *expected {
            return Err(format!(
                "managed file '{relative}' SHA-256 mismatch: expected {expected}, actual {actual}"
            ));
        }
    }
    revalidate_release_directories(&directories, expected_uid)?;

    Ok(MANAGED_RELEASE_FILES.len())
}

fn installation_prefix(manifest_path: &Path) -> Result<PathBuf, String> {
    if !manifest_path.is_absolute()
        || manifest_path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("managed manifest path must be a normalized absolute path".into());
    }
    if !manifest_path.ends_with(RELEASE_MANIFEST_RELATIVE_PATH) {
        return Err(format!(
            "managed manifest must be located at {RELEASE_MANIFEST_RELATIVE_PATH}"
        ));
    }

    let mut prefix = manifest_path.to_path_buf();
    for _ in 0..3 {
        prefix = prefix
            .parent()
            .ok_or_else(|| "managed manifest path has no installation prefix".to_string())?
            .to_path_buf();
    }
    if prefix.as_os_str().is_empty() {
        prefix.push(".");
    }
    if prefix == Path::new("/") {
        return Err("managed installation prefix must not be the filesystem root".into());
    }
    Ok(prefix)
}

#[derive(Clone, Debug)]
struct TrustedReleasePath {
    path: PathBuf,
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    symlink: bool,
}

fn verify_release_directories(
    prefix: &Path,
    expected_uid: u32,
) -> Result<Vec<TrustedReleasePath>, String> {
    let mut paths = BTreeMap::<PathBuf, bool>::new();
    let mut current = PathBuf::new();
    for component in prefix.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::Normal(component) => {
                current.push(component);
                paths.insert(current.clone(), current != prefix);
            }
            _ => return Err("managed installation prefix must be normalized".into()),
        }
    }

    for relative in std::iter::once(RELEASE_MANIFEST_RELATIVE_PATH)
        .chain(MANAGED_RELEASE_FILES.iter().map(|(relative, _)| *relative))
    {
        let parent = Path::new(relative)
            .parent()
            .ok_or_else(|| format!("managed release path has no parent: {relative}"))?;
        let mut directory = prefix.to_path_buf();
        for component in parent.components() {
            let Component::Normal(component) = component else {
                return Err(format!("managed release directory is invalid: {relative}"));
            };
            directory.push(component);
            paths.insert(directory.clone(), false);
        }
    }

    paths
        .into_iter()
        .map(|(path, allow_system_symlink)| {
            inspect_trusted_release_directory(&path, expected_uid, allow_system_symlink)
        })
        .collect()
}

fn inspect_trusted_release_directory(
    path: &Path,
    expected_uid: u32,
    allow_system_symlink: bool,
) -> Result<TrustedReleasePath, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect release directory '{}': {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        if allow_system_symlink && metadata.uid() == 0 {
            return Ok(TrustedReleasePath {
                path: path.to_path_buf(),
                device: metadata.dev(),
                inode: metadata.ino(),
                uid: metadata.uid(),
                mode: metadata.mode(),
                symlink: true,
            });
        }
        return Err(format!(
            "managed release directory path contains a symlink: {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "managed release path is not a directory: {}",
            path.display()
        ));
    }
    validate_trusted_owner(path, &metadata, expected_uid)?;
    validate_not_group_or_world_writable(path, &metadata)?;
    Ok(TrustedReleasePath {
        path: path.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
        symlink: false,
    })
}

fn revalidate_release_directories(
    directories: &[TrustedReleasePath],
    expected_uid: u32,
) -> Result<(), String> {
    for before in directories {
        let after = fs::symlink_metadata(&before.path).map_err(|error| {
            format!(
                "cannot re-inspect release directory '{}': {error}",
                before.path.display()
            )
        })?;
        if after.dev() != before.device
            || after.ino() != before.inode
            || after.uid() != before.uid
            || after.mode() != before.mode
            || after.file_type().is_symlink() != before.symlink
        {
            return Err(format!(
                "managed release directory changed while it was being verified: {}",
                before.path.display()
            ));
        }
        if before.symlink && after.uid() != 0 {
            return Err(format!(
                "trusted system symlink changed ownership while it was being verified: {}",
                before.path.display()
            ));
        }
        if !before.symlink {
            if !after.is_dir() {
                return Err(format!(
                    "managed release path is not a directory: {}",
                    before.path.display()
                ));
            }
            validate_trusted_owner(&before.path, &after, expected_uid)?;
            validate_not_group_or_world_writable(&before.path, &after)?;
        }
    }
    Ok(())
}

fn validate_trusted_owner(
    path: &Path,
    metadata: &fs::Metadata,
    expected_uid: u32,
) -> Result<(), String> {
    if metadata.uid() != expected_uid && metadata.uid() != 0 {
        return Err(format!(
            "managed release path is owned by untrusted uid {} (expected effective uid {expected_uid} or root): {}",
            metadata.uid(),
            path.display()
        ));
    }
    Ok(())
}

fn validate_not_group_or_world_writable(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    if metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "managed release path is group/world-writable (mode {:04o}): {}",
            metadata.mode() & 0o7777,
            path.display()
        ));
    }
    Ok(())
}

fn parse_release_manifest(text: &str) -> Result<BTreeMap<&str, String>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(RELEASE_MANIFEST_FORMAT) {
        return Err("managed manifest has an unsupported format".into());
    }

    let mut metadata = BTreeSet::new();
    let mut hashes = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            return Err("managed manifest contains an empty entry".into());
        }
        if line.contains('\r') || line.contains('\0') {
            return Err("managed manifest contains invalid control characters".into());
        }

        if let Some(version) = line.strip_prefix("version=") {
            record_manifest_metadata(&mut metadata, "version", version, |value| !value.is_empty())?;
        } else if let Some(target) = line.strip_prefix("target=") {
            record_manifest_metadata(&mut metadata, "target", target, |value| !value.is_empty())?;
        } else if let Some(profile) = line.strip_prefix("path_profile=") {
            record_manifest_metadata(&mut metadata, "path_profile", profile, |value| {
                !value.is_empty()
            })?;
        } else if let Some(created) = line.strip_prefix("path_profile_created=") {
            record_manifest_metadata(&mut metadata, "path_profile_created", created, |value| {
                matches!(value, "0" | "1")
            })?;
        } else if let Some(entry) = line.strip_prefix("file=") {
            let (relative, digest) = entry
                .split_once('|')
                .ok_or_else(|| "managed manifest contains a malformed file entry".to_string())?;
            if !MANAGED_RELEASE_FILES
                .iter()
                .any(|(expected, _)| *expected == relative)
            {
                return Err(format!(
                    "managed manifest references unexpected file '{relative}'"
                ));
            }
            validate_sha256(digest)
                .map_err(|error| format!("managed file '{relative}': {error}"))?;
            if hashes
                .insert(relative, digest.to_ascii_lowercase())
                .is_some()
            {
                return Err(format!(
                    "managed manifest contains duplicate file entry '{relative}'"
                ));
            }
        } else {
            return Err("managed manifest contains an unknown entry".into());
        }
    }

    for field in ["version", "target", "path_profile", "path_profile_created"] {
        if !metadata.contains(field) {
            return Err(format!("managed manifest is missing {field} metadata"));
        }
    }
    for (relative, _) in MANAGED_RELEASE_FILES {
        if !hashes.contains_key(relative) {
            return Err(format!(
                "managed manifest is missing file entry '{relative}'"
            ));
        }
    }
    Ok(hashes)
}

fn record_manifest_metadata(
    seen: &mut BTreeSet<&str>,
    name: &'static str,
    value: &str,
    valid: impl FnOnce(&str) -> bool,
) -> Result<(), String> {
    if !seen.insert(name) {
        return Err(format!(
            "managed manifest contains duplicate {name} metadata"
        ));
    }
    if !valid(value) {
        return Err(format!("managed manifest contains invalid {name} metadata"));
    }
    Ok(())
}

fn validate_sha256(digest: &str) -> Result<(), String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SHA-256 digest must contain exactly 64 hexadecimal characters".into());
    }
    Ok(())
}

fn resolve_managed_release_path(base: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty() {
        return Err("managed file path is empty".into());
    }

    let mut path = base.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(format!(
                "managed file path must be relative without '.' or '..': {}",
                relative.display()
            ));
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect '{}': {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "managed file path contains a symlink: {}",
                path.display()
            ));
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(format!(
                "managed file path component is not a directory: {}",
                path.display()
            ));
        }
    }
    Ok(path)
}

fn hash_stable_managed_file(
    path: &Path,
    executable: bool,
    expected_uid: u32,
) -> Result<String, String> {
    let (mut file, before) = open_stable_regular_file(path)?;
    validate_trusted_release_file(path, &before, expected_uid, executable)?;

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    ensure_stable_path(path, &file, &before)?;
    Ok(hasher.finalize_hex())
}

fn validate_trusted_release_file(
    path: &Path,
    metadata: &fs::Metadata,
    expected_uid: u32,
    executable: bool,
) -> Result<(), String> {
    validate_trusted_owner(path, metadata, expected_uid)?;
    validate_not_group_or_world_writable(path, metadata)?;
    if metadata.mode() & 0o6000 != 0 {
        return Err(format!(
            "managed release file must not have set-user-ID or set-group-ID permissions (mode {:04o}): {}",
            metadata.mode() & 0o7777,
            path.display()
        ));
    }
    if metadata.mode() & 0o400 == 0 {
        return Err(format!(
            "managed release file is not owner-readable: {}",
            path.display()
        ));
    }
    if executable && metadata.mode() & 0o100 == 0 {
        return Err(format!(
            "managed release binary is not owner-executable: {}",
            path.display()
        ));
    }
    Ok(())
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length_bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            length_bytes: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.length_bytes = self.length_bytes.wrapping_add(bytes.len() as u64);
        if self.buffered != 0 {
            let take = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&bytes[..take]);
            self.buffered += take;
            bytes = &bytes[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while bytes.len() >= 64 {
            let mut block = [0_u8; 64];
            block.copy_from_slice(&bytes[..64]);
            self.compress(&block);
            bytes = &bytes[64..];
        }
        self.buffer[..bytes.len()].copy_from_slice(bytes);
        self.buffered = bytes.len();
    }

    fn finalize_hex(mut self) -> String {
        let bit_length = self.length_bytes.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buffer[self.buffered..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer = [0; 64];
        } else {
            self.buffer[self.buffered..56].fill(0);
        }
        self.buffer[56..].copy_from_slice(&bit_length.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut digest = String::with_capacity(64);
        for word in self.state {
            use std::fmt::Write as _;
            write!(&mut digest, "{word:08x}").expect("writing to a String cannot fail");
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

fn read_stable_trusted_regular_file(
    path: &Path,
    maximum_bytes: Option<u64>,
    expected_uid: u32,
    executable: bool,
) -> Result<Vec<u8>, String> {
    let (mut file, before) = open_stable_regular_file(path)?;
    validate_trusted_release_file(path, &before, expected_uid, executable)?;
    read_bounded_stable_file(path, &mut file, &before, maximum_bytes)
}

#[cfg(test)]
fn read_stable_regular_file(path: &Path, maximum_bytes: Option<u64>) -> Result<Vec<u8>, String> {
    let (mut file, before) = open_stable_regular_file(path)?;
    read_bounded_stable_file(path, &mut file, &before, maximum_bytes)
}

fn read_bounded_stable_file(
    path: &Path,
    file: &mut File,
    before: &fs::Metadata,
    maximum_bytes: Option<u64>,
) -> Result<Vec<u8>, String> {
    if let Some(maximum_bytes) = maximum_bytes
        && before.len() > maximum_bytes
    {
        return Err(format!(
            "file exceeds the {maximum_bytes}-byte limit: {}",
            path.display()
        ));
    }

    let read_limit = maximum_bytes
        .map(|maximum| maximum.saturating_add(1))
        .unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    (&mut *file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
    if let Some(maximum_bytes) = maximum_bytes
        && bytes.len() as u64 > maximum_bytes
    {
        return Err(format!(
            "file exceeds the {maximum_bytes}-byte limit: {}",
            path.display()
        ));
    }
    ensure_stable_path(path, file, before)?;
    Ok(bytes)
}

fn open_stable_regular_file(path: &Path) -> Result<(File, fs::Metadata), String> {
    let before = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect '{}': {error}", path.display()))?;
    if before.file_type().is_symlink() {
        return Err(format!("path is a symlink: {}", path.display()));
    }
    if !before.is_file() {
        return Err(format!("path is not a regular file: {}", path.display()));
    }
    if before.nlink() != 1 {
        return Err(format!(
            "file must have exactly one hard link: {}",
            path.display()
        ));
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("cannot open '{}': {error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("cannot inspect opened '{}': {error}", path.display()))?;
    if !same_identity(&before, &opened) {
        return Err(format!(
            "path identity changed while opening: {}",
            path.display()
        ));
    }
    Ok((file, opened))
}

fn ensure_stable_path(path: &Path, file: &File, before: &fs::Metadata) -> Result<(), String> {
    let after = file
        .metadata()
        .map_err(|error| format!("cannot re-inspect opened '{}': {error}", path.display()))?;
    let current = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot re-inspect '{}': {error}", path.display()))?;
    if current.file_type().is_symlink()
        || !current.is_file()
        || !same_identity(before, &after)
        || !same_identity(before, &current)
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
    {
        return Err(format!(
            "file changed while it was being verified: {}",
            path.display()
        ));
    }
    Ok(())
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino() && right.nlink() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        time::Duration,
    };

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize_hex()
    }

    fn managed_installation(prefix: &Path) -> PathBuf {
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::create_dir_all(prefix.join("share/doc/hardknock")).unwrap();
        fs::create_dir_all(prefix.join("share/hardknock")).unwrap();

        let files = [
            ("bin/hardknock", b"trusted hardknock".as_slice(), true),
            ("bin/hk-effect", b"trusted hk-effect".as_slice(), true),
            (
                "share/doc/hardknock/LICENSE",
                b"trusted license".as_slice(),
                false,
            ),
            (
                "share/doc/hardknock/NOTICE",
                b"trusted notice".as_slice(),
                false,
            ),
        ];
        let mut manifest = format!(
            "{RELEASE_MANIFEST_FORMAT}\nversion=0.22.0\ntarget=x86_64-unknown-linux-gnu\n\
             path_profile=-\npath_profile_created=0\n"
        );
        for (relative, bytes, is_executable) in files {
            let path = prefix.join(relative);
            fs::write(&path, bytes).unwrap();
            set_mode(&path, if is_executable { 0o700 } else { 0o600 });
            manifest.push_str(&format!("file={relative}|{}\n", sha256(bytes)));
        }

        let manifest_path = prefix.join(RELEASE_MANIFEST_RELATIVE_PATH);
        fs::write(&manifest_path, manifest).unwrap();
        set_mode(&manifest_path, 0o600);
        manifest_path
    }

    fn managed_release_check(manifest_path: &Path) -> Check {
        check_release_integrity(&ReleaseIntegrityInput::Managed {
            manifest_path: manifest_path.to_path_buf(),
        })
    }

    #[test]
    fn report_enforces_strict_exit_semantics_and_serializes() {
        let ready = Report::from_checks(vec![
            Check::passed("required", "ok", true),
            Check::unavailable("optional", "not configured", false),
        ]);
        assert_eq!(ready.status, ReportStatus::Ready);
        assert!(ready.strict_ready);
        assert_eq!(ready.strict_exit_code(), STRICT_READY_EXIT_CODE);
        assert_eq!(ready.unavailable, 1);

        let degraded = Report::from_checks(vec![Check::warning("disk", "low", true)]);
        assert_eq!(degraded.status, ReportStatus::Degraded);
        assert!(!degraded.strict_ready);
        assert_eq!(degraded.strict_exit_code(), STRICT_WARNING_EXIT_CODE);

        let unavailable = Report::from_checks(vec![Check::unavailable("schema", "missing", true)]);
        assert_eq!(unavailable.status, ReportStatus::NotReady);
        assert_eq!(unavailable.strict_exit_code(), STRICT_FAILURE_EXIT_CODE);

        let value = serde_json::to_value(&ready).unwrap();
        assert_eq!(value["status"], "ready");
        assert_eq!(value["checks"][1]["status"], "unavailable");
    }

    #[test]
    fn schema_check_requires_an_exact_observation() {
        assert_eq!(check_schema(30, Some(30)).status, CheckStatus::Passed);
        assert_eq!(check_schema(30, Some(29)).status, CheckStatus::Failed);
        let missing = check_schema(30, None);
        assert_eq!(missing.status, CheckStatus::Unavailable);
        assert!(missing.required);
    }

    #[test]
    fn private_paths_require_expected_owner_type_and_mode() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        set_mode(&home, 0o700);
        assert_eq!(
            check_private_path(&PrivatePathInput::home(&home)).status,
            CheckStatus::Passed
        );

        let database = home.join("hardknock.db");
        fs::write(&database, b"database").unwrap();
        set_mode(&database, 0o600);
        let input = PrivatePathInput::database(&database);
        assert_eq!(check_private_path(&input).status, CheckStatus::Passed);

        set_mode(&database, 0o640);
        assert_eq!(check_private_path(&input).status, CheckStatus::Failed);
        set_mode(&database, 0o600);

        let wrong_uid = nix::unistd::geteuid().as_raw().wrapping_add(1);
        assert_eq!(
            check_private_path_for_uid(&input, wrong_uid).status,
            CheckStatus::Failed
        );
    }

    #[test]
    fn private_path_checks_fail_closed_on_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        fs::write(&target, b"config").unwrap();
        set_mode(&target, 0o600);
        symlink(&target, &link).unwrap();

        let check = check_private_path(&PrivatePathInput::configuration(&link, true));
        assert_eq!(check.status, CheckStatus::Failed);
        assert!(check.summary.contains("symlink"));
    }

    #[test]
    fn missing_optional_private_path_is_explicitly_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let check = check_private_path(&PrivatePathInput::configuration(
            temp.path().join("missing.toml"),
            false,
        ));
        assert_eq!(check.status, CheckStatus::Unavailable);
        assert!(!check.required);
        assert_eq!(check.strict_exit_code(), STRICT_READY_EXIT_CODE);
    }

    #[test]
    fn disk_thresholds_distinguish_warning_and_failure() {
        let thresholds = DiskThresholds {
            minimum_available_bytes: 100,
            warning_available_bytes: 200,
        };
        assert_eq!(
            evaluate_disk(Path::new("/storage"), thresholds, 250, 1_000).status,
            CheckStatus::Passed
        );
        assert_eq!(
            evaluate_disk(Path::new("/storage"), thresholds, 150, 1_000).status,
            CheckStatus::Warning
        );
        assert_eq!(
            evaluate_disk(Path::new("/storage"), thresholds, 50, 1_000).status,
            CheckStatus::Failed
        );

        let temp = tempfile::tempdir().unwrap();
        let live = check_available_disk(Some(&DiskInput {
            path: temp.path().to_path_buf(),
            thresholds: DiskThresholds {
                minimum_available_bytes: 0,
                warning_available_bytes: 0,
            },
        }));
        assert_eq!(live.status, CheckStatus::Passed);
    }

    #[test]
    fn stale_runtime_descriptions_are_strict_findings() {
        assert_eq!(
            check_stale_runtime_paths(None).status,
            CheckStatus::Unavailable
        );
        assert_eq!(
            check_stale_runtime_paths(Some(&[])).status,
            CheckStatus::Passed
        );
        let stale = vec!["stale bridge socket at runtime/bridge.sock".into()];
        let check = check_stale_runtime_paths(Some(&stale));
        assert_eq!(check.status, CheckStatus::Failed);
        assert_eq!(check.details["count"], "1");
    }

    #[test]
    fn backup_checks_distinguish_bundle_verification_from_staged_restore() {
        let healthy = check_backup(Some(&BackupInput {
            age_seconds: Some(60),
            maximum_age_seconds: 120,
            bundle_verification: BackupVerification::Verified,
            staged_restore: BackupVerification::Verified,
            required: true,
        }));
        assert!(
            healthy
                .iter()
                .all(|check| check.status == CheckStatus::Passed)
        );

        let unhealthy = check_backup(Some(&BackupInput {
            age_seconds: Some(121),
            maximum_age_seconds: 120,
            bundle_verification: BackupVerification::Verified,
            staged_restore: BackupVerification::Unavailable {
                reason: "staged restore drill has not run".into(),
            },
            required: true,
        }));
        assert_eq!(unhealthy[0].status, CheckStatus::Failed);
        assert_eq!(unhealthy[1].status, CheckStatus::Passed);
        assert!(unhealthy[1].summary.contains("cryptographic bundle"));
        assert_eq!(unhealthy[2].status, CheckStatus::Unavailable);
        assert!(unhealthy[2].summary.contains("staged restore drill"));
        assert_eq!(
            Report::from_checks(unhealthy).strict_exit_code(),
            STRICT_FAILURE_EXIT_CODE
        );

        let missing = check_backup(None);
        assert!(
            missing
                .iter()
                .all(|check| check.status == CheckStatus::Unavailable && check.required)
        );
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn release_manifest_verifies_managed_hashes_and_detects_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());

        let healthy = managed_release_check(&manifest);
        assert_eq!(healthy.status, CheckStatus::Passed, "{:?}", healthy.details);
        assert_eq!(healthy.details["verified_files"], "4");

        fs::write(
            temp.path().join("share/doc/hardknock/NOTICE"),
            b"tampered notice",
        )
        .unwrap();
        let check = managed_release_check(&manifest);
        assert_eq!(check.status, CheckStatus::Failed);
        assert!(check.details["error"].contains("SHA-256 mismatch"));
    }

    #[test]
    fn release_manifest_rejects_symlinked_manifest_and_managed_paths() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());
        let binary = temp.path().join("bin/hardknock");
        let real_binary = temp.path().join("bin/real-hardknock");
        fs::rename(&binary, &real_binary).unwrap();
        symlink(&real_binary, &binary).unwrap();

        let binary_check = managed_release_check(&manifest);
        assert_eq!(binary_check.status, CheckStatus::Failed);
        assert!(binary_check.details["error"].contains("symlink"));

        fs::remove_file(&binary).unwrap();
        fs::rename(&real_binary, &binary).unwrap();
        let real_manifest = temp.path().join("share/hardknock/install-manifest-v1.real");
        fs::rename(&manifest, &real_manifest).unwrap();
        symlink(&real_manifest, &manifest).unwrap();
        let manifest_check = managed_release_check(&manifest);
        assert_eq!(manifest_check.status, CheckStatus::Failed);
        assert!(manifest_check.details["error"].contains("symlink"));
    }

    #[test]
    fn release_manifest_rejects_group_writable_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());
        set_mode(temp.path(), 0o770);

        let check = managed_release_check(&manifest);
        assert_eq!(check.status, CheckStatus::Failed);
        assert!(check.details["error"].contains("group/world-writable"));
        assert!(check.details["error"].contains(temp.path().to_str().unwrap()));
    }

    #[test]
    fn release_manifest_rejects_group_writable_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());
        set_mode(&manifest, 0o620);

        let check = managed_release_check(&manifest);
        assert_eq!(check.status, CheckStatus::Failed);
        assert!(check.details["error"].contains("group/world-writable"));
        assert!(check.details["error"].contains("install-manifest-v1"));
    }

    #[test]
    fn release_manifest_rejects_group_writable_binary() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());
        let binary = temp.path().join("bin/hardknock");
        set_mode(&binary, 0o720);

        let check = managed_release_check(&manifest);
        assert_eq!(check.status, CheckStatus::Failed);
        assert!(check.details["error"].contains("group/world-writable"));
        assert!(check.details["error"].contains("bin/hardknock"));
    }

    #[test]
    fn missing_manifest_is_required_for_managed_install_but_honest_for_source_build() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("share/hardknock/install-manifest-v1");
        let managed = managed_release_check(&missing);
        assert_eq!(managed.status, CheckStatus::Unavailable);
        assert!(managed.required);
        assert!(managed.summary.contains("Managed installation requires"));
        assert_eq!(managed.strict_exit_code(), STRICT_FAILURE_EXIT_CODE);

        let source = check_release_integrity(&ReleaseIntegrityInput::SourceBuild {
            executable_path: Some(temp.path().join("target/debug/hardknock")),
        });
        assert_eq!(source.status, CheckStatus::Warning);
        assert!(!source.required);
        assert!(source.summary.contains("Source/development"));
        assert!(source.summary.contains("non-strict diagnostics"));
        assert_eq!(source.strict_exit_code(), STRICT_WARNING_EXIT_CODE);
    }

    #[test]
    fn release_manifest_rejects_incomplete_duplicate_and_misplaced_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = managed_installation(temp.path());
        let original = fs::read_to_string(&manifest).unwrap();

        fs::write(
            &manifest,
            original.replace(
                "file=share/doc/hardknock/NOTICE|",
                "file=share/doc/hardknock/UNKNOWN|",
            ),
        )
        .unwrap();
        let unexpected = managed_release_check(&manifest);
        assert!(
            unexpected.details["error"].contains("unexpected file"),
            "{:?}",
            unexpected.details
        );

        fs::write(&manifest, format!("{original}version=duplicate\n")).unwrap();
        assert!(managed_release_check(&manifest).details["error"].contains("duplicate version"));

        fs::write(&manifest, original).unwrap();
        let misplaced = temp.path().join("install-manifest-v1");
        fs::copy(&manifest, &misplaced).unwrap();
        assert!(
            managed_release_check(&misplaced).details["error"]
                .contains(RELEASE_MANIFEST_RELATIVE_PATH)
        );
    }

    #[test]
    fn aggregate_runner_requires_core_observations() {
        let report = run(&Input {
            expected_schema: 30,
            actual_schema: Some(30),
            private_paths: Vec::new(),
            disk: None,
            stale_runtime_paths: None,
            backup: None,
            release: ReleaseIntegrityInput::SourceBuild {
                executable_path: None,
            },
        });
        assert_eq!(report.status, ReportStatus::NotReady);
        assert!(!report.strict_ready);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.id == "filesystem.home" && check.required)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.id == "storage.available_disk" && check.required)
        );
    }

    #[test]
    fn stable_file_check_tolerates_read_time_but_not_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        fs::write(&path, b"stable").unwrap();
        let bytes = read_stable_regular_file(&path, Some(64)).unwrap();
        assert_eq!(bytes, b"stable");

        std::thread::sleep(Duration::from_millis(1));
        fs::write(&path, vec![b'x'; 65]).unwrap();
        assert!(read_stable_regular_file(&path, Some(64)).is_err());
    }
}
