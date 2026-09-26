// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::{
    error::Error as StdError,
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use rustix::{
    fd::OwnedFd,
    fs::{
        AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, Stat, flock, fstat, open,
        openat, renameat_with, statat, unlinkat,
    },
};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const GIB: u64 = 1024 * 1024 * 1024;
const TRANSIENT_DIRECTORY: &str = "transient";
const TRANSIENT_DIRECTORY_PREFIX: &str = "hk-transient-";
const TRANSIENT_LEASE_MARKER: &str = ".hardknock-active-v1";
const TRANSIENT_LEASE_CONTENT: &[u8] = b"hardknock-transient-lease-v1\n";
const MIN_ABANDONED_LEASE_AGE: Duration = Duration::from_secs(5 * 60);
// Experiments permit at most 32 concurrent realities. A capacity scan can
// therefore observe several separately leased scratch directories finishing
// one after another while it walks the managed transient namespace.
const MAX_TRANSIENT_INVENTORY_RETRIES: usize = 32;

/// A private transient directory that remains protected from retention while
/// this value is alive.
///
/// The directory lock is released automatically after a crash. The durable
/// marker then keeps the abandoned directory out of retention for a bounded
/// grace period before its regular files become reclaimable.
#[cfg(unix)]
pub(crate) struct LeasedTransientDir {
    // Field order matters: remove the directory while the lease is still held.
    directory: tempfile::TempDir,
    _lease: OwnedFd,
}

#[cfg(unix)]
impl LeasedTransientDir {
    pub(crate) fn create(transient_root: impl AsRef<Path>) -> io::Result<Self> {
        let transient_root = transient_root.as_ref();
        fs::create_dir_all(transient_root)?;
        let root_metadata = fs::symlink_metadata(transient_root)?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "transient artifact root {} must be a real directory",
                    transient_root.display()
                ),
            ));
        }

        let directory = tempfile::Builder::new()
            .prefix(TRANSIENT_DIRECTORY_PREFIX)
            .tempdir_in(transient_root)?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let lease = open(
            directory.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_to_io)?;
        flock(&lease, FlockOperation::LockExclusive).map_err(errno_to_io)?;

        let marker_path = directory.path().join(TRANSIENT_LEASE_MARKER);
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker_path)?;
        marker.write_all(TRANSIENT_LEASE_CONTENT)?;
        marker.sync_all()?;

        Ok(Self {
            directory,
            _lease: lease,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    #[cfg(test)]
    fn abandon_for_test(self) -> PathBuf {
        let Self { directory, _lease } = self;
        let path = directory.keep();
        drop(_lease);
        path
    }
}

/// Artifact retention limits.
///
/// The supplied root is the `artifacts/` directory. Only regular files below
/// its direct `transient/` child are eligible for pruning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoragePolicy {
    pub max_bytes: u64,
    pub max_files: u64,
    pub min_free_bytes: u64,
    pub max_scan_entries: u64,
    pub max_prune_items: u64,
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            max_bytes: 2 * GIB,
            max_files: 20_000,
            min_free_bytes: GIB,
            max_scan_entries: 100_000,
            max_prune_items: 10_000,
        }
    }
}

impl StoragePolicy {
    pub fn validate(&self) -> Result<(), StoragePolicyError> {
        if self.max_bytes == 0 {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_bytes must be greater than zero",
            ));
        }
        if self.max_files == 0 {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_files must be greater than zero",
            ));
        }
        if self.max_scan_entries == 0 {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_scan_entries must be greater than zero",
            ));
        }
        if self.max_prune_items == 0 {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_prune_items must be greater than zero",
            ));
        }
        if self.max_prune_items > self.max_scan_entries {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_prune_items cannot exceed max_scan_entries",
            ));
        }
        if usize::try_from(self.max_scan_entries).is_err() {
            return Err(StoragePolicyError::InvalidPolicy(
                "max_scan_entries exceeds this platform's addressable range",
            ));
        }
        Ok(())
    }

    pub fn inventory(
        &self,
        artifacts_root: impl AsRef<Path>,
    ) -> Result<StorageInventory, StoragePolicyError> {
        self.validate()?;
        inventory_artifacts(self, artifacts_root.as_ref())
    }

    /// Plans or explicitly applies retention.
    ///
    /// `PruneMode::DryRun` never mutates the filesystem. `PruneMode::Apply`
    /// first rejects every observed symlink, special entry, and hard-linked
    /// regular file before deleting any reclaimable file.
    pub fn prune(
        &self,
        artifacts_root: impl AsRef<Path>,
        mode: PruneMode,
    ) -> Result<PruneReport, StoragePolicyError> {
        let artifacts_root = artifacts_root.as_ref();
        let inventory = self.inventory(artifacts_root)?;
        let plan = build_prune_plan(self, &inventory)?;

        if mode == PruneMode::DryRun {
            let limits_met = plan.projected.limits.within_limits;
            return Ok(PruneReport {
                mode,
                before: inventory.snapshot(),
                projected: plan.projected,
                final_state: None,
                items: plan.items,
                reclaimed: plan.reclaimed,
                blocker: plan.blocker,
                limits_met,
            });
        }

        #[cfg(not(unix))]
        {
            let _ = plan;
            return Err(StoragePolicyError::ApplyUnsupported);
        }

        #[cfg(unix)]
        {
            if let Some(blocker) = plan.blocker {
                return Err(StoragePolicyError::PruneBlocked(blocker));
            }

            let candidates = reclaimable_candidates(&inventory);
            let mut current = inventory.snapshot();
            let mut deleted = Vec::new();
            let mut reclaimed = StorageUsage::default();

            for candidate in candidates {
                if current.limits.within_limits {
                    break;
                }
                if deleted.len() as u64 >= self.max_prune_items {
                    break;
                }

                delete_candidate_descriptor_relative(
                    artifacts_root,
                    inventory.root_device,
                    inventory.root_inode,
                    &candidate,
                )?;

                reclaimed = reclaimed.checked_add(StorageUsage {
                    bytes: candidate.bytes,
                    files: 1,
                })?;
                current.usage = current.usage.checked_sub(StorageUsage {
                    bytes: candidate.bytes,
                    files: 1,
                })?;
                current.reclaimable = current.reclaimable.checked_sub(StorageUsage {
                    bytes: candidate.bytes,
                    files: 1,
                })?;
                current.available_bytes = available_space(artifacts_root)?;
                current.limits =
                    StorageLimitState::evaluate(self, current.usage, current.available_bytes);
                deleted.push(PruneItem::from_entry(&candidate));
            }

            let after = self.inventory(artifacts_root)?;
            let final_state = after.snapshot();
            let limits_met = final_state.limits.within_limits;
            let blocker = if limits_met {
                None
            } else if final_state.unsafe_entries > 0 {
                Some(PruneBlocker::UnsafeEntries {
                    count: final_state.unsafe_entries,
                })
            } else if deleted.len() as u64 >= self.max_prune_items
                && final_state.reclaimable.files > 0
            {
                Some(PruneBlocker::PruneItemLimit {
                    required_items: self.max_prune_items.saturating_add(1),
                    max_prune_items: self.max_prune_items,
                })
            } else {
                Some(PruneBlocker::InsufficientReclaimable {
                    required_bytes: final_state
                        .limits
                        .bytes_over
                        .max(final_state.limits.free_space_shortfall_bytes),
                    required_files: final_state.limits.files_over,
                    reclaimable_bytes: final_state.reclaimable.bytes,
                    reclaimable_files: final_state.reclaimable.files,
                })
            };
            Ok(PruneReport {
                mode,
                before: inventory.snapshot(),
                projected: plan.projected,
                final_state: Some(final_state),
                items: deleted,
                reclaimed,
                blocker,
                limits_met,
            })
        }
    }

    /// Verifies room for a prospective write without deleting anything.
    ///
    /// Requested usage is treated as protected evidence. On failure, the
    /// returned numeric error is bounded and directs the caller to retention's
    /// dry-run and explicit apply flow.
    pub fn ensure_capacity(
        &self,
        artifacts_root: impl AsRef<Path>,
        requested_bytes: u64,
        requested_files: u64,
    ) -> Result<CapacityReport, StoragePolicyError> {
        self.ensure_capacity_with_reservations(
            artifacts_root,
            requested_bytes,
            requested_files,
            StorageUsage::default(),
        )
    }

    pub(crate) fn ensure_capacity_with_reservations(
        &self,
        artifacts_root: impl AsRef<Path>,
        requested_bytes: u64,
        requested_files: u64,
        reserved: StorageUsage,
    ) -> Result<CapacityReport, StoragePolicyError> {
        let inventory = self.inventory(artifacts_root)?;
        let requested = StorageUsage {
            bytes: requested_bytes,
            files: requested_files,
        };
        let projected_write = reserved.checked_add(requested)?;
        let mut projected = inventory.snapshot();
        projected.usage = projected.usage.checked_add(projected_write)?;
        projected.protected = projected.protected.checked_add(projected_write)?;
        projected.available_bytes = projected
            .available_bytes
            .saturating_sub(projected_write.bytes);
        projected.limits =
            StorageLimitState::evaluate(self, projected.usage, projected.available_bytes);

        let report = CapacityReport {
            requested,
            reserved,
            current: inventory.snapshot(),
            projected,
        };
        if report.projected.limits.within_limits {
            Ok(report)
        } else {
            Err(StoragePolicyError::InsufficientCapacity(Box::new(
                CapacityError::from_report(self, &report),
            )))
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsage {
    pub bytes: u64,
    pub files: u64,
}

impl StorageUsage {
    pub(crate) fn checked_add(self, other: Self) -> Result<Self, StoragePolicyError> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_add(other.bytes)
                .ok_or(StoragePolicyError::ArithmeticOverflow)?,
            files: self
                .files
                .checked_add(other.files)
                .ok_or(StoragePolicyError::ArithmeticOverflow)?,
        })
    }

    fn checked_sub(self, other: Self) -> Result<Self, StoragePolicyError> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_sub(other.bytes)
                .ok_or(StoragePolicyError::ArithmeticOverflow)?,
            files: self
                .files
                .checked_sub(other.files)
                .ok_or(StoragePolicyError::ArithmeticOverflow)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageLimitState {
    pub over_bytes: bool,
    pub bytes_over: u64,
    pub over_files: bool,
    pub files_over: u64,
    pub below_min_free_space: bool,
    pub free_space_shortfall_bytes: u64,
    pub within_limits: bool,
}

impl StorageLimitState {
    fn evaluate(policy: &StoragePolicy, usage: StorageUsage, available_bytes: u64) -> Self {
        let bytes_over = usage.bytes.saturating_sub(policy.max_bytes);
        let files_over = usage.files.saturating_sub(policy.max_files);
        let free_space_shortfall_bytes = policy.min_free_bytes.saturating_sub(available_bytes);
        let over_bytes = bytes_over > 0;
        let over_files = files_over > 0;
        let below_min_free_space = free_space_shortfall_bytes > 0;
        Self {
            over_bytes,
            bytes_over,
            over_files,
            files_over,
            below_min_free_space,
            free_space_shortfall_bytes,
            within_limits: !over_bytes && !over_files && !below_min_free_space,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactClassification {
    Reclaimable,
    Protected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEntryKind {
    RegularFile,
    Symlink,
    NonRegular,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInventoryEntry {
    pub relative_path: PathBuf,
    pub classification: ArtifactClassification,
    pub kind: ArtifactEntryKind,
    pub bytes: u64,
    pub modified_unix_seconds: u64,
    pub link_count: Option<u64>,
    #[serde(skip)]
    device: Option<u64>,
    #[serde(skip)]
    inode: Option<u64>,
    #[serde(skip)]
    modified_nanoseconds: Option<i64>,
    #[serde(skip)]
    change_unix_seconds: Option<i64>,
    #[serde(skip)]
    change_nanoseconds: Option<i64>,
}

impl ArtifactInventoryEntry {
    fn unsafe_for_apply(&self) -> bool {
        if self.kind != ArtifactEntryKind::RegularFile {
            return true;
        }
        #[cfg(unix)]
        {
            self.link_count != Some(1)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub usage: StorageUsage,
    pub protected: StorageUsage,
    pub reclaimable: StorageUsage,
    pub available_bytes: u64,
    pub scanned_entries: u64,
    pub unsafe_entries: u64,
    pub limits: StorageLimitState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInventory {
    pub root: PathBuf,
    pub usage: StorageUsage,
    pub protected: StorageUsage,
    pub reclaimable: StorageUsage,
    pub available_bytes: u64,
    pub scanned_entries: u64,
    pub unsafe_entries: u64,
    pub limits: StorageLimitState,
    pub entries: Vec<ArtifactInventoryEntry>,
    #[serde(skip)]
    root_device: Option<u64>,
    #[serde(skip)]
    root_inode: Option<u64>,
}

impl StorageInventory {
    pub fn snapshot(&self) -> StorageSnapshot {
        StorageSnapshot {
            usage: self.usage,
            protected: self.protected,
            reclaimable: self.reclaimable,
            available_bytes: self.available_bytes,
            scanned_entries: self.scanned_entries,
            unsafe_entries: self.unsafe_entries,
            limits: self.limits,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneMode {
    DryRun,
    Apply,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneItem {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub modified_unix_seconds: u64,
}

impl PruneItem {
    fn from_entry(entry: &ArtifactInventoryEntry) -> Self {
        Self {
            relative_path: entry.relative_path.clone(),
            bytes: entry.bytes,
            modified_unix_seconds: entry.modified_unix_seconds,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PruneBlocker {
    UnsafeEntries {
        count: u64,
    },
    InsufficientReclaimable {
        required_bytes: u64,
        required_files: u64,
        reclaimable_bytes: u64,
        reclaimable_files: u64,
    },
    PruneItemLimit {
        required_items: u64,
        max_prune_items: u64,
    },
}

impl fmt::Display for PruneBlocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafeEntries { count } => write!(
                formatter,
                "apply blocked by {count} unsafe artifact entries; inspect the inventory and replace symlinks, special files, or hardlinks before retrying"
            ),
            Self::InsufficientReclaimable {
                required_bytes,
                required_files,
                reclaimable_bytes,
                reclaimable_files,
            } => write!(
                formatter,
                "retention cannot meet quotas without protected evidence: need {required_bytes} bytes and {required_files} files reclaimed, but only {reclaimable_bytes} bytes and {reclaimable_files} files are reclaimable"
            ),
            Self::PruneItemLimit {
                required_items,
                max_prune_items,
            } => write!(
                formatter,
                "retention needs {required_items} deletions, above max_prune_items={max_prune_items}; raise the bounded batch limit deliberately and retry"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub mode: PruneMode,
    pub before: StorageSnapshot,
    pub projected: StorageSnapshot,
    pub final_state: Option<StorageSnapshot>,
    pub items: Vec<PruneItem>,
    pub reclaimed: StorageUsage,
    pub blocker: Option<PruneBlocker>,
    pub limits_met: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityReport {
    pub requested: StorageUsage,
    pub reserved: StorageUsage,
    pub current: StorageSnapshot,
    pub projected: StorageSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityError {
    pub requested: StorageUsage,
    pub reserved: StorageUsage,
    pub current: StorageUsage,
    pub projected: StorageUsage,
    pub limits: StorageUsage,
    pub available_bytes: u64,
    pub projected_available_bytes: u64,
    pub min_free_bytes: u64,
    pub reclaimable: StorageUsage,
    pub state: StorageLimitState,
}

impl CapacityError {
    fn from_report(policy: &StoragePolicy, report: &CapacityReport) -> Self {
        Self {
            requested: report.requested,
            reserved: report.reserved,
            current: report.current.usage,
            projected: report.projected.usage,
            limits: StorageUsage {
                bytes: policy.max_bytes,
                files: policy.max_files,
            },
            available_bytes: report.current.available_bytes,
            projected_available_bytes: report.projected.available_bytes,
            min_free_bytes: policy.min_free_bytes,
            reclaimable: report.current.reclaimable,
            state: report.projected.limits,
        }
    }
}

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "insufficient artifact capacity for {} bytes/{} files with {} bytes/{} files already reserved: projected usage {} bytes/{} files (limits {} bytes/{} files), projected free space {} bytes (minimum {}), reclaimable {} bytes/{} files; run retention dry-run, then explicit apply, and retry",
            self.requested.bytes,
            self.requested.files,
            self.reserved.bytes,
            self.reserved.files,
            self.projected.bytes,
            self.projected.files,
            self.limits.bytes,
            self.limits.files,
            self.projected_available_bytes,
            self.min_free_bytes,
            self.reclaimable.bytes,
            self.reclaimable.files,
        )
    }
}

#[derive(Debug)]
pub enum StoragePolicyError {
    InvalidPolicy(&'static str),
    InvalidRoot {
        path: PathBuf,
        reason: &'static str,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    ScanLimitExceeded {
        limit: u64,
    },
    PruneBlocked(PruneBlocker),
    EntryChanged {
        path: PathBuf,
    },
    InsufficientCapacity(Box<CapacityError>),
    ArithmeticOverflow,
    ApplyUnsupported,
}

impl fmt::Display for StoragePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(message) => write!(formatter, "invalid storage policy: {message}"),
            Self::InvalidRoot { path, reason } => {
                write!(
                    formatter,
                    "unsafe artifacts root {}: {reason}",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::ScanLimitExceeded { limit } => write!(
                formatter,
                "artifact inventory exceeded max_scan_entries={limit}; raise the limit deliberately after inspecting the tree"
            ),
            Self::PruneBlocked(blocker) => blocker.fmt(formatter),
            Self::EntryChanged { path } => write!(
                formatter,
                "reclaimable artifact {} changed after inventory; no deletion was attempted for that entry",
                path.display()
            ),
            Self::InsufficientCapacity(error) => error.fmt(formatter),
            Self::ArithmeticOverflow => {
                formatter.write_str("artifact usage arithmetic exceeded supported bounds")
            }
            Self::ApplyUnsupported => formatter.write_str(
                "retention apply requires Unix hard-link inspection; dry-run remains available",
            ),
        }
    }
}

impl StdError for StoragePolicyError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

struct PrunePlan {
    projected: StorageSnapshot,
    items: Vec<PruneItem>,
    reclaimed: StorageUsage,
    blocker: Option<PruneBlocker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryRetention {
    Protected,
    Transient,
    ActiveLease,
    AbandonedLease { marker_modified: u64 },
}

fn inventory_artifacts(
    policy: &StoragePolicy,
    artifacts_root: &Path,
) -> Result<StorageInventory, StoragePolicyError> {
    inventory_artifacts_with_retry(policy, artifacts_root, &mut |_, _| {})
}

fn inventory_artifacts_with_retry<F>(
    policy: &StoragePolicy,
    artifacts_root: &Path,
    after_directory_read: &mut F,
) -> Result<StorageInventory, StoragePolicyError>
where
    F: FnMut(&Path, &[PathBuf]),
{
    for attempt in 0..=MAX_TRANSIENT_INVENTORY_RETRIES {
        match inventory_artifacts_once(policy, artifacts_root, after_directory_read) {
            Err(error)
                if attempt < MAX_TRANSIENT_INVENTORY_RETRIES
                    && retryable_transient_disappearance(artifacts_root, &error) =>
            {
                std::thread::yield_now();
                continue;
            }
            result => return result,
        }
    }
    unreachable!("bounded inventory retry loop always returns")
}

fn retryable_transient_disappearance(artifacts_root: &Path, error: &StoragePolicyError) -> bool {
    matches!(
        error,
        StoragePolicyError::Io { path, source, .. }
            if source.kind() == io::ErrorKind::NotFound
                && is_managed_transient_path(artifacts_root, path)
    )
}

fn is_managed_transient_path(artifacts_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(artifacts_root) else {
        return false;
    };
    let mut components = relative.components();
    if !matches!(
        components.next(),
        Some(Component::Normal(name)) if name == TRANSIENT_DIRECTORY
    ) {
        return false;
    }
    matches!(
        components.next(),
        Some(Component::Normal(name))
            if name
                .to_str()
                .is_some_and(|name| name.starts_with(TRANSIENT_DIRECTORY_PREFIX))
    )
}

fn inventory_artifacts_once<F>(
    policy: &StoragePolicy,
    artifacts_root: &Path,
    after_directory_read: &mut F,
) -> Result<StorageInventory, StoragePolicyError>
where
    F: FnMut(&Path, &[PathBuf]),
{
    let root_metadata = fs::symlink_metadata(artifacts_root)
        .map_err(|error| io_error("inspect artifacts root", artifacts_root, error))?;
    if root_metadata.file_type().is_symlink() {
        return Err(StoragePolicyError::InvalidRoot {
            path: artifacts_root.to_path_buf(),
            reason: "symlinks are not accepted",
        });
    }
    if !root_metadata.is_dir() {
        return Err(StoragePolicyError::InvalidRoot {
            path: artifacts_root.to_path_buf(),
            reason: "root must be a directory",
        });
    }
    #[cfg(unix)]
    let (root_device, root_inode) = (Some(root_metadata.dev()), Some(root_metadata.ino()));
    #[cfg(not(unix))]
    let (root_device, root_inode) = (None, None);

    let mut usage = StorageUsage::default();
    let mut protected = StorageUsage::default();
    let mut reclaimable = StorageUsage::default();
    let mut unsafe_entries = 0_u64;
    let mut scanned_entries = 0_u64;
    let mut entries = Vec::new();
    let inventory_time = unix_seconds(SystemTime::now());
    let mut pending_directories =
        vec![(artifacts_root.to_path_buf(), DirectoryRetention::Protected)];

    while let Some((directory, retention)) = pending_directories.pop() {
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| io_error("inspect artifact directory", &directory, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoragePolicyError::EntryChanged {
                path: relative_or_full(artifacts_root, &directory),
            });
        }

        let directory_entries = fs::read_dir(&directory)
            .map_err(|error| io_error("read artifact directory", &directory, error))?;
        let mut children = Vec::new();
        for entry in directory_entries {
            scanned_entries = scanned_entries
                .checked_add(1)
                .ok_or(StoragePolicyError::ArithmeticOverflow)?;
            if scanned_entries > policy.max_scan_entries {
                return Err(StoragePolicyError::ScanLimitExceeded {
                    limit: policy.max_scan_entries,
                });
            }
            children.push(
                entry
                    .map_err(|error| io_error("read artifact entry", &directory, error))?
                    .path(),
            );
        }
        children.sort();
        after_directory_read(&directory, &children);

        let mut child_directories = Vec::new();
        for path in children {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| io_error("inspect artifact entry", &path, error))?;
            if metadata.file_type().is_dir() {
                let child_retention =
                    retention_for_directory(artifacts_root, &path, retention, &metadata)?;
                child_directories.push((path, child_retention));
                continue;
            }

            let relative_path = path
                .strip_prefix(artifacts_root)
                .map_err(|_| StoragePolicyError::EntryChanged { path: path.clone() })?
                .to_path_buf();
            let kind = if metadata.file_type().is_symlink() {
                ArtifactEntryKind::Symlink
            } else if metadata.is_file() {
                ArtifactEntryKind::RegularFile
            } else {
                ArtifactEntryKind::NonRegular
            };
            let bytes = if kind == ArtifactEntryKind::RegularFile {
                metadata.len()
            } else {
                0
            };
            let modified_unix_seconds = modified_seconds(&metadata, &path)?;
            let classification = classify(
                &relative_path,
                retention,
                modified_unix_seconds,
                inventory_time,
            );

            #[cfg(unix)]
            let (
                link_count,
                device,
                inode,
                modified_nanoseconds,
                change_unix_seconds,
                change_nanoseconds,
            ) = (
                Some(metadata.nlink()),
                Some(metadata.dev()),
                Some(metadata.ino()),
                Some(metadata.mtime_nsec()),
                Some(metadata.ctime()),
                Some(metadata.ctime_nsec()),
            );
            #[cfg(not(unix))]
            let (
                link_count,
                device,
                inode,
                modified_nanoseconds,
                change_unix_seconds,
                change_nanoseconds,
            ) = (None, None, None, None, None, None);

            let entry = ArtifactInventoryEntry {
                relative_path,
                classification,
                kind,
                bytes,
                modified_unix_seconds,
                link_count,
                device,
                inode,
                modified_nanoseconds,
                change_unix_seconds,
                change_nanoseconds,
            };
            if entry.unsafe_for_apply() {
                unsafe_entries = unsafe_entries
                    .checked_add(1)
                    .ok_or(StoragePolicyError::ArithmeticOverflow)?;
            }
            if kind == ArtifactEntryKind::RegularFile {
                let entry_usage = StorageUsage { bytes, files: 1 };
                usage = usage.checked_add(entry_usage)?;
                match classification {
                    ArtifactClassification::Reclaimable => {
                        reclaimable = reclaimable.checked_add(entry_usage)?;
                    }
                    ArtifactClassification::Protected => {
                        protected = protected.checked_add(entry_usage)?;
                    }
                }
            }
            entries.push(entry);
        }

        child_directories.sort_by(|left, right| left.0.cmp(&right.0));
        pending_directories.extend(child_directories.into_iter().rev());
    }

    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let available_bytes = available_space(artifacts_root)?;
    let limits = StorageLimitState::evaluate(policy, usage, available_bytes);
    Ok(StorageInventory {
        root: artifacts_root.to_path_buf(),
        usage,
        protected,
        reclaimable,
        available_bytes,
        scanned_entries,
        unsafe_entries,
        limits,
        entries,
        root_device,
        root_inode,
    })
}

fn retention_for_directory(
    artifacts_root: &Path,
    directory: &Path,
    parent_retention: DirectoryRetention,
    metadata: &fs::Metadata,
) -> Result<DirectoryRetention, StoragePolicyError> {
    let relative =
        directory
            .strip_prefix(artifacts_root)
            .map_err(|_| StoragePolicyError::EntryChanged {
                path: directory.to_path_buf(),
            })?;
    if relative == Path::new(TRANSIENT_DIRECTORY) {
        return Ok(DirectoryRetention::Transient);
    }
    if relative.parent() != Some(Path::new(TRANSIENT_DIRECTORY)) {
        return Ok(parent_retention);
    }

    inspect_transient_lease(directory, metadata)
}

#[cfg(unix)]
fn inspect_transient_lease(
    directory: &Path,
    metadata: &fs::Metadata,
) -> Result<DirectoryRetention, StoragePolicyError> {
    let descriptor = open(
        directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        io_error(
            "open transient artifact directory",
            directory,
            errno_to_io(error),
        )
    })?;
    let descriptor_stat = fstat(&descriptor).map_err(|error| {
        io_error(
            "inspect transient artifact directory",
            directory,
            errno_to_io(error),
        )
    })?;
    if FileType::from_raw_mode(descriptor_stat.st_mode) != FileType::Directory
        || stat_device(&descriptor_stat) != Some(metadata.dev())
        || stat_inode(&descriptor_stat) != Some(metadata.ino())
    {
        return Err(StoragePolicyError::EntryChanged {
            path: directory.to_path_buf(),
        });
    }

    let marker_stat = match statat(
        &descriptor,
        TRANSIENT_LEASE_MARKER,
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(DirectoryRetention::Transient),
        Err(error) => {
            return Err(io_error(
                "inspect transient lease marker",
                &directory.join(TRANSIENT_LEASE_MARKER),
                errno_to_io(error),
            ));
        }
    };
    if FileType::from_raw_mode(marker_stat.st_mode) != FileType::RegularFile
        || stat_link_count(&marker_stat) != Some(1)
    {
        return Ok(DirectoryRetention::ActiveLease);
    }

    match flock(&descriptor, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            let stable_marker = match statat(
                &descriptor,
                TRANSIENT_LEASE_MARKER,
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => {
                    return Err(io_error(
                        "inspect transient lease marker",
                        &directory.join(TRANSIENT_LEASE_MARKER),
                        io::Error::from(io::ErrorKind::NotFound),
                    ));
                }
                Err(_) => {
                    return Err(StoragePolicyError::EntryChanged {
                        path: directory.join(TRANSIENT_LEASE_MARKER),
                    });
                }
            };
            if !same_file_identity(&marker_stat, &stable_marker)
                || !same_file_state(&marker_stat, &stable_marker)
            {
                return Err(StoragePolicyError::EntryChanged {
                    path: directory.join(TRANSIENT_LEASE_MARKER),
                });
            }
            Ok(DirectoryRetention::AbandonedLease {
                marker_modified: stat_modified_seconds(&stable_marker),
            })
        }
        Err(error)
            if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::WOULDBLOCK =>
        {
            Ok(DirectoryRetention::ActiveLease)
        }
        Err(error) => Err(io_error(
            "inspect transient directory lease",
            directory,
            errno_to_io(error),
        )),
    }
}

#[cfg(not(unix))]
fn inspect_transient_lease(
    directory: &Path,
    _metadata: &fs::Metadata,
) -> Result<DirectoryRetention, StoragePolicyError> {
    match fs::symlink_metadata(directory.join(TRANSIENT_LEASE_MARKER)) {
        Ok(_) => Ok(DirectoryRetention::ActiveLease),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DirectoryRetention::Transient),
        Err(error) => Err(io_error(
            "inspect transient lease marker",
            &directory.join(TRANSIENT_LEASE_MARKER),
            error,
        )),
    }
}

fn classify(
    relative_path: &Path,
    retention: DirectoryRetention,
    modified_unix_seconds: u64,
    inventory_time: u64,
) -> ArtifactClassification {
    let mut components = relative_path.components();
    let in_transient = matches!(
        components.next(),
        Some(Component::Normal(name)) if name == TRANSIENT_DIRECTORY
    );
    if !in_transient || components.next().is_none() {
        return ArtifactClassification::Protected;
    }

    match retention {
        DirectoryRetention::Transient => ArtifactClassification::Reclaimable,
        DirectoryRetention::AbandonedLease { marker_modified }
            if old_enough(inventory_time, marker_modified)
                && old_enough(inventory_time, modified_unix_seconds) =>
        {
            ArtifactClassification::Reclaimable
        }
        DirectoryRetention::Protected
        | DirectoryRetention::ActiveLease
        | DirectoryRetention::AbandonedLease { .. } => ArtifactClassification::Protected,
    }
}

fn reclaimable_candidates(inventory: &StorageInventory) -> Vec<ArtifactInventoryEntry> {
    let mut candidates = inventory
        .entries
        .iter()
        .filter(|entry| {
            entry.classification == ArtifactClassification::Reclaimable
                && entry.kind == ArtifactEntryKind::RegularFile
                && !entry.unsafe_for_apply()
        })
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        is_lease_marker(&left.relative_path)
            .cmp(&is_lease_marker(&right.relative_path))
            .then_with(|| left.modified_unix_seconds.cmp(&right.modified_unix_seconds))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    candidates
}

fn is_lease_marker(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(TRANSIENT_LEASE_MARKER))
}

fn build_prune_plan(
    policy: &StoragePolicy,
    inventory: &StorageInventory,
) -> Result<PrunePlan, StoragePolicyError> {
    let candidates = reclaimable_candidates(inventory);
    let mut simulated = inventory.snapshot();
    let mut required_items = Vec::new();
    let mut required_reclaimed = StorageUsage::default();

    for candidate in &candidates {
        if simulated.limits.within_limits {
            break;
        }
        let item_usage = StorageUsage {
            bytes: candidate.bytes,
            files: 1,
        };
        simulated.usage = simulated.usage.checked_sub(item_usage)?;
        simulated.reclaimable = simulated.reclaimable.checked_sub(item_usage)?;
        simulated.available_bytes = simulated.available_bytes.saturating_add(candidate.bytes);
        simulated.limits =
            StorageLimitState::evaluate(policy, simulated.usage, simulated.available_bytes);
        required_reclaimed = required_reclaimed.checked_add(item_usage)?;
        required_items.push(PruneItem::from_entry(candidate));
    }

    let blocker = if inventory.unsafe_entries > 0 {
        Some(PruneBlocker::UnsafeEntries {
            count: inventory.unsafe_entries,
        })
    } else if !simulated.limits.within_limits {
        Some(PruneBlocker::InsufficientReclaimable {
            required_bytes: inventory
                .limits
                .bytes_over
                .max(inventory.limits.free_space_shortfall_bytes),
            required_files: inventory.limits.files_over,
            reclaimable_bytes: inventory.reclaimable.bytes,
            reclaimable_files: inventory.reclaimable.files,
        })
    } else if required_items.len() as u64 > policy.max_prune_items {
        Some(PruneBlocker::PruneItemLimit {
            required_items: required_items.len() as u64,
            max_prune_items: policy.max_prune_items,
        })
    } else {
        None
    };

    if required_items.len() as u64 > policy.max_prune_items {
        required_items.truncate(policy.max_prune_items as usize);
        let mut projected = inventory.snapshot();
        let mut reclaimed = StorageUsage::default();
        for item in &required_items {
            let item_usage = StorageUsage {
                bytes: item.bytes,
                files: 1,
            };
            projected.usage = projected.usage.checked_sub(item_usage)?;
            projected.reclaimable = projected.reclaimable.checked_sub(item_usage)?;
            projected.available_bytes = projected.available_bytes.saturating_add(item.bytes);
            reclaimed = reclaimed.checked_add(item_usage)?;
        }
        projected.limits =
            StorageLimitState::evaluate(policy, projected.usage, projected.available_bytes);
        return Ok(PrunePlan {
            projected,
            items: required_items,
            reclaimed,
            blocker,
        });
    }

    Ok(PrunePlan {
        projected: simulated,
        items: required_items,
        reclaimed: required_reclaimed,
        blocker,
    })
}

#[cfg(unix)]
struct OpenedDirectory {
    descriptor: OwnedFd,
    name_in_parent: Option<OsString>,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn delete_candidate_descriptor_relative(
    artifacts_root: &Path,
    root_device: Option<u64>,
    root_inode: Option<u64>,
    candidate: &ArtifactInventoryEntry,
) -> Result<(), StoragePolicyError> {
    delete_candidate_descriptor_relative_with_hook(
        artifacts_root,
        root_device,
        root_inode,
        candidate,
        || {},
    )
}

#[cfg(unix)]
fn delete_candidate_descriptor_relative_with_hook<F>(
    artifacts_root: &Path,
    root_device: Option<u64>,
    root_inode: Option<u64>,
    candidate: &ArtifactInventoryEntry,
    before_final_validation: F,
) -> Result<(), StoragePolicyError>
where
    F: FnOnce(),
{
    if candidate.classification != ArtifactClassification::Reclaimable
        || candidate.kind != ArtifactEntryKind::RegularFile
        || candidate.link_count != Some(1)
    {
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }

    let components = candidate
        .relative_path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(StoragePolicyError::EntryChanged {
                path: candidate.relative_path.clone(),
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() < 2 || components[0] != OsStr::new(TRANSIENT_DIRECTORY) {
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }

    let root_descriptor = open(
        artifacts_root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| StoragePolicyError::EntryChanged {
        path: PathBuf::from("."),
    })?;
    let root_stat = fstat(&root_descriptor).map_err(|_| StoragePolicyError::EntryChanged {
        path: PathBuf::from("."),
    })?;
    if FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory
        || stat_device(&root_stat) != root_device
        || stat_inode(&root_stat) != root_inode
    {
        return Err(StoragePolicyError::EntryChanged {
            path: PathBuf::from("."),
        });
    }
    let root_device = stat_device(&root_stat).ok_or_else(|| StoragePolicyError::EntryChanged {
        path: PathBuf::from("."),
    })?;
    let root_inode = stat_inode(&root_stat).ok_or_else(|| StoragePolicyError::EntryChanged {
        path: PathBuf::from("."),
    })?;

    let mut directories = vec![OpenedDirectory {
        descriptor: root_descriptor,
        name_in_parent: None,
        device: root_device,
        inode: root_inode,
    }];
    for component in &components[..components.len() - 1] {
        let parent = directories.last().expect("artifacts root is open");
        let descriptor = openat(
            &parent.descriptor,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
        let stat = fstat(&descriptor).map_err(|_| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(StoragePolicyError::EntryChanged {
                path: candidate.relative_path.clone(),
            });
        }
        let device = stat_device(&stat).ok_or_else(|| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
        let inode = stat_inode(&stat).ok_or_else(|| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
        directories.push(OpenedDirectory {
            descriptor,
            name_in_parent: Some(component.clone()),
            device,
            inode,
        });
    }

    let basename = components.last().expect("candidate has a basename");
    let parent = directories.last().expect("candidate parent is open");
    let candidate_descriptor = openat(
        &parent.descriptor,
        basename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| StoragePolicyError::EntryChanged {
        path: candidate.relative_path.clone(),
    })?;
    let opened_candidate_stat =
        fstat(&candidate_descriptor).map_err(|_| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
    if !stat_matches_candidate(&opened_candidate_stat, candidate) {
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }

    before_final_validation();
    verify_opened_directory_chain(artifacts_root, &directories)?;
    if directories.len() >= 3 {
        guard_abandoned_lease(&directories[2], &components[1], candidate)?;
    }

    let path_stat =
        statat(&parent.descriptor, basename, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| {
            StoragePolicyError::EntryChanged {
                path: candidate.relative_path.clone(),
            }
        })?;
    if !stat_matches_candidate(&path_stat, candidate)
        || !same_file_identity(&path_stat, &opened_candidate_stat)
    {
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }

    let quarantine_name = isolate_candidate(&parent.descriptor, basename, candidate)?;
    let isolated_stat = statat(
        &parent.descriptor,
        &quarantine_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|_| StoragePolicyError::EntryChanged {
        path: candidate.relative_path.clone(),
    })?;
    let isolated_descriptor_stat =
        fstat(&candidate_descriptor).map_err(|_| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
    if !stat_matches_isolated_candidate(&isolated_stat, candidate)
        || !same_file_identity(&isolated_stat, &opened_candidate_stat)
        || !same_file_state(&isolated_stat, &isolated_descriptor_stat)
    {
        restore_isolated_candidate(&parent.descriptor, &quarantine_name, basename);
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }

    let final_stat = statat(
        &parent.descriptor,
        &quarantine_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|_| StoragePolicyError::EntryChanged {
        path: candidate.relative_path.clone(),
    })?;
    let final_descriptor_stat =
        fstat(&candidate_descriptor).map_err(|_| StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        })?;
    if !stat_matches_isolated_candidate(&final_stat, candidate)
        || !same_file_identity(&final_stat, &opened_candidate_stat)
        || !same_file_state(&final_stat, &final_descriptor_stat)
    {
        restore_isolated_candidate(&parent.descriptor, &quarantine_name, basename);
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }
    if let Err(error) = unlinkat(&parent.descriptor, &quarantine_name, AtFlags::empty()) {
        restore_isolated_candidate(&parent.descriptor, &quarantine_name, basename);
        return Err(io_error(
            "delete isolated reclaimable artifact",
            &artifacts_root.join(&candidate.relative_path),
            errno_to_io(error),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_opened_directory_chain(
    artifacts_root: &Path,
    directories: &[OpenedDirectory],
) -> Result<(), StoragePolicyError> {
    let root = directories.first().expect("artifacts root is open");
    let root_stat =
        statat(rustix::fs::CWD, artifacts_root, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| {
            StoragePolicyError::EntryChanged {
                path: PathBuf::from("."),
            }
        })?;
    if FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory
        || stat_device(&root_stat) != Some(root.device)
        || stat_inode(&root_stat) != Some(root.inode)
    {
        return Err(StoragePolicyError::EntryChanged {
            path: PathBuf::from("."),
        });
    }

    for (index, directory) in directories.iter().enumerate().skip(1) {
        let parent = &directories[index - 1];
        let name = directory
            .name_in_parent
            .as_ref()
            .expect("non-root directory has a name");
        let path_stat =
            statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| {
                StoragePolicyError::EntryChanged {
                    path: PathBuf::from(name),
                }
            })?;
        let descriptor_stat =
            fstat(&directory.descriptor).map_err(|_| StoragePolicyError::EntryChanged {
                path: PathBuf::from(name),
            })?;
        if FileType::from_raw_mode(path_stat.st_mode) != FileType::Directory
            || stat_device(&path_stat) != Some(directory.device)
            || stat_inode(&path_stat) != Some(directory.inode)
            || !same_file_identity(&path_stat, &descriptor_stat)
        {
            return Err(StoragePolicyError::EntryChanged {
                path: PathBuf::from(name),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn guard_abandoned_lease(
    directory: &OpenedDirectory,
    lease_directory_name: &OsStr,
    candidate: &ArtifactInventoryEntry,
) -> Result<(), StoragePolicyError> {
    let marker_path = PathBuf::from(TRANSIENT_DIRECTORY)
        .join(lease_directory_name)
        .join(TRANSIENT_LEASE_MARKER);
    let marker_stat = match statat(
        &directory.descriptor,
        TRANSIENT_LEASE_MARKER,
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => {
            return Err(StoragePolicyError::EntryChanged { path: marker_path });
        }
    };
    if FileType::from_raw_mode(marker_stat.st_mode) != FileType::RegularFile
        || stat_link_count(&marker_stat) != Some(1)
    {
        return Err(StoragePolicyError::EntryChanged { path: marker_path });
    }

    match flock(
        &directory.descriptor,
        FlockOperation::NonBlockingLockExclusive,
    ) {
        Ok(()) => {}
        Err(error)
            if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::WOULDBLOCK =>
        {
            return Err(StoragePolicyError::EntryChanged {
                path: candidate.relative_path.clone(),
            });
        }
        Err(error) => {
            return Err(io_error(
                "acquire abandoned transient lease",
                &marker_path,
                errno_to_io(error),
            ));
        }
    }

    let stable_marker = statat(
        &directory.descriptor,
        TRANSIENT_LEASE_MARKER,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|_| StoragePolicyError::EntryChanged {
        path: marker_path.clone(),
    })?;
    if !same_file_identity(&marker_stat, &stable_marker)
        || !same_file_state(&marker_stat, &stable_marker)
    {
        return Err(StoragePolicyError::EntryChanged { path: marker_path });
    }

    let now = unix_seconds(SystemTime::now());
    if !old_enough(now, stat_modified_seconds(&stable_marker))
        || !old_enough(now, candidate.modified_unix_seconds)
    {
        return Err(StoragePolicyError::EntryChanged {
            path: candidate.relative_path.clone(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn isolate_candidate(
    parent: &OwnedFd,
    basename: &OsStr,
    candidate: &ArtifactInventoryEntry,
) -> Result<OsString, StoragePolicyError> {
    for _ in 0..8 {
        let quarantine_name = OsString::from(format!(
            ".hardknock-prune-{}",
            uuid::Uuid::new_v4().simple()
        ));
        match renameat_with(
            parent,
            basename,
            parent,
            &quarantine_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => return Ok(quarantine_name),
            Err(rustix::io::Errno::EXIST) => {}
            Err(error)
                if error == rustix::io::Errno::NOENT
                    || error == rustix::io::Errno::LOOP
                    || error == rustix::io::Errno::NOTDIR =>
            {
                return Err(StoragePolicyError::EntryChanged {
                    path: candidate.relative_path.clone(),
                });
            }
            Err(error) => {
                return Err(io_error(
                    "isolate reclaimable artifact",
                    &candidate.relative_path,
                    errno_to_io(error),
                ));
            }
        }
    }
    Err(StoragePolicyError::EntryChanged {
        path: candidate.relative_path.clone(),
    })
}

#[cfg(unix)]
fn restore_isolated_candidate(parent: &OwnedFd, quarantine_name: &OsStr, basename: &OsStr) {
    let _ = renameat_with(
        parent,
        quarantine_name,
        parent,
        basename,
        RenameFlags::NOREPLACE,
    );
}

#[cfg(unix)]
fn stat_matches_candidate(stat: &Stat, candidate: &ArtifactInventoryEntry) -> bool {
    stat_matches_isolated_candidate(stat, candidate)
        && Some(stat.st_ctime) == candidate.change_unix_seconds
        && Some(stat_time_nanoseconds(stat.st_ctime_nsec)) == candidate.change_nanoseconds
}

#[cfg(unix)]
fn stat_matches_isolated_candidate(stat: &Stat, candidate: &ArtifactInventoryEntry) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
        && stat_link_count(stat) == Some(1)
        && u64::try_from(stat.st_size).ok() == Some(candidate.bytes)
        && stat_device(stat) == candidate.device
        && stat_inode(stat) == candidate.inode
        && stat_modified_seconds(stat) == candidate.modified_unix_seconds
        && Some(stat_time_nanoseconds(stat.st_mtime_nsec)) == candidate.modified_nanoseconds
}

#[cfg(unix)]
fn same_file_identity(left: &Stat, right: &Stat) -> bool {
    stat_device(left) == stat_device(right) && stat_inode(left) == stat_inode(right)
}

#[cfg(unix)]
fn same_file_state(left: &Stat, right: &Stat) -> bool {
    same_file_identity(left, right)
        && left.st_mode == right.st_mode
        && stat_link_count(left) == stat_link_count(right)
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

#[cfg(unix)]
fn stat_modified_seconds(stat: &Stat) -> u64 {
    u64::try_from(stat.st_mtime).unwrap_or_default()
}

#[cfg(unix)]
fn stat_device(stat: &Stat) -> Option<u64> {
    u64::try_from(stat.st_dev).ok()
}

#[cfg(unix)]
fn stat_inode(stat: &Stat) -> Option<u64> {
    Some(stat.st_ino)
}

#[cfg(unix)]
#[allow(clippy::unnecessary_fallible_conversions, clippy::useless_conversion)]
fn stat_link_count(stat: &Stat) -> Option<u64> {
    u64::try_from(stat.st_nlink).ok()
}

#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn stat_time_nanoseconds<T>(value: T) -> i64
where
    T: Into<i64>,
{
    value.into()
}

fn modified_seconds(metadata: &fs::Metadata, path: &Path) -> Result<u64, StoragePolicyError> {
    let modified = metadata
        .modified()
        .map_err(|error| io_error("read artifact modification time", path, error))?;
    Ok(unix_seconds(modified))
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn old_enough(now: u64, modified: u64) -> bool {
    now.saturating_sub(modified) >= MIN_ABANDONED_LEASE_AGE.as_secs()
}

fn available_space(path: &Path) -> Result<u64, StoragePolicyError> {
    fs2::available_space(path)
        .map_err(|error| io_error("read available disk space for", path, error))
}

fn relative_or_full(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> StoragePolicyError {
    StoragePolicyError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{File, FileTimes},
        io::Write,
        thread,
        time::Duration,
    };
    use tempfile::tempdir;

    fn test_policy(max_bytes: u64, max_files: u64) -> StoragePolicy {
        StoragePolicy {
            max_bytes,
            max_files,
            min_free_bytes: 0,
            max_scan_entries: 1_000,
            max_prune_items: 100,
        }
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = File::create(path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn age_file(path: &Path) {
        let old = SystemTime::now()
            .checked_sub(MIN_ABANDONED_LEASE_AGE + Duration::from_secs(5))
            .unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
    }

    #[test]
    fn serde_defaults_are_conservative_and_validation_rejects_unsafe_limits() {
        let policy: StoragePolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, StoragePolicy::default());
        assert!(policy.validate().is_ok());
        assert!(policy.max_bytes > 0);
        assert!(policy.max_files > 0);
        assert!(policy.min_free_bytes > 0);

        let unknown = serde_json::from_str::<StoragePolicy>(r#"{"unknown":1}"#);
        assert!(unknown.is_err());

        let mut invalid = policy;
        invalid.max_bytes = 0;
        assert!(matches!(
            invalid.validate(),
            Err(StoragePolicyError::InvalidPolicy(_))
        ));
        invalid.max_bytes = 1;
        invalid.max_prune_items = invalid.max_scan_entries + 1;
        assert!(matches!(
            invalid.validate(),
            Err(StoragePolicyError::InvalidPolicy(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inventory_only_marks_transient_descendants_reclaimable_and_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let outside = directory.path().join("outside");
        write_file(&root.join("transient/cache.bin"), b"cache");
        write_file(&root.join("exp-1/evidence.bin"), b"evidence");
        write_file(&outside.join("secret.bin"), b"must not be scanned");
        symlink(&outside, root.join("transient/outside-link")).unwrap();

        let inventory = test_policy(1_000, 100).inventory(&root).unwrap();
        assert_eq!(
            inventory.usage,
            StorageUsage {
                bytes: 13,
                files: 2
            }
        );
        assert_eq!(inventory.reclaimable, StorageUsage { bytes: 5, files: 1 });
        assert_eq!(inventory.protected, StorageUsage { bytes: 8, files: 1 });
        assert_eq!(inventory.unsafe_entries, 1);
        assert!(
            !inventory
                .entries
                .iter()
                .any(|entry| entry.relative_path.ends_with("secret.bin"))
        );

        let link = inventory
            .entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("transient/outside-link"))
            .unwrap();
        assert_eq!(link.classification, ArtifactClassification::Reclaimable);
        assert_eq!(link.kind, ArtifactEntryKind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_and_apply_prune_oldest_reclaimable_file_only() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let old = root.join("transient/a-old.bin");
        let new = root.join("transient/z-new.bin");
        let protected = root.join("exp-1/evidence.bin");
        write_file(&old, b"old!");
        thread::sleep(Duration::from_millis(1_100));
        write_file(&new, b"new!");
        write_file(&protected, b"keep");

        let policy = test_policy(100, 2);
        let dry_run = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert_eq!(dry_run.mode, PruneMode::DryRun);
        assert_eq!(dry_run.items.len(), 1);
        assert_eq!(
            dry_run.items[0].relative_path,
            Path::new("transient/a-old.bin")
        );
        assert!(dry_run.blocker.is_none());
        assert!(old.exists());
        assert!(new.exists());
        assert!(protected.exists());

        let applied = policy.prune(&root, PruneMode::Apply).unwrap();
        assert_eq!(applied.items.len(), 1);
        assert_eq!(
            applied.items[0].relative_path,
            Path::new("transient/a-old.bin")
        );
        assert_eq!(applied.reclaimed, StorageUsage { bytes: 4, files: 1 });
        assert!(applied.limits_met);
        assert!(!old.exists());
        assert!(new.exists());
        assert!(protected.exists());
    }

    #[cfg(unix)]
    #[test]
    fn active_transient_lease_is_protected_even_when_files_look_abandoned() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        fs::create_dir_all(root.join(TRANSIENT_DIRECTORY)).unwrap();
        let leased = LeasedTransientDir::create(root.join(TRANSIENT_DIRECTORY)).unwrap();
        let candidate = leased.path().join("evaluation.out");
        write_file(&candidate, b"active");
        age_file(&candidate);
        age_file(&leased.path().join(TRANSIENT_LEASE_MARKER));

        let policy = test_policy(1, 1);
        let dry_run = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert!(dry_run.items.is_empty());
        assert!(matches!(
            dry_run.blocker,
            Some(PruneBlocker::InsufficientReclaimable { .. })
        ));
        assert!(matches!(
            policy.prune(&root, PruneMode::Apply),
            Err(StoragePolicyError::PruneBlocked(
                PruneBlocker::InsufficientReclaimable { .. }
            ))
        ));
        assert_eq!(fs::read(&candidate).unwrap(), b"active");
    }

    #[cfg(unix)]
    #[test]
    fn inventory_restarts_when_a_managed_transient_directory_disappears() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let transient_root = root.join(TRANSIENT_DIRECTORY);
        fs::create_dir_all(&transient_root).unwrap();
        let leased = LeasedTransientDir::create(&transient_root).unwrap();
        let disappearing = leased.path().to_path_buf();
        write_file(&disappearing.join("evaluation.out"), b"temporary");
        let protected = root.join("experience/evidence.bin");
        write_file(&protected, b"protected");

        let mut leased = Some(leased);
        let mut removals = 0;
        let inventory = inventory_artifacts_with_retry(
            &test_policy(u64::MAX, u64::MAX),
            &root,
            &mut |scanned, children| {
                if scanned == transient_root
                    && children.iter().any(|path| path == &disappearing)
                    && leased.is_some()
                {
                    drop(leased.take());
                    removals += 1;
                }
            },
        )
        .unwrap();

        assert_eq!(removals, 1);
        assert!(!disappearing.exists());
        assert_eq!(inventory.usage, StorageUsage { bytes: 9, files: 1 });
        assert_eq!(inventory.protected, inventory.usage);
        assert!(inventory.entries.iter().all(|entry| {
            !entry
                .relative_path
                .starts_with(disappearing.strip_prefix(&root).unwrap())
        }));
    }

    #[cfg(unix)]
    #[test]
    fn inventory_tolerates_multiple_managed_transient_teardowns() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let transient_root = root.join(TRANSIENT_DIRECTORY);
        fs::create_dir_all(&transient_root).unwrap();
        let mut leased = (0..4)
            .map(|_| LeasedTransientDir::create(&transient_root).unwrap())
            .collect::<Vec<_>>();
        for (index, directory) in leased.iter().enumerate() {
            write_file(
                &directory.path().join(format!("evaluation-{index}.out")),
                b"temporary",
            );
        }
        let protected = root.join("experience/evidence.bin");
        write_file(&protected, b"protected");

        let mut removals = 0;
        let inventory = inventory_artifacts_with_retry(
            &test_policy(u64::MAX, u64::MAX),
            &root,
            &mut |scanned, _| {
                if scanned != transient_root {
                    return;
                }
                let Some(directory) = leased.pop() else {
                    return;
                };
                drop(directory);
                removals += 1;
            },
        )
        .unwrap();

        assert_eq!(removals, 4);
        assert!(leased.is_empty());
        assert_eq!(inventory.usage, StorageUsage { bytes: 9, files: 1 });
        assert_eq!(inventory.protected, inventory.usage);
    }

    #[test]
    fn inventory_retry_is_limited_to_not_found_under_managed_transients() {
        let root = Path::new("/tmp/hardknock-artifacts");
        let managed = root.join("transient/hk-transient-test/evaluation.out");
        let unleased = root.join("transient/user-cache/evaluation.out");
        let protected = root.join("experience/evidence.bin");

        let not_found = |path: &Path| {
            io_error(
                "inspect artifact entry",
                path,
                io::Error::from(io::ErrorKind::NotFound),
            )
        };
        assert!(retryable_transient_disappearance(
            root,
            &not_found(&managed)
        ));
        assert!(!retryable_transient_disappearance(
            root,
            &not_found(&unleased)
        ));
        assert!(!retryable_transient_disappearance(
            root,
            &not_found(&protected)
        ));
        assert!(!retryable_transient_disappearance(
            root,
            &io_error(
                "inspect artifact entry",
                &managed,
                io::Error::from(io::ErrorKind::PermissionDenied),
            )
        ));
        assert!(!retryable_transient_disappearance(
            root,
            &StoragePolicyError::EntryChanged { path: managed }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn abandoned_transient_lease_becomes_reclaimable_after_grace_period() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        fs::create_dir_all(root.join(TRANSIENT_DIRECTORY)).unwrap();
        let leased = LeasedTransientDir::create(root.join(TRANSIENT_DIRECTORY)).unwrap();
        let candidate = leased.path().join("evaluation.out");
        let protected = root.join("experience/evidence.bin");
        write_file(&candidate, b"abandoned");
        write_file(&protected, b"protected");
        let abandoned = leased.abandon_for_test();

        let policy = test_policy(u64::MAX, 2);
        let fresh = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert!(fresh.items.is_empty());
        assert!(matches!(
            fresh.blocker,
            Some(PruneBlocker::InsufficientReclaimable { .. })
        ));

        age_file(&candidate);
        age_file(&abandoned.join(TRANSIENT_LEASE_MARKER));
        let dry_run = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert_eq!(dry_run.items.len(), 1);
        assert_eq!(
            dry_run.items[0].relative_path,
            candidate.strip_prefix(&root).unwrap()
        );

        let applied = policy.prune(&root, PruneMode::Apply).unwrap();
        assert!(applied.limits_met);
        assert!(!candidate.exists());
        assert_eq!(fs::read(&protected).unwrap(), b"protected");
        assert!(abandoned.join(TRANSIENT_LEASE_MARKER).exists());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_deletion_rejects_ancestor_symlink_swap_without_touching_outside_file() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let candidate_path = root.join("transient/session/candidate.bin");
        let moved_session = root.join("transient/session-original");
        let outside = directory.path().join("outside");
        let outside_file = outside.join("candidate.bin");
        write_file(&candidate_path, b"candidate");
        write_file(&outside_file, b"outside");

        let inventory = test_policy(1, 1).inventory(&root).unwrap();
        let candidate = inventory
            .entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("transient/session/candidate.bin"))
            .unwrap()
            .clone();
        let error = delete_candidate_descriptor_relative_with_hook(
            &root,
            inventory.root_device,
            inventory.root_inode,
            &candidate,
            || {
                fs::rename(root.join("transient/session"), &moved_session).unwrap();
                symlink(&outside, root.join("transient/session")).unwrap();
            },
        )
        .unwrap_err();

        assert!(matches!(error, StoragePolicyError::EntryChanged { .. }));
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
        assert_eq!(
            fs::read(moved_session.join("candidate.bin")).unwrap(),
            b"candidate"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_deletion_rejects_candidate_identity_swap_without_following_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let candidate_path = root.join("transient/candidate.bin");
        let original_path = root.join("transient/candidate-original.bin");
        let protected = root.join("experience/evidence.bin");
        write_file(&candidate_path, b"candidate");
        write_file(&protected, b"protected");

        let inventory = test_policy(1, 1).inventory(&root).unwrap();
        let candidate = inventory
            .entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("transient/candidate.bin"))
            .unwrap()
            .clone();
        let error = delete_candidate_descriptor_relative_with_hook(
            &root,
            inventory.root_device,
            inventory.root_inode,
            &candidate,
            || {
                fs::rename(&candidate_path, &original_path).unwrap();
                symlink(&protected, &candidate_path).unwrap();
            },
        )
        .unwrap_err();

        assert!(matches!(error, StoragePolicyError::EntryChanged { .. }));
        assert_eq!(fs::read(&protected).unwrap(), b"protected");
        assert_eq!(fs::read(&original_path).unwrap(), b"candidate");
    }

    #[cfg(unix)]
    #[test]
    fn protected_evidence_is_never_planned_or_deleted() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let protected = root.join("exp-1/evidence.bin");
        write_file(&protected, b"protected");

        let policy = test_policy(1, 100);
        let dry_run = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert!(dry_run.items.is_empty());
        assert!(matches!(
            dry_run.blocker,
            Some(PruneBlocker::InsufficientReclaimable { .. })
        ));

        let error = policy.prune(&root, PruneMode::Apply).unwrap_err();
        assert!(matches!(
            error,
            StoragePolicyError::PruneBlocked(PruneBlocker::InsufficientReclaimable { .. })
        ));
        assert_eq!(fs::read(&protected).unwrap(), b"protected");
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_symlink_before_deleting_reclaimable_files() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let candidate = root.join("transient/candidate.bin");
        let protected = root.join("exp-1/evidence.bin");
        write_file(&candidate, b"candidate");
        write_file(&protected, b"protected");
        symlink(&protected, root.join("transient/evidence-link")).unwrap();

        let error = test_policy(1, 1)
            .prune(&root, PruneMode::Apply)
            .unwrap_err();
        assert!(matches!(
            error,
            StoragePolicyError::PruneBlocked(PruneBlocker::UnsafeEntries { count: 1 })
        ));
        assert!(candidate.exists());
        assert_eq!(fs::read(&protected).unwrap(), b"protected");
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_hardlinks_and_non_regular_entries() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let original = root.join("transient/original.bin");
        let linked = root.join("transient/linked.bin");
        let fifo = root.join("transient/runtime.pipe");
        write_file(&original, b"linked");
        fs::hard_link(&original, &linked).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );

        let inventory = test_policy(1, 1).inventory(&root).unwrap();
        assert_eq!(inventory.unsafe_entries, 3);
        let error = test_policy(1, 1)
            .prune(&root, PruneMode::Apply)
            .unwrap_err();
        assert!(matches!(
            error,
            StoragePolicyError::PruneBlocked(PruneBlocker::UnsafeEntries { count: 3 })
        ));
        assert!(original.exists());
        assert!(linked.exists());
        assert!(fifo.exists());
    }

    #[test]
    fn inventory_reports_minimum_free_space_shortfall() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        fs::create_dir_all(&root).unwrap();
        let mut policy = test_policy(u64::MAX, u64::MAX);
        policy.min_free_bytes = u64::MAX;

        let inventory = policy.inventory(&root).unwrap();
        assert!(inventory.limits.below_min_free_space);
        assert!(inventory.limits.free_space_shortfall_bytes > 0);
        assert!(!inventory.limits.within_limits);
    }

    #[test]
    fn inventory_enforces_the_bounded_scan_limit() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        write_file(&root.join("one.bin"), b"1");
        write_file(&root.join("two.bin"), b"2");
        let mut policy = test_policy(100, 100);
        policy.max_scan_entries = 1;
        policy.max_prune_items = 1;

        assert!(matches!(
            policy.inventory(&root),
            Err(StoragePolicyError::ScanLimitExceeded { limit: 1 })
        ));
    }

    #[test]
    fn dry_run_prunes_reclaimable_files_to_meet_byte_quota() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let candidate = root.join("transient/cache.bin");
        let protected = root.join("exp-1/evidence.bin");
        write_file(&candidate, b"12345678");
        write_file(&protected, b"keep");

        let report = test_policy(8, 100).prune(&root, PruneMode::DryRun).unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].relative_path,
            Path::new("transient/cache.bin")
        );
        assert_eq!(report.projected.usage.bytes, 4);
        assert!(report.projected.limits.within_limits);
        assert!(candidate.exists());
        assert!(protected.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_item_limit_blocks_apply_before_partial_deletion() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let first = root.join("transient/a.bin");
        let second = root.join("transient/b.bin");
        let third = root.join("transient/c.bin");
        write_file(&first, b"a");
        write_file(&second, b"b");
        write_file(&third, b"c");
        let mut policy = test_policy(100, 1);
        policy.max_prune_items = 1;

        let dry_run = policy.prune(&root, PruneMode::DryRun).unwrap();
        assert!(matches!(
            dry_run.blocker,
            Some(PruneBlocker::PruneItemLimit {
                required_items: 2,
                max_prune_items: 1
            })
        ));
        assert_eq!(dry_run.items.len(), 1);

        assert!(matches!(
            policy.prune(&root, PruneMode::Apply),
            Err(StoragePolicyError::PruneBlocked(
                PruneBlocker::PruneItemLimit {
                    required_items: 2,
                    max_prune_items: 1
                }
            ))
        ));
        assert!(first.exists());
        assert!(second.exists());
        assert!(third.exists());
    }

    #[test]
    fn ensure_capacity_returns_bounded_actionable_error_without_pruning() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("artifacts");
        let protected = root.join("exp-1/evidence.bin");
        write_file(&protected, b"keep");

        let policy = test_policy(4, 1);
        let error = policy.ensure_capacity(&root, 1, 1).unwrap_err();
        let StoragePolicyError::InsufficientCapacity(capacity) = error else {
            panic!("unexpected error");
        };
        let message = capacity.to_string();
        assert!(message.len() < 512);
        assert!(message.contains("retention dry-run"));
        assert!(message.contains("explicit apply"));
        assert_eq!(capacity.requested, StorageUsage { bytes: 1, files: 1 });
        assert_eq!(fs::read(&protected).unwrap(), b"keep");
    }
}
