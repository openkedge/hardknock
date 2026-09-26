// SPDX-License-Identifier: Apache-2.0

use crate::{Error, Result};
use chrono::Utc;
use fs2::FileExt;
use nix::unistd::geteuid;
use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, Stat, fchmod, fstat, fsync, mkdirat,
    open, openat, renameat_with, statat, unlinkat,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::fd::AsFd,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const MAX_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SNAPSHOT_FILES: usize = 64;
const MAX_FINGERPRINT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECOVERY_STATE_BYTES: u64 = 1024 * 1024;
const MAX_DIRECTORY_SNAPSHOTS: usize = 512;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const SETUP_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_SCHEMA: &str = "hardknock-setup-journal-v1";
const RECOVERY_SCHEMA: &str = "hardknock-setup-recovery-v1";
const RECOVERY_STATE_FILE: &str = "state.json";
const RECOVERY_BLOBS_DIRECTORY: &str = "previous";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum InitialHome {
    Missing,
    Empty { mode: u32, identity: FileIdentity },
    Existing,
}

#[derive(Debug)]
enum PreviousFile {
    Missing,
    Present { bytes: Vec<u8>, mode: u32 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum FileState {
    Missing,
    Present {
        dev: u64,
        ino: u64,
        uid: u32,
        nlink: u64,
        mode: u32,
        len: u64,
        mtime: i64,
        mtime_nsec: i64,
        ctime: i64,
        ctime_nsec: i64,
        content_blake3: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ownership", rename_all = "snake_case")]
enum ExpectedState {
    Exact {
        state: FileState,
        #[serde(default)]
        pending_cleanup: Option<PrivateCleanup>,
    },
    PlannedWrite {
        before: FileState,
        staged: PrivateEntry,
        len: u64,
        content_blake3: String,
    },
    PlannedRemoval {
        before: FileState,
        captured: PrivateEntry,
    },
    PlannedCreation {
        before: FileState,
    },
    MutableCreation {
        before: FileState,
        identity: Option<FileIdentity>,
        staging_path: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuarantineRecovery {
    Restore,
    ResumeDeletion,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuarantineState {
    Planned,
    Applied,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct QuarantineAction {
    pub original: PathBuf,
    pub quarantine: PathBuf,
    pub recovery: QuarantineRecovery,
    pub state: QuarantineState,
    identity: FileIdentity,
    #[serde(default)]
    deletion: Option<QuarantineDeletion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct QuarantineDeletion {
    namespace: PrivateNamespace,
    root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum RecoveryPhase {
    Active,
    Finalizing {
        outcome: String,
        journal_destination: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FileIdentity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) uid: u32,
    pub(crate) mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PrivateNamespace {
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PrivateEntry {
    namespace: PrivateNamespace,
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PrivateCleanup {
    namespace: PrivateNamespace,
    entry: Option<PrivateEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedWrite {
    path: PathBuf,
    before: FileState,
    staged: PrivateEntry,
    len: u64,
    content_blake3: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedRemoval {
    path: PathBuf,
    before: FileState,
    captured: PrivateEntry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryReceipt {
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectorySnapshot {
    path: PathBuf,
    identity: Option<FileIdentity>,
    owns_contents: bool,
    entries: Vec<OwnedDirectoryEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnedDirectoryEntry {
    path: PathBuf,
    identity: FileIdentity,
}

#[derive(Debug)]
struct FileSnapshot {
    path: PathBuf,
    previous: PreviousFile,
    expected: ExpectedState,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotDescription {
    pub path: PathBuf,
    pub previous: &'static str,
}

#[derive(Debug)]
pub(crate) struct SnapshotSet {
    files: Vec<FileSnapshot>,
    directories: Vec<DirectorySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WriteReceipt {
    path: PathBuf,
    state: FileState,
    pending_cleanup: Option<PrivateCleanup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecoveryState {
    schema: String,
    transaction_id: String,
    operation: String,
    home: PathBuf,
    initial_home: InitialHome,
    journal_path: PathBuf,
    journal_identity: FileIdentity,
    phase: RecoveryPhase,
    quarantine: Option<QuarantineAction>,
    files: Vec<RecoveryFile>,
    directories: Vec<RecoveryDirectory>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecoveryFile {
    path: PathBuf,
    previous: RecoveryPrevious,
    expected: ExpectedState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecoveryDirectory {
    path: PathBuf,
    identity: Option<FileIdentity>,
    #[serde(default)]
    owns_contents: bool,
    #[serde(default)]
    entries: Vec<OwnedDirectoryEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RecoveryPrevious {
    Missing,
    Present {
        blob: String,
        mode: u32,
        len: u64,
        content_blake3: String,
    },
}

pub(crate) struct UnfinishedTransaction {
    operation: String,
    initial_home: InitialHome,
    snapshots: SnapshotSet,
    recovery_path: PathBuf,
    journal_path: PathBuf,
    journal_identity: FileIdentity,
    quarantine: Option<QuarantineAction>,
    transaction_id: String,
}

#[derive(Debug)]
pub(crate) struct SetupLock {
    directory: File,
}

pub(crate) trait MutationObserver {
    fn prepare_write(&mut self, intent: PreparedWrite) -> Result<()>;
    fn prepare_removal(&mut self, intent: PreparedRemoval) -> Result<()>;
    fn record_write(&mut self, receipt: WriteReceipt) -> Result<()>;
    fn record_removal(&mut self, receipt: WriteReceipt) -> Result<()>;
    fn abort_mutation(&mut self, path: &Path, cleanup: PrivateCleanup) -> Result<()>;
    fn record_directory(&mut self, receipt: DirectoryReceipt) -> Result<()>;
}

pub(crate) struct UnobservedMutation;

impl MutationObserver for UnobservedMutation {
    fn prepare_write(&mut self, _intent: PreparedWrite) -> Result<()> {
        Ok(())
    }

    fn prepare_removal(&mut self, _intent: PreparedRemoval) -> Result<()> {
        Ok(())
    }

    fn record_write(&mut self, receipt: WriteReceipt) -> Result<()> {
        receipt.complete_unobserved()
    }

    fn record_removal(&mut self, receipt: WriteReceipt) -> Result<()> {
        receipt.complete_unobserved()
    }

    fn abort_mutation(&mut self, _path: &Path, cleanup: PrivateCleanup) -> Result<()> {
        cleanup_private_namespace(&cleanup)
    }

    fn record_directory(&mut self, _receipt: DirectoryReceipt) -> Result<()> {
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Intervention(message.into())
}

fn errno_error(error: rustix::io::Errno) -> Error {
    Error::Io(std::io::Error::from(error))
}

fn rename_no_replace(source: &Path, destination: &Path) -> Result<()> {
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(errno_error)
}

fn same_stat_identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn metadata_matches_stat(metadata: &std::fs::Metadata, stat: &Stat) -> bool {
    metadata.dev() == stat.st_dev as u64 && metadata.ino() == stat.st_ino
}

struct BoundDirectory {
    descriptor: File,
    path: PathBuf,
    identity: FileIdentity,
}

impl BoundDirectory {
    fn open(path: &Path) -> Result<Self> {
        let before = fs::symlink_metadata(path)?;
        if before.file_type().is_symlink() || !before.is_dir() || before.uid() != geteuid().as_raw()
        {
            return Err(invalid(format!(
                "Managed parent is unsafe: {}",
                path.display()
            )));
        }
        let descriptor = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(path)?;
        let opened = descriptor.metadata()?;
        let current = fs::symlink_metadata(path)?;
        let identity = file_identity(&before);
        if !same_identity(&opened, &identity) || !same_identity(&current, &identity) {
            return Err(invalid(format!(
                "Managed parent changed while it was opened: {}",
                path.display()
            )));
        }
        Ok(Self {
            descriptor,
            path: path.to_path_buf(),
            identity,
        })
    }

    fn verify(&self) -> Result<()> {
        let opened = self.descriptor.metadata()?;
        let current = fs::symlink_metadata(&self.path)?;
        if !same_identity(&opened, &self.identity) || !same_identity(&current, &self.identity) {
            return Err(invalid(format!(
                "Managed parent changed during mutation: {}",
                self.path.display()
            )));
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        self.descriptor.sync_all()?;
        Ok(())
    }
}

fn validate_open_directory(
    descriptor: &File,
    path: &Path,
    require_private: bool,
    allow_trusted_ancestor: bool,
) -> Result<FileIdentity> {
    let opened = descriptor.metadata()?;
    let named = fs::symlink_metadata(path)?;
    let effective_uid = geteuid().as_raw();
    let trusted_owner =
        opened.uid() == effective_uid || (allow_trusted_ancestor && opened.uid() == 0);
    let safe_mode = if require_private {
        opened.mode() & 0o7777 == 0o700
    } else {
        opened.mode() & 0o002 == 0 || opened.mode() & 0o1000 != 0
    };
    if opened.file_type().is_symlink()
        || !opened.is_dir()
        || !trusted_owner
        || !safe_mode
        || !same_file_state(&opened, &named)
    {
        return Err(invalid(format!(
            "Managed directory is unsafe or changed while it was opened: {}",
            path.display()
        )));
    }
    Ok(file_identity(&opened))
}

pub(crate) fn ensure_directory_tree(
    path: &Path,
    require_private: bool,
    observer: &mut impl MutationObserver,
) -> Result<()> {
    ensure_directory_tree_with_hook(path, require_private, observer, |_| {})
}

fn ensure_directory_tree_with_hook(
    path: &Path,
    require_private: bool,
    observer: &mut impl MutationObserver,
    mut after_create: impl FnMut(&Path),
) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid(format!(
            "Managed directory must be absolute: {}",
            path.display()
        )));
    }

    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(invalid(format!(
                        "Managed directory ancestor is unsafe: {}",
                        ancestor.display()
                    )));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.to_path_buf());
                ancestor = ancestor.parent().ok_or_else(|| {
                    invalid(format!(
                        "Managed directory has no existing ancestor: {}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }

    let mut descriptor = File::from(
        open(
            ancestor,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?,
    );
    validate_open_directory(
        &descriptor,
        ancestor,
        missing.is_empty() && require_private,
        !missing.is_empty(),
    )?;

    for component_path in missing.into_iter().rev() {
        let name = component_path.file_name().ok_or_else(|| {
            invalid(format!(
                "Managed directory component has no name: {}",
                component_path.display()
            ))
        })?;
        match mkdirat(&descriptor, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {
                return Err(invalid(format!(
                    "Managed directory appeared during descriptor-bound creation: {}",
                    component_path.display()
                )));
            }
            Err(error) => return Err(errno_error(error)),
        }
        after_create(&component_path);
        let child = File::from(
            openat(
                &descriptor,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(errno_error)?,
        );
        fchmod(&child, Mode::from_raw_mode(0o700)).map_err(errno_error)?;
        fsync(&child).map_err(errno_error)?;
        fsync(&descriptor).map_err(errno_error)?;
        let opened = fstat(&child).map_err(errno_error)?;
        let named = statat(&descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
            || !same_stat_identity(&opened, &named)
            || opened.st_uid != geteuid().as_raw()
            || opened.st_mode & 0o7777 != 0o700
        {
            return Err(invalid(format!(
                "Managed directory changed during descriptor-bound creation: {}",
                component_path.display()
            )));
        }
        let identity = FileIdentity {
            dev: opened.st_dev as u64,
            ino: opened.st_ino,
            uid: opened.st_uid,
            mode: opened.st_mode as u32,
        };
        observer.record_directory(DirectoryReceipt {
            path: component_path,
            identity,
        })?;
        descriptor = child;
    }

    validate_open_directory(&descriptor, path, require_private, false)?;
    Ok(())
}

pub(crate) struct PrivateMutationDirectory {
    descriptor: File,
    parent_path: PathBuf,
    parent_identity: FileIdentity,
    name: OsString,
    namespace: PrivateNamespace,
}

impl PrivateMutationDirectory {
    pub(crate) fn create<Fd: AsFd>(parent: &Fd, parent_path: &Path, prefix: &str) -> Result<Self> {
        let parent_stat = fstat(parent).map_err(errno_error)?;
        let parent_named =
            statat(CWD, parent_path, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if FileType::from_raw_mode(parent_stat.st_mode) != FileType::Directory
            || !same_stat_identity(&parent_stat, &parent_named)
            || parent_stat.st_uid != geteuid().as_raw()
        {
            return Err(invalid(format!(
                "Managed mutation parent changed before quarantine creation: {}",
                parent_path.display()
            )));
        }
        let parent_identity = FileIdentity {
            dev: parent_stat.st_dev as u64,
            ino: parent_stat.st_ino,
            uid: parent_stat.st_uid,
            mode: parent_stat.st_mode as u32,
        };

        for _ in 0..16 {
            let name = OsString::from(format!(
                ".hardknock-{prefix}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            match mkdirat(parent, &name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let descriptor = File::from(
                        openat(
                            parent,
                            &name,
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(errno_error)?,
                    );
                    fchmod(&descriptor, Mode::from_raw_mode(0o700)).map_err(errno_error)?;
                    fsync(&descriptor).map_err(errno_error)?;
                    fsync(parent).map_err(errno_error)?;
                    let opened = descriptor.metadata()?;
                    let named =
                        statat(parent, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
                    if !opened.is_dir()
                        || opened.uid() != geteuid().as_raw()
                        || opened.mode() & 0o7777 != 0o700
                        || !metadata_matches_stat(&opened, &named)
                    {
                        return Err(invalid(format!(
                            "Private mutation namespace changed during creation: {}",
                            parent_path.join(&name).display()
                        )));
                    }
                    let namespace = PrivateNamespace {
                        path: parent_path.join(&name),
                        identity: file_identity(&opened),
                    };
                    return Ok(Self {
                        descriptor,
                        parent_path: parent_path.to_path_buf(),
                        parent_identity,
                        name,
                        namespace,
                    });
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(errno_error(error)),
            }
        }
        Err(invalid(
            "Could not allocate a private mutation quarantine namespace",
        ))
    }

    pub(crate) fn descriptor(&self) -> &File {
        &self.descriptor
    }

    pub(crate) fn entry_path(&self) -> PathBuf {
        self.namespace.path.join("entry")
    }

    pub(crate) fn verify_parent<Fd: AsFd>(&self, parent: &Fd) -> Result<()> {
        let opened = fstat(parent).map_err(errno_error)?;
        let named =
            statat(CWD, &self.parent_path, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if opened.st_dev as u64 != self.parent_identity.dev
            || opened.st_ino != self.parent_identity.ino
            || opened.st_uid != self.parent_identity.uid
            || !same_stat_identity(&opened, &named)
        {
            return Err(invalid(format!(
                "Managed mutation parent changed while its private namespace was active: {}",
                self.parent_path.display()
            )));
        }
        let namespace =
            statat(parent, &self.name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        let opened_namespace = fstat(&self.descriptor).map_err(errno_error)?;
        if !same_stat_identity(&namespace, &opened_namespace)
            || opened_namespace.st_dev as u64 != self.namespace.identity.dev
            || opened_namespace.st_ino != self.namespace.identity.ino
            || opened_namespace.st_uid != self.namespace.identity.uid
            || opened_namespace.st_mode as u32 != self.namespace.identity.mode
        {
            return Err(invalid(format!(
                "Private mutation namespace changed while it was active: {}",
                self.namespace.path.display()
            )));
        }
        Ok(())
    }

    pub(crate) fn create_file(&self, bytes: &[u8], mode: u32) -> Result<File> {
        let descriptor = openat(
            &self.descriptor,
            "entry",
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode as _),
        )
        .map_err(errno_error)?;
        let mut file = File::from(descriptor);
        fchmod(&file, Mode::from_raw_mode(mode as _)).map_err(errno_error)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fsync(&self.descriptor).map_err(errno_error)?;
        let opened = fstat(&file).map_err(errno_error)?;
        let named =
            statat(&self.descriptor, "entry", AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
            || !same_stat_identity(&opened, &named)
            || opened.st_uid != geteuid().as_raw()
            || opened.st_nlink != 1
            || opened.st_mode as u32 & 0o7777 != mode
        {
            return Err(invalid(format!(
                "Private mutation staging file changed during creation: {}",
                self.entry_path().display()
            )));
        }
        Ok(file)
    }

    fn entry_for_file(&self, file: &File) -> Result<PrivateEntry> {
        let metadata = file.metadata()?;
        let named =
            statat(&self.descriptor, "entry", AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if !metadata.is_file()
            || metadata.uid() != geteuid().as_raw()
            || metadata.nlink() != 1
            || !metadata_matches_stat(&metadata, &named)
        {
            return Err(invalid(format!(
                "Private mutation entry changed before ownership was recorded: {}",
                self.entry_path().display()
            )));
        }
        Ok(PrivateEntry {
            namespace: self.namespace.clone(),
            path: self.entry_path(),
            identity: file_identity(&metadata),
        })
    }

    pub(crate) fn staged_write(
        &self,
        path: &Path,
        before: Option<(&File, &[u8])>,
        staged: &File,
        bytes: &[u8],
    ) -> Result<PreparedWrite> {
        let before = match before {
            Some((file, bytes)) => {
                state_from_open_file(file, Some(blake3::hash(bytes).to_hex().to_string()))?
            }
            None => FileState::Missing,
        };
        Ok(PreparedWrite {
            path: path.to_path_buf(),
            before,
            staged: self.entry_for_file(staged)?,
            len: bytes.len() as u64,
            content_blake3: blake3::hash(bytes).to_hex().to_string(),
        })
    }

    pub(crate) fn planned_removal(
        &self,
        path: &Path,
        current: &File,
        bytes: &[u8],
    ) -> Result<PreparedRemoval> {
        let before = state_from_open_file(current, Some(blake3::hash(bytes).to_hex().to_string()))?;
        let metadata = current.metadata()?;
        Ok(PreparedRemoval {
            path: path.to_path_buf(),
            before,
            captured: PrivateEntry {
                namespace: self.namespace.clone(),
                path: self.entry_path(),
                identity: file_identity(&metadata),
            },
        })
    }

    pub(crate) fn cleanup_for_entry(&self, file: &File) -> Result<PrivateCleanup> {
        Ok(PrivateCleanup {
            namespace: self.namespace.clone(),
            entry: Some(self.entry_for_file(file)?),
        })
    }

    pub(crate) fn cleanup_empty(&self) -> PrivateCleanup {
        PrivateCleanup {
            namespace: self.namespace.clone(),
            entry: None,
        }
    }
}

fn path_parts(path: &Path) -> Result<(&Path, &OsStr)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid(format!("Managed path has no parent: {}", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid(format!("Managed path has no name: {}", path.display())))?;
    if name == OsStr::new(".") || name == OsStr::new("..") {
        return Err(invalid(format!(
            "Managed path has an unsafe name: {}",
            path.display()
        )));
    }
    Ok((parent, name))
}

fn inspect_bound_regular(
    parent: &BoundDirectory,
    name: &OsStr,
    display_path: &Path,
) -> Result<Option<FileState>> {
    let descriptor = match openat(
        &parent.descriptor,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(errno_error(error)),
    };
    let mut file = File::from(descriptor);
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != geteuid().as_raw()
        || before.nlink() != 1
        || before.mode() & 0o022 != 0
    {
        return Err(invalid(format!(
            "Managed rollback path is not a safe regular file: {}",
            display_path.display()
        )));
    }
    let named = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !metadata_matches_stat(&before, &named) {
        return Err(invalid(format!(
            "Managed rollback path changed while it was opened: {}",
            display_path.display()
        )));
    }
    if before.len() > MAX_FINGERPRINT_BYTES {
        return Err(invalid(format!(
            "Managed rollback path exceeds its inspection limit: {}",
            display_path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total_read = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total_read = total_read.saturating_add(read as u64);
        if total_read > MAX_FINGERPRINT_BYTES {
            return Err(invalid(format!(
                "Managed rollback path grew beyond its inspection limit: {}",
                display_path.display()
            )));
        }
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    let named_after =
        statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !same_file_state(&before, &after) || !metadata_matches_stat(&after, &named_after) {
        return Err(invalid(format!(
            "Managed rollback path changed while it was inspected: {}",
            display_path.display()
        )));
    }
    Ok(Some(state_from_metadata(
        &after,
        Some(hasher.finalize().to_hex().to_string()),
    )))
}

fn transaction_scratch_name(transaction_id: &str, path: &Path, operation: &str) -> OsString {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_os_str().as_bytes());
    hasher.update(operation.as_bytes());
    let digest = hasher.finalize().to_hex();
    OsString::from(format!(
        ".hardknock-rollback-{transaction_id}-{}-{operation}",
        &digest[..16]
    ))
}

fn scratch_delete_name(name: &OsStr) -> OsString {
    let mut value = name.as_bytes().to_vec();
    value.extend_from_slice(b".quarantine");
    OsString::from_vec(value)
}

fn final_delete_name(prefix: &str) -> OsString {
    OsString::from(format!(
        ".hardknock-{prefix}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn restore_captured_name(
    parent: &BoundDirectory,
    captured_name: &OsStr,
    target_name: &OsStr,
    display_path: &Path,
) -> Result<()> {
    match renameat_with(
        &parent.descriptor,
        captured_name,
        &parent.descriptor,
        target_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            parent.sync()?;
            Ok(())
        }
        Err(error) => Err(invalid(format!(
            "Managed rollback path changed during compensation; both paths were preserved ({}; {}): {error}",
            display_path.display(),
            parent.path.join(captured_name).display()
        ))),
    }
}

fn delete_transaction_scratch(
    parent: &BoundDirectory,
    scratch_name: &OsStr,
    display_path: &Path,
    matches: &impl Fn(&FileState) -> bool,
) -> Result<()> {
    delete_transaction_scratch_with_hook(parent, scratch_name, display_path, matches, || {})
}

fn delete_transaction_scratch_with_hook(
    parent: &BoundDirectory,
    scratch_name: &OsStr,
    display_path: &Path,
    matches: &impl Fn(&FileState) -> bool,
    before_final_capture: impl FnOnce(),
) -> Result<()> {
    let namespace_name = scratch_delete_name(scratch_name);
    let namespace_path = parent.path.join(&namespace_name);
    let namespace_descriptor = match openat(
        &parent.descriptor,
        &namespace_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(rustix::io::Errno::NOENT) => {
            mkdirat(
                &parent.descriptor,
                &namespace_name,
                Mode::from_raw_mode(0o700),
            )
            .map_err(errno_error)?;
            let descriptor = File::from(
                openat(
                    &parent.descriptor,
                    &namespace_name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(errno_error)?,
            );
            fchmod(&descriptor, Mode::from_raw_mode(0o700)).map_err(errno_error)?;
            fsync(&descriptor).map_err(errno_error)?;
            parent.sync()?;
            descriptor
        }
        Err(error) => {
            return Err(invalid(format!(
                "Managed rollback cleanup namespace could not be safely reopened; ambiguous data was preserved: {} ({error})",
                namespace_path.display()
            )));
        }
    };
    let namespace_metadata = namespace_descriptor.metadata()?;
    let namespace_named = statat(
        &parent.descriptor,
        &namespace_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if !namespace_metadata.is_dir()
        || namespace_metadata.uid() != geteuid().as_raw()
        || namespace_metadata.mode() & 0o7777 != 0o700
        || !metadata_matches_stat(&namespace_metadata, &namespace_named)
    {
        return Err(invalid(format!(
            "Managed rollback cleanup namespace is unsafe; it was preserved: {}",
            namespace_path.display()
        )));
    }
    let namespace = BoundDirectory {
        descriptor: namespace_descriptor,
        path: namespace_path.clone(),
        identity: file_identity(&namespace_metadata),
    };
    let entry_path = namespace_path.join("entry");
    let scratch_state = inspect_bound_regular(parent, scratch_name, display_path)?;
    let entry_state = inspect_bound_regular(&namespace, OsStr::new("entry"), &entry_path)?;
    if scratch_state.is_some() && entry_state.is_some() {
        return Err(invalid(format!(
            "Managed rollback cleanup is ambiguous; scratch and private quarantine were preserved: {}",
            display_path.display()
        )));
    }
    if let Some(state) = scratch_state {
        if !matches(&state) {
            return Err(invalid(format!(
                "Managed rollback scratch does not belong to this transaction: {}",
                parent.path.join(scratch_name).display()
            )));
        }
        before_final_capture();
        renameat_with(
            &parent.descriptor,
            scratch_name,
            &namespace.descriptor,
            "entry",
            RenameFlags::NOREPLACE,
        )
        .map_err(errno_error)?;
        parent.sync()?;
        namespace.sync()?;
    }
    let captured = inspect_bound_regular(&namespace, OsStr::new("entry"), &entry_path)?;
    let cleanup = match captured {
        Some(state) if matches(&state) => PrivateCleanup {
            namespace: PrivateNamespace {
                path: namespace_path,
                identity: namespace.identity,
            },
            entry: Some(PrivateEntry {
                namespace: PrivateNamespace {
                    path: namespace.path.clone(),
                    identity: namespace.identity,
                },
                path: entry_path,
                identity: identity_from_file_state(&state)
                    .ok_or_else(|| invalid("Rollback cleanup entry identity is missing"))?,
            }),
        },
        Some(_) => {
            return Err(invalid(format!(
                "Managed rollback cleanup captured unexpected data; private quarantine was preserved: {}",
                namespace.path.display()
            )));
        }
        None => PrivateCleanup {
            namespace: PrivateNamespace {
                path: namespace.path.clone(),
                identity: namespace.identity,
            },
            entry: None,
        },
    };
    cleanup_private_namespace(&cleanup)
}

fn validate_existing_file(path: &Path) -> Result<std::fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "Refusing symbolic link at managed setup path: {}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(invalid(format!(
            "Managed setup path is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.uid() != geteuid().as_raw() {
        return Err(invalid(format!(
            "Managed setup path is owned by another user: {}",
            path.display()
        )));
    }
    if metadata.nlink() != 1 {
        return Err(invalid(format!(
            "Managed setup path must be singly linked: {}",
            path.display()
        )));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(invalid(format!(
            "Managed setup path is group/world-writable: {}",
            path.display()
        )));
    }
    Ok(metadata)
}

fn same_file_state(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
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

pub(crate) fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
    }
}

fn same_identity(metadata: &std::fs::Metadata, expected: &FileIdentity) -> bool {
    metadata.dev() == expected.dev
        && metadata.ino() == expected.ino
        && metadata.uid() == expected.uid
        && metadata.mode() == expected.mode
}

fn state_from_metadata(metadata: &std::fs::Metadata, content_blake3: Option<String>) -> FileState {
    FileState::Present {
        dev: metadata.dev(),
        ino: metadata.ino(),
        uid: metadata.uid(),
        nlink: metadata.nlink(),
        mode: metadata.mode(),
        len: metadata.len(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
        content_blake3,
    }
}

fn state_from_open_file(file: &File, content_blake3: Option<String>) -> Result<FileState> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid(
            "Managed mutation descriptor is not a safe regular file",
        ));
    }
    Ok(state_from_metadata(&metadata, content_blake3))
}

impl WriteReceipt {
    pub(crate) fn persisted_with_cleanup(
        path: &Path,
        file: &File,
        bytes: &[u8],
        pending_cleanup: Option<PrivateCleanup>,
    ) -> Result<Self> {
        let opened = file.metadata()?;
        if !opened.is_file()
            || opened.uid() != geteuid().as_raw()
            || opened.nlink() != 1
            || opened.len() != bytes.len() as u64
        {
            return Err(invalid(format!(
                "Managed write did not produce a safe regular file: {}",
                path.display()
            )));
        }
        let current = validate_existing_file(path)?;
        if !same_file_state(&opened, &current) {
            return Err(invalid(format!(
                "Managed write path changed before ownership was recorded: {}",
                path.display()
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            state: state_from_metadata(&opened, Some(blake3::hash(bytes).to_hex().to_string())),
            pending_cleanup,
        })
    }

    pub(crate) fn removed_with_cleanup(
        path: &Path,
        pending_cleanup: Option<PrivateCleanup>,
    ) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path: path.to_path_buf(),
                state: FileState::Missing,
                pending_cleanup,
            }),
            Ok(_) => Err(invalid(format!(
                "Managed path reappeared before removal ownership was recorded: {}",
                path.display()
            ))),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn without_cleanup(mut self) -> Self {
        self.pending_cleanup = None;
        self
    }

    fn complete_unobserved(self) -> Result<()> {
        if let Some(cleanup) = &self.pending_cleanup {
            cleanup_private_namespace(cleanup)?;
        }
        Ok(())
    }
}

fn private_entry_matches(entry: &PrivateEntry) -> Result<bool> {
    if entry.path.parent() != Some(entry.namespace.path.as_path()) {
        return Err(invalid(
            "Private mutation entry escaped its recorded namespace",
        ));
    }
    let namespace_parent = entry
        .namespace
        .path
        .parent()
        .ok_or_else(|| invalid("Private mutation namespace has no parent"))?;
    let namespace_name = entry
        .namespace
        .path
        .file_name()
        .ok_or_else(|| invalid("Private mutation namespace has no name"))?;
    let parent = BoundDirectory::open(namespace_parent)?;
    let namespace = match openat(
        &parent.descriptor,
        namespace_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(errno_error(error)),
    };
    let namespace_metadata = namespace.metadata()?;
    let namespace_named = statat(
        &parent.descriptor,
        namespace_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if !same_identity(&namespace_metadata, &entry.namespace.identity)
        || !metadata_matches_stat(&namespace_metadata, &namespace_named)
        || namespace_metadata.mode() & 0o7777 != 0o700
    {
        return Err(invalid(format!(
            "Private mutation namespace identity is ambiguous: {}",
            entry.namespace.path.display()
        )));
    }
    let name = entry
        .path
        .file_name()
        .ok_or_else(|| invalid("Private mutation entry has no name"))?;
    let descriptor = match openat(
        &namespace,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(errno_error(error)),
    };
    let metadata = descriptor.metadata()?;
    let named = statat(&namespace, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
        || !same_identity(&metadata, &entry.identity)
        || !metadata_matches_stat(&metadata, &named)
    {
        return Err(invalid(format!(
            "Private mutation entry identity is ambiguous: {}",
            entry.path.display()
        )));
    }
    Ok(true)
}

fn cleanup_private_namespace(cleanup: &PrivateCleanup) -> Result<()> {
    cleanup_private_namespace_with_hook(cleanup, || {})
}

fn cleanup_private_namespace_with_hook(
    cleanup: &PrivateCleanup,
    before_entry_capture: impl FnOnce(),
) -> Result<()> {
    let parent_path = cleanup
        .namespace
        .path
        .parent()
        .ok_or_else(|| invalid("Private mutation namespace has no parent"))?;
    let namespace_name = cleanup
        .namespace
        .path
        .file_name()
        .ok_or_else(|| invalid("Private mutation namespace has no name"))?;
    let parent = BoundDirectory::open(parent_path)?;
    let namespace = match openat(
        &parent.descriptor,
        namespace_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(rustix::io::Errno::NOENT) if cleanup.entry.is_none() => return Ok(()),
        Err(rustix::io::Errno::NOENT) => {
            return Err(invalid(format!(
                "Private mutation cleanup namespace disappeared; ownership is ambiguous: {}",
                cleanup.namespace.path.display()
            )));
        }
        Err(error) => {
            return Err(invalid(format!(
                "Private mutation cleanup namespace could not be safely reopened; ambiguous data was preserved: {} ({error})",
                cleanup.namespace.path.display()
            )));
        }
    };
    let opened = namespace.metadata()?;
    let named = statat(
        &parent.descriptor,
        namespace_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if !same_identity(&opened, &cleanup.namespace.identity)
        || !metadata_matches_stat(&opened, &named)
        || opened.uid() != geteuid().as_raw()
        || opened.mode() & 0o7777 != 0o700
    {
        return Err(invalid(format!(
            "Private mutation cleanup namespace changed; it was preserved: {}",
            cleanup.namespace.path.display()
        )));
    }

    if let Some(entry) = &cleanup.entry {
        if entry.namespace != cleanup.namespace || !private_entry_matches(entry)? {
            return Err(invalid(format!(
                "Private mutation cleanup entry is missing or ambiguous; namespace was preserved: {}",
                cleanup.namespace.path.display()
            )));
        }
        let entry_name = entry
            .path
            .file_name()
            .ok_or_else(|| invalid("Private mutation cleanup entry has no name"))?;
        let captured_name = final_delete_name("private-entry");
        before_entry_capture();
        renameat_with(
            &namespace,
            entry_name,
            &namespace,
            &captured_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(errno_error)?;
        fsync(&namespace).map_err(errno_error)?;
        let captured =
            statat(&namespace, &captured_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if captured.st_dev as u64 != entry.identity.dev
            || captured.st_ino != entry.identity.ino
            || captured.st_uid != entry.identity.uid
            || captured.st_mode as u32 != entry.identity.mode
        {
            return Err(invalid(format!(
                "Private mutation cleanup captured unexpected data; it was preserved as {}",
                cleanup.namespace.path.join(&captured_name).display()
            )));
        }
        unlinkat(&namespace, &captured_name, AtFlags::empty()).map_err(errno_error)?;
        fsync(&namespace).map_err(errno_error)?;
    }

    let remaining = directory_entry_names(&namespace)?;
    if !remaining.is_empty() {
        return Err(invalid(format!(
            "Private mutation cleanup namespace contains unexpected data and was preserved: {}",
            cleanup.namespace.path.display()
        )));
    }
    parent.verify()?;
    let namespace_after = namespace.metadata()?;
    let named_after = statat(
        &parent.descriptor,
        namespace_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if !same_identity(&namespace_after, &cleanup.namespace.identity)
        || !metadata_matches_stat(&namespace_after, &named_after)
    {
        return Err(invalid(format!(
            "Private mutation cleanup namespace binding changed; it was preserved: {}",
            cleanup.namespace.path.display()
        )));
    }
    let captured_namespace = final_delete_name("private-namespace");
    renameat_with(
        &parent.descriptor,
        namespace_name,
        &parent.descriptor,
        &captured_namespace,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    let final_stat = statat(
        &parent.descriptor,
        &captured_namespace,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if final_stat.st_dev as u64 != cleanup.namespace.identity.dev
        || final_stat.st_ino != cleanup.namespace.identity.ino
        || final_stat.st_uid != cleanup.namespace.identity.uid
        || final_stat.st_mode as u32 != cleanup.namespace.identity.mode
    {
        return Err(invalid(format!(
            "Private mutation cleanup captured an unexpected namespace; it was preserved as {}",
            parent.path.join(&captured_namespace).display()
        )));
    }
    unlinkat(&parent.descriptor, &captured_namespace, AtFlags::REMOVEDIR).map_err(errno_error)?;
    parent.sync()
}

fn inspect_file(path: &Path, capture_bytes: bool) -> Result<(FileState, Option<Vec<u8>>)> {
    let before = match fs::symlink_metadata(path) {
        Ok(_) => validate_existing_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((FileState::Missing, None));
        }
        Err(error) => return Err(error.into()),
    };
    if capture_bytes && before.len() > MAX_SNAPSHOT_BYTES {
        return Err(invalid(format!(
            "Managed setup path exceeds the rollback snapshot limit: {}",
            path.display()
        )));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if !same_file_state(&before, &opened) {
        return Err(invalid(format!(
            "Managed setup path changed while it was opened: {}",
            path.display()
        )));
    }

    let mut retained = capture_bytes.then(Vec::new);
    let content_blake3 = if opened.len() <= MAX_FINGERPRINT_BYTES {
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        let read_limit = if capture_bytes {
            MAX_SNAPSHOT_BYTES
        } else {
            MAX_FINGERPRINT_BYTES
        };
        let mut total_read = 0_u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total_read = total_read.saturating_add(read as u64);
            if total_read > read_limit {
                return Err(invalid(format!(
                    "Managed setup path grew beyond its inspection limit: {}",
                    path.display()
                )));
            }
            hasher.update(&buffer[..read]);
            if let Some(bytes) = retained.as_mut() {
                bytes.extend_from_slice(&buffer[..read]);
            }
        }
        Some(hasher.finalize().to_hex().to_string())
    } else {
        None
    };
    let after_read = file.metadata()?;
    let current = validate_existing_file(path)?;
    if !same_file_state(&before, &after_read) || !same_file_state(&after_read, &current) {
        return Err(invalid(format!(
            "Managed setup path changed while it was inspected: {}",
            path.display()
        )));
    }
    Ok((state_from_metadata(&current, content_blake3), retained))
}

fn validate_parent_chain(path: &Path) -> Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut current = path
        .parent()
        .ok_or_else(|| invalid(format!("Managed path has no parent: {}", path.display())))?;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(invalid(format!(
                        "Refusing symbolic link in managed path: {}",
                        current.display()
                    )));
                }
                if !metadata.is_dir() {
                    return Err(invalid(format!(
                        "Managed path parent is not a directory: {}",
                        current.display()
                    )));
                }
                if metadata.mode() & 0o002 != 0 && metadata.mode() & 0o1000 == 0 {
                    return Err(invalid(format!(
                        "Managed path parent is world-writable without a sticky bit: {}",
                        current.display()
                    )));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    invalid(format!(
                        "Managed path has no existing ancestor: {}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(missing)
}

fn capture_owned_directory_tree(
    path: &Path,
    identity: &FileIdentity,
) -> Result<Vec<DirectorySnapshot>> {
    let (parent_path, name) = path_parts(path)?;
    let parent = BoundDirectory::open(parent_path)?;
    let directory = File::from(
        openat(
            &parent.descriptor,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?,
    );
    verify_directory_binding(&directory, &parent.descriptor, name, identity)?;
    let mut captured = Vec::new();
    capture_owned_directory_node(
        &directory,
        &parent.descriptor,
        name,
        path,
        identity,
        &mut captured,
    )?;
    parent.verify()?;
    Ok(captured)
}

fn capture_owned_directory_node(
    directory: &File,
    parent: &File,
    directory_name: &OsStr,
    path: &Path,
    identity: &FileIdentity,
    captured: &mut Vec<DirectorySnapshot>,
) -> Result<()> {
    verify_directory_binding(directory, parent, directory_name, identity)?;
    let mut entries = Vec::new();
    for name in directory_entry_names(directory)? {
        verify_directory_binding(directory, parent, directory_name, identity)?;
        let before = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        let child_path = path.join(&name);
        match FileType::from_raw_mode(before.st_mode) {
            FileType::Directory => {
                if before.st_uid != geteuid().as_raw() || before.st_mode as u32 & 0o022 != 0 {
                    return Err(invalid(format!(
                        "Transaction-created runtime directory contains an unsafe child directory: {}",
                        child_path.display()
                    )));
                }
                let child = File::from(
                    openat(
                        directory,
                        &name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(errno_error)?,
                );
                let opened = fstat(&child).map_err(errno_error)?;
                let named =
                    statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
                if !same_stat_identity(&before, &opened)
                    || !same_stat_identity(&opened, &named)
                    || opened.st_uid != before.st_uid
                    || opened.st_mode != before.st_mode
                {
                    return Err(invalid(format!(
                        "Transaction-created runtime directory changed while it was inventoried: {}",
                        child_path.display()
                    )));
                }
                let child_identity = FileIdentity {
                    dev: opened.st_dev as u64,
                    ino: opened.st_ino,
                    uid: opened.st_uid,
                    mode: opened.st_mode as u32,
                };
                capture_owned_directory_node(
                    &child,
                    directory,
                    &name,
                    &child_path,
                    &child_identity,
                    captured,
                )?;
            }
            FileType::RegularFile => {
                if before.st_uid != geteuid().as_raw()
                    || before.st_nlink != 1
                    || before.st_mode as u32 & 0o022 != 0
                {
                    return Err(invalid(format!(
                        "Transaction-created runtime directory contains an unsafe file: {}",
                        child_path.display()
                    )));
                }
                let file = File::from(
                    openat(
                        directory,
                        &name,
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                        Mode::empty(),
                    )
                    .map_err(errno_error)?,
                );
                let opened = fstat(&file).map_err(errno_error)?;
                let named =
                    statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
                if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
                    || !same_stat_identity(&before, &opened)
                    || !same_stat_identity(&opened, &named)
                    || opened.st_uid != before.st_uid
                    || opened.st_nlink != 1
                    || opened.st_mode != before.st_mode
                {
                    return Err(invalid(format!(
                        "Transaction-created runtime file changed while it was inventoried: {}",
                        child_path.display()
                    )));
                }
                entries.push(OwnedDirectoryEntry {
                    path: child_path,
                    identity: FileIdentity {
                        dev: opened.st_dev as u64,
                        ino: opened.st_ino,
                        uid: opened.st_uid,
                        mode: opened.st_mode as u32,
                    },
                });
            }
            FileType::Socket => {
                if before.st_uid != geteuid().as_raw()
                    || before.st_nlink != 1
                    || before.st_mode as u32 & 0o022 != 0
                {
                    return Err(invalid(format!(
                        "Transaction-created runtime directory contains an unsafe socket: {}",
                        child_path.display()
                    )));
                }
                verify_directory_binding(directory, parent, directory_name, identity)?;
                let after =
                    statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
                if !same_stat_identity(&before, &after)
                    || after.st_uid != before.st_uid
                    || after.st_nlink != 1
                    || after.st_mode != before.st_mode
                {
                    return Err(invalid(format!(
                        "Transaction-created runtime socket changed while it was inventoried: {}",
                        child_path.display()
                    )));
                }
                entries.push(OwnedDirectoryEntry {
                    path: child_path,
                    identity: FileIdentity {
                        dev: after.st_dev as u64,
                        ino: after.st_ino,
                        uid: after.st_uid,
                        mode: after.st_mode as u32,
                    },
                });
            }
            _ => {
                return Err(invalid(format!(
                    "Transaction-created runtime directory contains an unowned special entry: {}",
                    child_path.display()
                )));
            }
        }
    }
    verify_directory_binding(directory, parent, directory_name, identity)?;
    captured.push(DirectorySnapshot {
        path: path.to_path_buf(),
        identity: Some(*identity),
        owns_contents: true,
        entries,
    });
    Ok(())
}

impl SnapshotSet {
    pub(crate) fn capture(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let paths = paths.into_iter().collect::<BTreeSet<_>>();
        if paths.len() > MAX_SNAPSHOT_FILES {
            return Err(invalid(format!(
                "Setup rollback set exceeds {MAX_SNAPSHOT_FILES} files"
            )));
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut missing_parents = BTreeSet::new();
        let mut total_snapshot_bytes = 0_u64;
        for path in paths {
            if !path.is_absolute() {
                return Err(invalid(format!(
                    "Managed setup path must be absolute: {}",
                    path.display()
                )));
            }
            missing_parents.extend(validate_parent_chain(&path)?);
            let (previous, state) = match inspect_file(&path, true)? {
                (FileState::Missing, _) => (PreviousFile::Missing, FileState::Missing),
                (state @ FileState::Present { mode, .. }, Some(bytes)) => (
                    PreviousFile::Present {
                        bytes,
                        mode: mode & 0o777,
                    },
                    state,
                ),
                (FileState::Present { .. }, None) => {
                    return Err(invalid(format!(
                        "Managed setup path could not be captured: {}",
                        path.display()
                    )));
                }
            };
            if let PreviousFile::Present { bytes, .. } = &previous {
                total_snapshot_bytes = total_snapshot_bytes.saturating_add(bytes.len() as u64);
                if total_snapshot_bytes > MAX_SNAPSHOT_TOTAL_BYTES {
                    return Err(invalid(format!(
                        "Setup rollback snapshots exceed {MAX_SNAPSHOT_TOTAL_BYTES} bytes"
                    )));
                }
            }
            files.push(FileSnapshot {
                path,
                previous,
                expected: ExpectedState::Exact {
                    state,
                    pending_cleanup: None,
                },
            });
        }
        let mut directories = missing_parents
            .into_iter()
            .map(|path| DirectorySnapshot {
                path,
                identity: None,
                owns_contents: false,
                entries: Vec::new(),
            })
            .collect::<Vec<_>>();
        directories.sort_by_key(|directory| std::cmp::Reverse(directory.path.components().count()));
        Ok(Self { files, directories })
    }

    pub(crate) fn track_directories(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<()> {
        let mut tracked = self
            .directories
            .iter()
            .map(|directory| directory.path.clone())
            .collect::<BTreeSet<_>>();
        for path in paths {
            if !path.is_absolute() {
                return Err(invalid(format!(
                    "Tracked setup directory must be absolute: {}",
                    path.display()
                )));
            }
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(invalid(format!(
                            "Tracked setup directory is unsafe: {}",
                            path.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if let Some(directory) = self
                        .directories
                        .iter_mut()
                        .find(|directory| directory.path == path)
                    {
                        directory.owns_contents = true;
                    } else if tracked.insert(path.clone()) {
                        self.directories.push(DirectorySnapshot {
                            path,
                            identity: None,
                            owns_contents: true,
                            entries: Vec::new(),
                        });
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        if self.directories.len() > MAX_DIRECTORY_SNAPSHOTS {
            return Err(invalid(format!(
                "Setup rollback set exceeds {MAX_DIRECTORY_SNAPSHOTS} directories"
            )));
        }
        self.directories
            .sort_by_key(|directory| std::cmp::Reverse(directory.path.components().count()));
        Ok(())
    }

    pub(crate) fn descriptions(&self) -> Vec<SnapshotDescription> {
        self.files
            .iter()
            .map(|snapshot| SnapshotDescription {
                path: snapshot.path.clone(),
                previous: match snapshot.previous {
                    PreviousFile::Missing => "missing",
                    PreviousFile::Present { .. } => "file",
                },
            })
            .collect()
    }

    fn snapshot_mut(&mut self, path: &Path) -> Result<&mut FileSnapshot> {
        self.files
            .iter_mut()
            .find(|snapshot| snapshot.path == path)
            .ok_or_else(|| {
                invalid(format!(
                    "Cannot update a path outside the rollback set: {}",
                    path.display()
                ))
            })
    }

    pub(crate) fn prepare_write(&mut self, journal: &Journal, intent: PreparedWrite) -> Result<()> {
        if intent.len > MAX_SNAPSHOT_BYTES {
            return Err(invalid(format!(
                "Managed setup write exceeds the transaction limit: {}",
                intent.path.display()
            )));
        }
        let snapshot = self.snapshot_mut(&intent.path)?;
        let before = match &snapshot.expected {
            ExpectedState::Exact {
                state,
                pending_cleanup: None,
            } => state.clone(),
            _ => {
                return Err(invalid(format!(
                    "Managed setup path already has a pending mutation: {}",
                    intent.path.display()
                )));
            }
        };
        if before != intent.before {
            return Err(invalid(format!(
                "Managed setup path changed before its staged inode was recorded: {}",
                intent.path.display()
            )));
        }
        snapshot.expected = ExpectedState::PlannedWrite {
            before,
            staged: intent.staged,
            len: intent.len,
            content_blake3: intent.content_blake3,
        };
        journal.persist_snapshot(self)
    }

    pub(crate) fn prepare_removal(
        &mut self,
        journal: &Journal,
        intent: PreparedRemoval,
    ) -> Result<()> {
        let snapshot = self.snapshot_mut(&intent.path)?;
        let before = match &snapshot.expected {
            ExpectedState::Exact {
                state,
                pending_cleanup: None,
            } => state.clone(),
            _ => {
                return Err(invalid(format!(
                    "Managed setup path already has a pending mutation: {}",
                    intent.path.display()
                )));
            }
        };
        if before != intent.before {
            return Err(invalid(format!(
                "Managed setup path changed before its captured inode was recorded: {}",
                intent.path.display()
            )));
        }
        snapshot.expected = ExpectedState::PlannedRemoval {
            before,
            captured: intent.captured,
        };
        journal.persist_snapshot(self)
    }

    #[cfg(test)]
    pub(crate) fn prepare_creation(&mut self, journal: &Journal, path: &Path) -> Result<()> {
        let snapshot = self.snapshot_mut(path)?;
        let before = match &snapshot.expected {
            ExpectedState::Exact {
                state: FileState::Missing,
                pending_cleanup: None,
            } => FileState::Missing,
            _ => {
                return Err(invalid(format!(
                    "Managed creation path was not initially absent: {}",
                    path.display()
                )));
            }
        };
        snapshot.expected = ExpectedState::PlannedCreation { before };
        journal.persist_snapshot(self)
    }

    pub(crate) fn create_mutable_file(&mut self, journal: &Journal, path: &Path) -> Result<File> {
        let parent = path.parent().ok_or_else(|| {
            invalid(format!(
                "Mutable creation has no parent: {}",
                path.display()
            ))
        })?;
        let parent_before = fs::symlink_metadata(parent)?;
        if parent_before.file_type().is_symlink()
            || !parent_before.is_dir()
            || parent_before.uid() != geteuid().as_raw()
            || parent_before.mode() & 0o022 != 0
        {
            return Err(invalid(format!(
                "Mutable creation parent is unsafe: {}",
                parent.display()
            )));
        }
        let parent_directory = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(parent)?;
        let opened_parent = parent_directory.metadata()?;
        if !same_identity(&opened_parent, &file_identity(&parent_before)) {
            return Err(invalid(format!(
                "Mutable creation parent changed while it was opened: {}",
                parent.display()
            )));
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "Mutable creation target already exists: {}",
                    path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }

        let staging_path = parent.join(format!(
            ".hardknock-mutable-{}.staging",
            uuid::Uuid::new_v4()
        ));
        let snapshot = self.snapshot_mut(path)?;
        let before = match &snapshot.expected {
            ExpectedState::Exact {
                state: FileState::Missing,
                pending_cleanup: None,
            }
            | ExpectedState::PlannedCreation {
                before: FileState::Missing,
            } => FileState::Missing,
            _ => {
                return Err(invalid(format!(
                    "Mutable creation target was not initially absent: {}",
                    path.display()
                )));
            }
        };
        snapshot.expected = ExpectedState::MutableCreation {
            before,
            identity: None,
            staging_path: Some(staging_path.clone()),
        };
        journal.persist_snapshot(self)?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&staging_path)?;
        file.sync_all()?;
        let opened = file.metadata()?;
        let named = validate_existing_file(&staging_path)?;
        if !same_file_state(&opened, &named) {
            return Err(invalid(
                "Mutable creation staging path changed before ownership was recorded",
            ));
        }
        let identity = file_identity(&opened);
        let snapshot = self.snapshot_mut(path)?;
        snapshot.expected = ExpectedState::MutableCreation {
            before: FileState::Missing,
            identity: Some(identity),
            staging_path: Some(staging_path.clone()),
        };
        journal.persist_snapshot(self)?;

        let current_parent = fs::symlink_metadata(parent)?;
        if !same_identity(&current_parent, &file_identity(&opened_parent)) {
            return Err(invalid(
                "Mutable creation parent changed before staged publication",
            ));
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "Mutable creation target appeared before publication: {}",
                    path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        rename_no_replace(&staging_path, path)?;
        parent_directory.sync_all()?;
        let published = validate_existing_file(path)?;
        if !same_identity(&published, &identity) {
            return Err(invalid(
                "Mutable creation inode changed during staged publication",
            ));
        }
        Ok(file)
    }

    pub(crate) fn checkpoint_receipt(
        &mut self,
        journal: &Journal,
        receipt: WriteReceipt,
    ) -> Result<()> {
        let path = receipt.path.clone();
        let state = receipt.state.clone();
        let pending_cleanup = receipt.pending_cleanup.clone();
        let snapshot = self.snapshot_mut(&path)?;
        snapshot.expected = ExpectedState::Exact {
            state: state.clone(),
            pending_cleanup: pending_cleanup.clone(),
        };
        journal.persist_snapshot(self)?;
        if let Some(cleanup) = &pending_cleanup {
            cleanup_private_namespace(cleanup)?;
            let snapshot = self.snapshot_mut(&path)?;
            snapshot.expected = ExpectedState::Exact {
                state,
                pending_cleanup: None,
            };
            journal.persist_snapshot(self)?;
        }
        Ok(())
    }

    pub(crate) fn abort_mutation(
        &mut self,
        journal: &Journal,
        path: &Path,
        cleanup: PrivateCleanup,
    ) -> Result<()> {
        let before = {
            let snapshot = self.snapshot_mut(path)?;
            match &snapshot.expected {
                ExpectedState::PlannedWrite { before, .. }
                | ExpectedState::PlannedRemoval { before, .. } => before.clone(),
                _ => {
                    return Err(invalid(format!(
                        "Managed setup path has no prepared mutation to abort: {}",
                        path.display()
                    )));
                }
            }
        };
        cleanup_private_namespace(&cleanup)?;
        let current = inspect_file(path, false)?.0;
        if current != before {
            return Err(invalid(format!(
                "Managed setup path changed while its prepared mutation was aborted: {}",
                path.display()
            )));
        }
        let snapshot = self.snapshot_mut(path)?;
        snapshot.expected = ExpectedState::Exact {
            state: before,
            pending_cleanup: None,
        };
        journal.persist_snapshot(self)
    }

    pub(crate) fn checkpoint(
        &mut self,
        journal: &Journal,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<()> {
        for path in paths.into_iter().collect::<BTreeSet<_>>() {
            let snapshot = self.snapshot_mut(&path)?;
            snapshot.expected = ExpectedState::Exact {
                state: inspect_file(&path, false)?.0,
                pending_cleanup: None,
            };
        }
        journal.persist_snapshot(self)
    }

    pub(crate) fn record_directory(
        &mut self,
        journal: &Journal,
        receipt: DirectoryReceipt,
    ) -> Result<()> {
        let directory = self
            .directories
            .iter_mut()
            .find(|directory| directory.path == receipt.path)
            .ok_or_else(|| {
                invalid(format!(
                    "Cannot record a directory outside the rollback set: {}",
                    receipt.path.display()
                ))
            })?;
        match &directory.identity {
            Some(identity) if identity != &receipt.identity => {
                return Err(invalid(format!(
                    "Transaction-created directory identity changed: {}",
                    receipt.path.display()
                )));
            }
            Some(_) => return Ok(()),
            None => directory.identity = Some(receipt.identity),
        }
        journal.persist_snapshot(self)
    }

    pub(crate) fn record_existing_created_directories(&mut self, journal: &Journal) -> Result<()> {
        for directory in &self.directories {
            if directory.owns_contents
                && directory.identity.is_none()
                && path_exists_no_follow(&directory.path)?
            {
                return Err(invalid(format!(
                    "Transaction-created runtime directory appeared before its identity was durably recorded: {}",
                    directory.path.display()
                )));
            }
        }

        let owned_paths = self
            .directories
            .iter()
            .filter(|directory| directory.owns_contents && directory.identity.is_some())
            .map(|directory| directory.path.clone())
            .collect::<BTreeSet<_>>();
        let roots = self
            .directories
            .iter()
            .filter(|directory| {
                directory.owns_contents
                    && directory.identity.is_some()
                    && !owned_paths
                        .iter()
                        .any(|other| other != &directory.path && directory.path.starts_with(other))
            })
            .map(|directory| (directory.path.clone(), directory.identity.unwrap()))
            .collect::<Vec<_>>();

        let mut captured = Vec::new();
        for (path, identity) in roots {
            match fs::symlink_metadata(&path) {
                Ok(_) => captured.extend(capture_owned_directory_tree(&path, &identity)?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }

        let before = self.directories.clone();
        let mut indices = self
            .directories
            .iter()
            .enumerate()
            .map(|(index, directory)| (directory.path.clone(), index))
            .collect::<BTreeMap<_, _>>();
        for captured_directory in captured {
            for entry in &captured_directory.entries {
                if indices.contains_key(&entry.path) {
                    return Err(invalid(format!(
                        "Transaction runtime path changed from a directory to a file: {}",
                        entry.path.display()
                    )));
                }
            }
            if let Some(index) = indices.get(&captured_directory.path).copied() {
                let directory = &mut self.directories[index];
                match directory.identity {
                    Some(identity) if identity != captured_directory.identity.unwrap() => {
                        return Err(invalid(format!(
                            "Transaction-created directory identity changed while its contents were inventoried: {}",
                            directory.path.display()
                        )));
                    }
                    None => directory.identity = captured_directory.identity,
                    Some(_) => {}
                }
                directory.owns_contents = true;
                for entry in captured_directory.entries {
                    match directory
                        .entries
                        .iter()
                        .find(|recorded| recorded.path == entry.path)
                    {
                        Some(recorded) if recorded.identity != entry.identity => {
                            return Err(invalid(format!(
                                "Transaction-created runtime file identity changed before inventory checkpoint: {}",
                                entry.path.display()
                            )));
                        }
                        Some(_) => {}
                        None => directory.entries.push(entry),
                    }
                }
                directory
                    .entries
                    .sort_by(|left, right| left.path.cmp(&right.path));
            } else {
                if self
                    .directories
                    .iter()
                    .flat_map(|directory| &directory.entries)
                    .any(|entry| entry.path == captured_directory.path)
                {
                    return Err(invalid(format!(
                        "Transaction runtime path changed from a file to a directory: {}",
                        captured_directory.path.display()
                    )));
                }
                let index = self.directories.len();
                indices.insert(captured_directory.path.clone(), index);
                self.directories.push(captured_directory);
            }
        }
        if self.directories.len() > MAX_DIRECTORY_SNAPSHOTS
            || self
                .directories
                .iter()
                .map(|directory| directory.entries.len())
                .sum::<usize>()
                > MAX_DIRECTORY_ENTRIES
        {
            return Err(invalid(
                "Transaction-created runtime inventory exceeds its safety limit",
            ));
        }
        self.directories
            .sort_by_key(|directory| std::cmp::Reverse(directory.path.components().count()));
        if self.directories != before {
            journal.persist_snapshot(self)?;
        }
        Ok(())
    }

    pub(crate) fn rollback(&self, journal: &Journal) -> Result<()> {
        self.rollback_for_transaction(&journal.transaction_id, |_| {})
    }

    fn rollback_for_transaction(
        &self,
        transaction_id: &str,
        mut before_mutation: impl FnMut(&Path),
    ) -> Result<()> {
        let mut failures = Vec::new();
        for snapshot in self.files.iter().rev() {
            if let Err(error) = remove_mutable_staging(snapshot, transaction_id) {
                failures.push(format!("{}: {error}", snapshot.path.display()));
            }
            let current = match inspect_file(&snapshot.path, false) {
                Ok((state, _)) => state,
                Err(error) => {
                    failures.push(format!("{}: {error}", snapshot.path.display()));
                    continue;
                }
            };
            if previous_matches(&current, &snapshot.previous) {
                if let Err(error) = cleanup_expected_private_state(snapshot, &current) {
                    failures.push(format!("{}: {error}", snapshot.path.display()));
                }
                continue;
            }
            let transaction_owned =
                match expected_matches_with_private(&current, &snapshot.expected) {
                    Ok(matches) => matches,
                    Err(error) => {
                        failures.push(format!("{}: {error}", snapshot.path.display()));
                        continue;
                    }
                };
            if !transaction_owned {
                failures.push(format!(
                    "{}: changed after the setup transaction wrote it; preserved current content",
                    snapshot.path.display()
                ));
                continue;
            }
            let result = match &snapshot.previous {
                PreviousFile::Missing => {
                    remove_managed_file(&snapshot.path, transaction_id, &snapshot.expected, || {
                        before_mutation(&snapshot.path)
                    })
                }
                PreviousFile::Present { bytes, mode } => restore_file(
                    &snapshot.path,
                    bytes,
                    *mode,
                    transaction_id,
                    &snapshot.expected,
                    || before_mutation(&snapshot.path),
                ),
            };
            if let Err(error) = result {
                failures.push(format!("{}: {error}", snapshot.path.display()));
            } else if let Err(error) = cleanup_expected_private_state(snapshot, &current) {
                failures.push(format!("{}: {error}", snapshot.path.display()));
            }
        }
        for directory in &self.directories {
            match remove_recorded_directory(directory) {
                Ok(()) => {}
                Err(error) => {
                    failures.push(format!("{}: {error}", directory.path.display()));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(invalid(format!(
                "Setup rollback could not restore every managed path: {}",
                failures.join("; ")
            )))
        }
    }

    #[cfg(test)]
    fn rollback_with_hook(
        &self,
        journal: &Journal,
        before_mutation: impl FnMut(&Path),
    ) -> Result<()> {
        self.rollback_for_transaction(&journal.transaction_id, before_mutation)
    }

    pub(crate) fn verify_rollback_complete(&self) -> Result<()> {
        let mut failures = Vec::new();
        for snapshot in &self.files {
            if let Some(staging_path) = mutable_staging_path(snapshot)
                && !matches!(
                    fs::symlink_metadata(staging_path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            {
                failures.push(format!(
                    "{}: transaction-owned staging file still exists",
                    staging_path.display()
                ));
            }
            let current = match inspect_file(&snapshot.path, false) {
                Ok((state, _)) => state,
                Err(error) => {
                    failures.push(format!("{}: {error}", snapshot.path.display()));
                    continue;
                }
            };
            if !previous_matches(&current, &snapshot.previous) {
                failures.push(format!(
                    "{}: pre-transaction content has not been restored",
                    snapshot.path.display()
                ));
            }
        }
        for directory in &self.directories {
            if !directory.owns_contents {
                continue;
            }
            match fs::symlink_metadata(&directory.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => failures.push(format!(
                    "{}: transaction-created directory still exists",
                    directory.path.display()
                )),
                Err(error) => failures.push(format!("{}: {error}", directory.path.display())),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(invalid(format!(
                "Setup rollback is incomplete; recovery was preserved: {}",
                failures.join("; ")
            )))
        }
    }
}

fn mutable_staging_path(snapshot: &FileSnapshot) -> Option<&Path> {
    match &snapshot.expected {
        ExpectedState::MutableCreation {
            staging_path: Some(path),
            ..
        } => Some(path),
        _ => None,
    }
}

fn remove_mutable_staging(snapshot: &FileSnapshot, transaction_id: &str) -> Result<()> {
    let ExpectedState::MutableCreation {
        identity,
        staging_path: Some(staging_path),
        ..
    } = &snapshot.expected
    else {
        return Ok(());
    };
    let expected = if identity.is_some() {
        ExpectedState::MutableCreation {
            before: FileState::Missing,
            identity: *identity,
            staging_path: Some(staging_path.clone()),
        }
    } else {
        let current = inspect_file(staging_path, false)?.0;
        if current == FileState::Missing {
            return Ok(());
        }
        return Err(invalid(format!(
            "Mutable staging identity was never durably recorded; preserved for intervention: {}",
            staging_path.display()
        )));
    };
    remove_managed_file(staging_path, transaction_id, &expected, || {})
}

fn file_state_matches_identity(current: &FileState, identity: &FileIdentity) -> bool {
    matches!(
        current,
        FileState::Present {
            dev,
            ino,
            uid,
            nlink,
            mode,
            ..
        } if *dev == identity.dev
            && *ino == identity.ino
            && *uid == identity.uid
            && *nlink == 1
            && *mode == identity.mode
    )
}

fn identity_from_file_state(state: &FileState) -> Option<FileIdentity> {
    match state {
        FileState::Present {
            dev,
            ino,
            uid,
            mode,
            ..
        } => Some(FileIdentity {
            dev: *dev,
            ino: *ino,
            uid: *uid,
            mode: *mode,
        }),
        FileState::Missing => None,
    }
}

fn expected_matches(current: &FileState, expected: &ExpectedState) -> bool {
    match expected {
        ExpectedState::Exact { state, .. } => current == state,
        ExpectedState::PlannedWrite { before, staged, .. } => {
            current == before || file_state_matches_identity(current, &staged.identity)
        }
        ExpectedState::PlannedRemoval { before, .. } => {
            current == before || matches!(current, FileState::Missing)
        }
        ExpectedState::PlannedCreation { before } => current == before,
        ExpectedState::MutableCreation {
            before, identity, ..
        } => {
            current == before
                || identity.as_ref().is_some_and(|identity| {
                    matches!(
                        current,
                        FileState::Present {
                            dev,
                            ino,
                            uid,
                            nlink,
                            mode,
                            ..
                        } if *dev == identity.dev
                            && *ino == identity.ino
                            && *uid == identity.uid
                            && *nlink == 1
                            && *mode == identity.mode
                    )
                })
        }
    }
}

fn expected_matches_with_private(current: &FileState, expected: &ExpectedState) -> Result<bool> {
    match expected {
        ExpectedState::PlannedRemoval { before, captured } => {
            if current == before {
                Ok(true)
            } else if matches!(current, FileState::Missing) {
                private_entry_matches(captured)
            } else {
                Ok(false)
            }
        }
        _ => Ok(expected_matches(current, expected)),
    }
}

fn cleanup_expected_private_state(snapshot: &FileSnapshot, current: &FileState) -> Result<()> {
    let cleanup = match &snapshot.expected {
        ExpectedState::Exact {
            pending_cleanup: Some(cleanup),
            ..
        } => Some(cleanup.clone()),
        ExpectedState::PlannedWrite { staged, before, .. } => {
            if current == before {
                Some(PrivateCleanup {
                    namespace: staged.namespace.clone(),
                    entry: Some(staged.clone()),
                })
            } else if file_state_matches_identity(current, &staged.identity) {
                let displaced = identity_from_file_state(before).map(|identity| PrivateEntry {
                    namespace: staged.namespace.clone(),
                    path: staged.path.clone(),
                    identity,
                });
                Some(PrivateCleanup {
                    namespace: staged.namespace.clone(),
                    entry: displaced,
                })
            } else {
                None
            }
        }
        ExpectedState::PlannedRemoval { before, captured } => {
            if private_entry_matches(captured)? {
                Some(PrivateCleanup {
                    namespace: captured.namespace.clone(),
                    entry: Some(captured.clone()),
                })
            } else if current == before {
                Some(PrivateCleanup {
                    namespace: captured.namespace.clone(),
                    entry: None,
                })
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(cleanup) = cleanup {
        cleanup_private_namespace(&cleanup)?;
    }
    Ok(())
}

fn remove_recorded_directory(directory: &DirectorySnapshot) -> Result<()> {
    let metadata = match fs::symlink_metadata(&directory.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let Some(identity) = &directory.identity else {
        return Err(invalid(
            "Directory appeared without a durably recorded transaction identity; preserved for intervention",
        ));
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || !same_identity(&metadata, identity)
    {
        return Err(invalid(
            "Transaction-created directory binding changed; current data was preserved",
        ));
    }
    let parent_path = directory
        .path
        .parent()
        .ok_or_else(|| invalid("Transaction-created directory has no parent"))?;
    let name = directory
        .path
        .file_name()
        .ok_or_else(|| invalid("Transaction-created directory has no name"))?;
    let parent = BoundDirectory::open(parent_path)?;
    let descriptor = File::from(
        openat(
            &parent.descriptor,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?,
    );
    let opened = descriptor.metadata()?;
    let named = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !same_identity(&opened, identity) || !metadata_matches_stat(&opened, &named) {
        return Err(invalid(
            "Transaction-created directory changed while it was opened; it was preserved",
        ));
    }
    if directory.owns_contents {
        let bound = BoundDirectory {
            descriptor,
            path: directory.path.clone(),
            identity: *identity,
        };
        for entry in &directory.entries {
            remove_recorded_directory_entry(&bound, entry)?;
        }
        bound.verify()?;
        if !directory_entry_names(&bound.descriptor)?.is_empty() {
            return Err(invalid(
                "Transaction-created runtime directory contains unrecorded data; it was preserved for intervention",
            ));
        }
        remove_empty_recorded_directory(&parent, name, identity, &bound.descriptor)
    } else if directory_entry_names(&descriptor)?.is_empty() {
        remove_empty_recorded_directory(&parent, name, identity, &descriptor)
    } else {
        Ok(())
    }
}

fn remove_recorded_directory_entry(
    directory: &BoundDirectory,
    entry: &OwnedDirectoryEntry,
) -> Result<()> {
    if entry.path.parent() != Some(directory.path.as_path()) {
        return Err(invalid(
            "Recorded transaction runtime file escaped its directory",
        ));
    }
    let name = entry
        .path
        .file_name()
        .ok_or_else(|| invalid("Recorded transaction runtime file has no name"))?;
    directory.verify()?;
    let current = match statat(&directory.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(errno_error(error)),
    };
    let expected_type = FileType::from_raw_mode(entry.identity.mode as _);
    if !matches!(expected_type, FileType::RegularFile | FileType::Socket)
        || FileType::from_raw_mode(current.st_mode) != expected_type
        || current.st_dev as u64 != entry.identity.dev
        || current.st_ino != entry.identity.ino
        || current.st_uid != entry.identity.uid
        || current.st_nlink != 1
        || current.st_mode as u32 != entry.identity.mode
    {
        return Err(invalid(format!(
            "Transaction-created runtime file changed; current data was preserved: {}",
            entry.path.display()
        )));
    }

    let namespace = PrivateMutationDirectory::create(
        &directory.descriptor,
        &directory.path,
        "rollback-runtime",
    )?;
    match renameat_with(
        &directory.descriptor,
        name,
        namespace.descriptor(),
        "entry",
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {}
        Err(rustix::io::Errno::NOENT) => {
            cleanup_private_namespace(&namespace.cleanup_empty())?;
            return match statat(&directory.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => Ok(()),
                Ok(_) => Err(invalid(format!(
                    "Transaction-created runtime file changed during atomic capture; current data was preserved: {}",
                    entry.path.display()
                ))),
                Err(error) => Err(errno_error(error)),
            };
        }
        Err(error) => {
            let cleanup = cleanup_private_namespace(&namespace.cleanup_empty());
            return match cleanup {
                Ok(()) => Err(errno_error(error)),
                Err(cleanup) => Err(Error::Cleanup {
                    primary: Box::new(errno_error(error)),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
    }
    directory.sync()?;
    let captured =
        statat(namespace.descriptor(), "entry", AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if FileType::from_raw_mode(captured.st_mode) != expected_type
        || captured.st_dev as u64 != entry.identity.dev
        || captured.st_ino != entry.identity.ino
        || captured.st_uid != entry.identity.uid
        || captured.st_nlink != 1
        || captured.st_mode as u32 != entry.identity.mode
    {
        return Err(invalid(format!(
            "Transaction rollback captured an unexpected runtime file; it was preserved in {}",
            namespace.entry_path().display()
        )));
    }
    let final_name = final_delete_name("runtime-entry");
    renameat_with(
        namespace.descriptor(),
        "entry",
        namespace.descriptor(),
        &final_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    fsync(namespace.descriptor()).map_err(errno_error)?;
    let final_stat = statat(
        namespace.descriptor(),
        &final_name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if FileType::from_raw_mode(final_stat.st_mode) != expected_type
        || final_stat.st_dev as u64 != entry.identity.dev
        || final_stat.st_ino != entry.identity.ino
        || final_stat.st_uid != entry.identity.uid
        || final_stat.st_nlink != 1
        || final_stat.st_mode as u32 != entry.identity.mode
    {
        return Err(invalid(format!(
            "Transaction rollback final runtime capture changed; data was preserved in {}",
            namespace.namespace.path.join(&final_name).display()
        )));
    }
    unlinkat(namespace.descriptor(), &final_name, AtFlags::empty()).map_err(errno_error)?;
    fsync(namespace.descriptor()).map_err(errno_error)?;
    cleanup_private_namespace(&namespace.cleanup_empty())?;
    directory.verify()
}

fn remove_empty_recorded_directory(
    parent: &BoundDirectory,
    name: &OsStr,
    identity: &FileIdentity,
    descriptor: &File,
) -> Result<()> {
    let captured = final_delete_name("rollback-directory");
    renameat_with(
        &parent.descriptor,
        name,
        &parent.descriptor,
        &captured,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    let opened_after = fstat(descriptor).map_err(errno_error)?;
    let captured_stat =
        statat(&parent.descriptor, &captured, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if opened_after.st_dev as u64 != identity.dev
        || opened_after.st_ino != identity.ino
        || opened_after.st_uid != identity.uid
        || opened_after.st_mode as u32 != identity.mode
        || !same_stat_identity(&opened_after, &captured_stat)
        || captured_stat.st_dev as u64 != identity.dev
        || captured_stat.st_ino != identity.ino
        || captured_stat.st_uid != identity.uid
        || captured_stat.st_mode as u32 != identity.mode
    {
        return Err(invalid(format!(
            "Rollback captured an unexpected directory; it was preserved as {}",
            parent.path.join(&captured).display()
        )));
    }
    unlinkat(&parent.descriptor, &captured, AtFlags::REMOVEDIR).map_err(errno_error)?;
    parent.sync()
}

fn file_state_matches_after_rename(current: &FileState, expected: &FileState) -> bool {
    match (current, expected) {
        (FileState::Missing, FileState::Missing) => true,
        (
            FileState::Present {
                dev: current_dev,
                ino: current_ino,
                uid: current_uid,
                nlink: current_nlink,
                mode: current_mode,
                len: current_len,
                mtime: current_mtime,
                mtime_nsec: current_mtime_nsec,
                content_blake3: current_hash,
                ..
            },
            FileState::Present {
                dev: expected_dev,
                ino: expected_ino,
                uid: expected_uid,
                nlink: expected_nlink,
                mode: expected_mode,
                len: expected_len,
                mtime: expected_mtime,
                mtime_nsec: expected_mtime_nsec,
                content_blake3: expected_hash,
                ..
            },
        ) => {
            current_dev == expected_dev
                && current_ino == expected_ino
                && current_uid == expected_uid
                && current_nlink == expected_nlink
                && current_mode == expected_mode
                && current_len == expected_len
                && current_mtime == expected_mtime
                && current_mtime_nsec == expected_mtime_nsec
                && current_hash == expected_hash
        }
        _ => false,
    }
}

fn expected_matches_after_rename(current: &FileState, expected: &ExpectedState) -> bool {
    match expected {
        ExpectedState::Exact { state, .. } => file_state_matches_after_rename(current, state),
        ExpectedState::PlannedWrite { before, staged, .. } => {
            file_state_matches_after_rename(current, before)
                || file_state_matches_identity(current, &staged.identity)
        }
        ExpectedState::PlannedRemoval { before, .. } => {
            file_state_matches_after_rename(current, before)
                || matches!(current, FileState::Missing)
        }
        ExpectedState::PlannedCreation { before } => {
            file_state_matches_after_rename(current, before)
        }
        ExpectedState::MutableCreation {
            before, identity, ..
        } => {
            file_state_matches_after_rename(current, before)
                || identity.as_ref().is_some_and(|identity| {
                    matches!(
                        current,
                        FileState::Present {
                            dev,
                            ino,
                            uid,
                            nlink,
                            mode,
                            ..
                        } if *dev == identity.dev
                            && *ino == identity.ino
                            && *uid == identity.uid
                            && *nlink == 1
                            && *mode == identity.mode
                    )
                })
        }
    }
}

fn previous_matches(current: &FileState, previous: &PreviousFile) -> bool {
    match (current, previous) {
        (FileState::Missing, PreviousFile::Missing) => true,
        (
            FileState::Present {
                uid,
                nlink,
                mode,
                len,
                content_blake3: Some(current_hash),
                ..
            },
            PreviousFile::Present {
                bytes,
                mode: previous_mode,
            },
        ) => {
            *uid == geteuid().as_raw()
                && *nlink == 1
                && mode & 0o777 == *previous_mode
                && *len == bytes.len() as u64
                && current_hash == &blake3::hash(bytes).to_hex().to_string()
        }
        _ => false,
    }
}

fn remove_managed_file(
    path: &Path,
    transaction_id: &str,
    expected: &ExpectedState,
    mut before_capture: impl FnMut(),
) -> Result<()> {
    let (parent_path, target_name) = path_parts(path)?;
    let parent = BoundDirectory::open(parent_path)?;
    let scratch_name = transaction_scratch_name(transaction_id, path, "remove");
    let scratch_path = parent_path.join(&scratch_name);
    let target = inspect_bound_regular(&parent, target_name, path)?;
    let captured = inspect_bound_regular(&parent, &scratch_name, &scratch_path)?;
    if target.is_some() && captured.is_some() {
        return Err(invalid(format!(
            "Rollback removal is ambiguous; both the managed path and transaction quarantine exist: {}",
            path.display()
        )));
    }
    if let Some(captured) = captured {
        if !expected_matches_after_rename(&captured, expected) {
            if target.is_none() {
                restore_captured_name(&parent, &scratch_name, target_name, path)?;
            }
            return Err(invalid(format!(
                "Rollback quarantine does not match the transaction-owned file: {}",
                path.display()
            )));
        }
        delete_transaction_scratch(&parent, &scratch_name, path, &|state| {
            expected_matches_after_rename(state, expected)
        })?;
        if inspect_bound_regular(&parent, target_name, path)?.is_some() {
            return Err(invalid(format!(
                "A concurrent file appeared while rollback removed its transaction-owned file; current content was preserved: {}",
                path.display()
            )));
        }
        return Ok(());
    }
    let Some(target) = target else {
        return Ok(());
    };
    if !expected_matches(&target, expected) {
        return Err(invalid(format!(
            "Rollback path changed before atomic removal; current content was preserved: {}",
            path.display()
        )));
    }
    before_capture();
    renameat_with(
        &parent.descriptor,
        target_name,
        &parent.descriptor,
        &scratch_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    let captured = inspect_bound_regular(&parent, &scratch_name, &scratch_path)?
        .ok_or_else(|| invalid("Rollback quarantine disappeared after atomic capture"))?;
    if !expected_matches_after_rename(&captured, expected) {
        restore_captured_name(&parent, &scratch_name, target_name, path)?;
        return Err(invalid(format!(
            "Rollback target changed during atomic removal; the captured file was restored: {}",
            path.display()
        )));
    }
    delete_transaction_scratch(&parent, &scratch_name, path, &|state| {
        expected_matches_after_rename(state, expected)
    })?;
    if inspect_bound_regular(&parent, target_name, path)?.is_some() {
        return Err(invalid(format!(
            "A concurrent file appeared during rollback removal; current content was preserved: {}",
            path.display()
        )));
    }
    parent.verify()
}

fn stage_restored_file(
    parent: &BoundDirectory,
    scratch_name: &OsStr,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let scratch_path = parent.path.join(scratch_name);
    if inspect_bound_regular(parent, scratch_name, &scratch_path)?.is_some() {
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&parent.path)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&scratch_path)
        .map_err(|error| Error::Io(error.error))?;
    parent.sync()?;
    let state = inspect_bound_regular(parent, scratch_name, &scratch_path)?
        .ok_or_else(|| invalid("Rollback restoration staging file disappeared"))?;
    let previous = PreviousFile::Present {
        bytes: bytes.to_vec(),
        mode,
    };
    if !previous_matches(&state, &previous) {
        return Err(invalid(format!(
            "Rollback restoration staging file changed: {}",
            scratch_path.display()
        )));
    }
    Ok(())
}

fn compensate_restore_exchange(
    parent: &BoundDirectory,
    scratch_name: &OsStr,
    target_name: &OsStr,
    path: &Path,
    previous: &PreviousFile,
) -> Result<()> {
    let target = inspect_bound_regular(parent, target_name, path)?;
    if !target
        .as_ref()
        .is_some_and(|state| previous_matches(state, previous))
    {
        return Err(invalid(format!(
            "Rollback restoration target changed during compensation; both files were preserved: {}",
            path.display()
        )));
    }
    renameat_with(
        &parent.descriptor,
        scratch_name,
        &parent.descriptor,
        target_name,
        RenameFlags::EXCHANGE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    let scratch_path = parent.path.join(scratch_name);
    let restored = inspect_bound_regular(parent, scratch_name, &scratch_path)?;
    if !restored
        .as_ref()
        .is_some_and(|state| previous_matches(state, previous))
    {
        return Err(invalid(format!(
            "Rollback restoration compensation raced with another mutation; both files were preserved: {}",
            path.display()
        )));
    }
    delete_transaction_scratch(parent, scratch_name, path, &|state| {
        previous_matches(state, previous)
    })
}

fn restore_file(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    transaction_id: &str,
    expected: &ExpectedState,
    mut before_commit: impl FnMut(),
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid(format!("Rollback path has no parent: {}", path.display())))?;
    let parent = BoundDirectory::open(parent)?;
    let (_, target_name) = path_parts(path)?;
    let scratch_name = transaction_scratch_name(transaction_id, path, "restore");
    let scratch_path = parent.path.join(&scratch_name);
    let previous = PreviousFile::Present {
        bytes: bytes.to_vec(),
        mode,
    };

    let mut target = inspect_bound_regular(&parent, target_name, path)?;
    if target
        .as_ref()
        .is_some_and(|state| previous_matches(state, &previous))
    {
        delete_transaction_scratch(&parent, &scratch_name, path, &|state| {
            expected_matches(state, expected)
        })?;
        return Ok(());
    }
    if !target.as_ref().map_or_else(
        || expected_matches(&FileState::Missing, expected),
        |state| expected_matches(state, expected),
    ) {
        return Err(invalid(format!(
            "Rollback restoration target changed; current content was preserved: {}",
            path.display()
        )));
    }

    let mut scratch = inspect_bound_regular(&parent, &scratch_name, &scratch_path)?;
    if scratch
        .as_ref()
        .is_some_and(|state| !previous_matches(state, &previous))
    {
        return Err(invalid(format!(
            "Rollback restoration scratch is not transaction-owned: {}",
            scratch_path.display()
        )));
    }
    if scratch.is_none() {
        stage_restored_file(&parent, &scratch_name, bytes, mode)?;
        scratch = inspect_bound_regular(&parent, &scratch_name, &scratch_path)?;
    }
    if !scratch
        .as_ref()
        .is_some_and(|state| previous_matches(state, &previous))
    {
        return Err(invalid("Rollback restoration staging is unavailable"));
    }

    before_commit();
    if target.is_none() {
        match renameat_with(
            &parent.descriptor,
            &scratch_name,
            &parent.descriptor,
            target_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                parent.sync()?;
                let restored = inspect_bound_regular(&parent, target_name, path)?;
                if restored
                    .as_ref()
                    .is_some_and(|state| previous_matches(state, &previous))
                {
                    return parent.verify();
                }
                return Err(invalid(format!(
                    "Rollback restoration changed during publication: {}",
                    path.display()
                )));
            }
            Err(error) if error == rustix::io::Errno::EXIST => {
                return Err(invalid(format!(
                    "A concurrent file appeared during rollback restoration; it was preserved: {}",
                    path.display()
                )));
            }
            Err(error) => return Err(errno_error(error)),
        }
    }

    renameat_with(
        &parent.descriptor,
        &scratch_name,
        &parent.descriptor,
        target_name,
        RenameFlags::EXCHANGE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    target = inspect_bound_regular(&parent, target_name, path)?;
    let displaced = inspect_bound_regular(&parent, &scratch_name, &scratch_path)?;
    let restored_matches = target
        .as_ref()
        .is_some_and(|state| previous_matches(state, &previous));
    let displaced_matches = displaced
        .as_ref()
        .is_some_and(|state| expected_matches_after_rename(state, expected));
    if !restored_matches || !displaced_matches {
        if restored_matches {
            compensate_restore_exchange(&parent, &scratch_name, target_name, path, &previous)?;
        }
        return Err(invalid(format!(
            "Rollback restoration detected a concurrent replacement; current content was preserved: {}",
            path.display()
        )));
    }
    delete_transaction_scratch(&parent, &scratch_name, path, &|state| {
        expected_matches_after_rename(state, expected)
    })?;
    let restored = inspect_bound_regular(&parent, target_name, path)?;
    if !restored
        .as_ref()
        .is_some_and(|state| previous_matches(state, &previous))
    {
        return Err(invalid(format!(
            "Rollback restoration target changed after commit: {}",
            path.display()
        )));
    }
    parent.verify()
}

impl SetupLock {
    pub(crate) fn acquire(home: &Path) -> Result<Self> {
        Self::acquire_with_timeout(home, SETUP_LOCK_TIMEOUT)
    }

    fn acquire_with_timeout(home: &Path, timeout: Duration) -> Result<Self> {
        let mut anchor = home
            .parent()
            .ok_or_else(|| invalid("Hardknock home has no parent for the setup lock"))?;
        loop {
            match fs::symlink_metadata(anchor) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(invalid(format!(
                            "Setup lock anchor must be a regular directory: {}",
                            anchor.display()
                        )));
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    anchor = anchor.parent().ok_or_else(|| {
                        invalid("Hardknock home has no existing ancestor for the setup lock")
                    })?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let before = fs::symlink_metadata(anchor)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(anchor)?;
        let opened = directory.metadata()?;
        let current = fs::symlink_metadata(anchor)?;
        if before.dev() != opened.dev()
            || before.ino() != opened.ino()
            || opened.dev() != current.dev()
            || opened.ino() != current.ino()
        {
            return Err(invalid("Setup lock anchor changed while it was opened"));
        }
        let started = Instant::now();
        loop {
            match directory.try_lock_exclusive() {
                Ok(()) => return Ok(Self { directory }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(invalid(format!(
                            "Another Hardknock setup transaction is active below {}",
                            anchor.display()
                        )));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for SetupLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.directory);
    }
}

pub(crate) struct Journal {
    transaction_id: String,
    operation: String,
    home: PathBuf,
    initial_home: InitialHome,
    temporary_path: PathBuf,
    recovery_path: PathBuf,
    journal_identity: FileIdentity,
    quarantine: Option<QuarantineAction>,
    file: File,
}

impl Journal {
    pub(crate) fn begin(
        home: &Path,
        operation: &str,
        plan: &Value,
        initial_home: InitialHome,
        snapshots: &SnapshotSet,
    ) -> Result<Self> {
        let parent = home
            .parent()
            .ok_or_else(|| invalid("Hardknock home has no parent for the setup journal"))?;
        let transaction_id = uuid::Uuid::new_v4().to_string();
        let temporary_path =
            parent.join(format!(".hardknock-setup-{transaction_id}.journal.jsonl"));
        let published_recovery_path = recovery_path(home)?;
        match fs::symlink_metadata(&published_recovery_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(
                    "An unfinished Hardknock setup transaction exists; run hardknock repair",
                ));
            }
            Err(error) => return Err(error.into()),
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)?;
        let journal_metadata = file.metadata()?;
        let journal_identity = file_identity(&journal_metadata);
        let staged_recovery_path = parent.join(format!(
            ".hardknock-setup-{transaction_id}.recovery.staging"
        ));
        let recovery_created = create_private_directory(&staged_recovery_path).and_then(|()| {
            create_private_directory(&staged_recovery_path.join(RECOVERY_BLOBS_DIRECTORY))
        });
        if let Err(error) = recovery_created {
            drop(file);
            let _ = cleanup_recovery_directory(&staged_recovery_path);
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
        let mut journal = Self {
            transaction_id,
            operation: operation.to_owned(),
            home: home.to_path_buf(),
            initial_home,
            temporary_path,
            recovery_path: staged_recovery_path.clone(),
            journal_identity,
            quarantine: None,
            file,
        };
        let prepared = (|| {
            journal.persist_snapshot(snapshots)?;
            journal.record(
                "planned",
                json!({"operation":operation,"home":home,"plan":plan}),
            )
        })();
        if let Err(error) = prepared {
            let _ = journal.cleanup_recovery();
            let _ = fs::remove_file(&journal.temporary_path);
            return Err(error);
        }
        sync_directory(&staged_recovery_path)?;
        if let Err(error) = rename_no_replace(&staged_recovery_path, &published_recovery_path) {
            let _ = journal.cleanup_recovery();
            let _ = fs::remove_file(&journal.temporary_path);
            return Err(error);
        }
        journal.recovery_path = published_recovery_path;
        sync_directory(parent)?;
        Ok(journal)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.temporary_path
    }

    pub(crate) fn record(&mut self, event: &str, details: Value) -> Result<()> {
        serde_json::to_writer(
            &mut self.file,
            &json!({
                "schema": JOURNAL_SCHEMA,
                "transaction_id": self.transaction_id,
                "at": Utc::now(),
                "event": event,
                "details": details
            }),
        )?;
        self.file.write_all(b"\n")?;
        self.file.sync_data()?;
        Ok(())
    }

    fn persist_snapshot(&self, snapshots: &SnapshotSet) -> Result<()> {
        let files = snapshots
            .files
            .iter()
            .enumerate()
            .map(|(index, snapshot)| {
                let previous = match &snapshot.previous {
                    PreviousFile::Missing => RecoveryPrevious::Missing,
                    PreviousFile::Present { bytes, mode } => {
                        let blob = format!("{index:03}.bin");
                        self.ensure_blob(&blob, bytes)?;
                        RecoveryPrevious::Present {
                            blob,
                            mode: *mode,
                            len: bytes.len() as u64,
                            content_blake3: blake3::hash(bytes).to_hex().to_string(),
                        }
                    }
                };
                Ok(RecoveryFile {
                    path: snapshot.path.clone(),
                    previous,
                    expected: snapshot.expected.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let state = RecoveryState {
            schema: RECOVERY_SCHEMA.into(),
            transaction_id: self.transaction_id.clone(),
            operation: self.operation.clone(),
            home: self.home.clone(),
            initial_home: self.initial_home,
            journal_path: self.temporary_path.clone(),
            journal_identity: self.journal_identity,
            phase: RecoveryPhase::Active,
            quarantine: self.quarantine.clone(),
            files,
            directories: snapshots
                .directories
                .iter()
                .map(|directory| RecoveryDirectory {
                    path: directory.path.clone(),
                    identity: directory.identity,
                    owns_contents: directory.owns_contents,
                    entries: directory.entries.clone(),
                })
                .collect(),
        };
        write_recovery_state(&self.recovery_path, &state)
    }

    fn ensure_blob(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let path = self.recovery_path.join(RECOVERY_BLOBS_DIRECTORY).join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                validate_recovery_file(&path, &metadata, bytes.len() as u64)?;
                let existing = read_bounded_file(&path, bytes.len() as u64)?;
                if existing != bytes {
                    return Err(invalid("Setup recovery snapshot changed unexpectedly"));
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
                sync_directory(
                    path.parent()
                        .ok_or_else(|| invalid("Recovery blob has no parent"))?,
                )
            }
            Err(error) => Err(error.into()),
        }
    }

    fn cleanup_recovery(&self) -> Result<()> {
        cleanup_recovery_directory(&self.recovery_path)
    }

    #[allow(dead_code)] // Called by the remove-data quarantine flow in setup.
    pub(crate) fn prepare_quarantine(
        &mut self,
        original: &Path,
        quarantine: &Path,
        recovery: QuarantineRecovery,
    ) -> Result<()> {
        validate_quarantine_paths(&self.home, original, quarantine)?;
        let metadata = fs::symlink_metadata(original)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(invalid("Hardknock home is unsafe for quarantine"));
        }
        let action = QuarantineAction {
            original: original.to_path_buf(),
            quarantine: quarantine.to_path_buf(),
            recovery,
            state: QuarantineState::Planned,
            identity: file_identity(&metadata),
            deletion: None,
        };
        update_recovery_quarantine(
            &self.recovery_path,
            &self.transaction_id,
            Some(action.clone()),
        )?;
        self.quarantine = Some(action);
        Ok(())
    }

    #[allow(dead_code)] // Called immediately after the remove-data rename is durable.
    pub(crate) fn checkpoint_quarantine_applied(&mut self) -> Result<()> {
        let mut action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        validate_applied_quarantine(&action)?;
        action.state = QuarantineState::Applied;
        update_recovery_quarantine(
            &self.recovery_path,
            &self.transaction_id,
            Some(action.clone()),
        )?;
        self.quarantine = Some(action);
        Ok(())
    }

    #[allow(dead_code)] // Preferred live-transaction API for the remove-data rename.
    pub(crate) fn apply_quarantine(&mut self) -> Result<()> {
        let action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        if action.state != QuarantineState::Planned {
            return Err(invalid("Setup quarantine rename was already applied"));
        }
        rename_no_replace(&action.original, &action.quarantine)?;
        sync_directory(
            action
                .original
                .parent()
                .ok_or_else(|| invalid("Setup quarantine has no parent"))?,
        )?;
        self.checkpoint_quarantine_applied()
    }

    #[allow(dead_code)] // Used when a live transaction resolves its quarantine.
    pub(crate) fn clear_quarantine(&mut self) -> Result<()> {
        let action = self
            .quarantine
            .as_ref()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        validate_planned_quarantine_not_applied(action)?;
        update_recovery_quarantine(&self.recovery_path, &self.transaction_id, None)?;
        self.quarantine = None;
        Ok(())
    }

    #[allow(dead_code)] // Allows callers to explicitly retain recovery after compensation failure.
    pub(crate) fn preserve_for_repair(mut self, reason: &str) -> Result<PathBuf> {
        self.record("recovery_preserved", json!({"reason":reason}))?;
        Ok(self.recovery_path.clone())
    }

    pub(crate) fn finish(self, home: &Path, outcome: &str) -> Result<PathBuf> {
        let destination = journal_archive_path(home, &self.transaction_id);
        self.finish_to(destination, outcome, outcome.starts_with("rolled_back"))
    }

    #[cfg(test)]
    pub(crate) fn finish_beside_home(self, outcome: &str) -> Result<PathBuf> {
        let destination = self.temporary_path.clone();
        self.finish_to(destination, outcome, outcome.starts_with("rolled_back"))
    }

    pub(crate) fn finish_after_rollback(self, home: &Path) -> Result<PathBuf> {
        if self.initial_home == InitialHome::Existing && home.is_dir() {
            let destination = journal_archive_path(home, &self.transaction_id);
            self.finish_to(destination, "rolled_back", true)
        } else {
            let destination = self.temporary_path.clone();
            self.finish_to(destination, "rolled_back", true)
        }
    }

    #[allow(dead_code)] // Completes a live remove-data transaction after quarantine deletion.
    pub(crate) fn finish_resumed_deletion(mut self, outcome: &str) -> Result<PathBuf> {
        let mut action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine deletion requires finalization"))?;
        prepare_quarantine_deletion(&self.recovery_path, &self.transaction_id, &mut action)?;
        self.quarantine = Some(action.clone());
        let destination = self.temporary_path.clone();
        begin_resumed_deletion_finalization(
            &self.recovery_path,
            &self.transaction_id,
            outcome,
            &destination,
            &action,
        )?;
        self.file.sync_all()?;
        delete_resumed_quarantine(&action)?;
        complete_finalization(
            &self.recovery_path,
            &self.temporary_path,
            &self.journal_identity,
            &destination,
            &self.transaction_id,
            &self.operation,
            outcome,
        )
    }

    fn finish_to(
        self,
        destination: PathBuf,
        outcome: &str,
        require_rollback: bool,
    ) -> Result<PathBuf> {
        if require_rollback {
            verify_persisted_rollback(&self.home, &self.transaction_id)?;
        }
        if self.quarantine.is_some() {
            return Err(invalid(
                "Setup recovery has an unresolved quarantine action; recovery was preserved",
            ));
        }
        begin_finalization(
            &self.recovery_path,
            &self.transaction_id,
            outcome,
            &destination,
        )?;
        self.file.sync_all()?;
        complete_finalization(
            &self.recovery_path,
            &self.temporary_path,
            &self.journal_identity,
            &destination,
            &self.transaction_id,
            &self.operation,
            outcome,
        )
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    ensure_directory_tree(path, true, &mut UnobservedMutation)?;
    validate_recovery_directory(path)
}

fn write_recovery_state(recovery_path: &Path, state: &RecoveryState) -> Result<()> {
    validate_recovery_directory(recovery_path)?;
    let before = fs::symlink_metadata(recovery_path)?;
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() as u64 > MAX_RECOVERY_STATE_BYTES {
        return Err(invalid("Setup recovery state exceeds its size limit"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(recovery_path)?;
    fchmod(temporary.as_file(), Mode::from_raw_mode(0o600)).map_err(errno_error)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    let after = fs::symlink_metadata(recovery_path)?;
    if !same_identity(&after, &file_identity(&before)) {
        return Err(invalid(
            "Setup recovery directory changed while state was written",
        ));
    }
    temporary
        .persist(recovery_path.join(RECOVERY_STATE_FILE))
        .map_err(|error| Error::Io(error.error))?;
    sync_directory(recovery_path)
}

fn update_recovery_quarantine(
    recovery_path: &Path,
    transaction_id: &str,
    quarantine: Option<QuarantineAction>,
) -> Result<()> {
    let mut state = read_recovery_state(&recovery_path.join(RECOVERY_STATE_FILE))?;
    if state.transaction_id != transaction_id || state.phase != RecoveryPhase::Active {
        return Err(invalid(
            "Setup recovery is not active for a quarantine update",
        ));
    }
    state.quarantine = quarantine;
    write_recovery_state(recovery_path, &state)
}

fn validate_quarantine_paths(home: &Path, original: &Path, quarantine: &Path) -> Result<()> {
    if !home.is_absolute() || original != home || !quarantine.is_absolute() {
        return Err(invalid(
            "Setup quarantine must move the exact absolute Hardknock home",
        ));
    }
    let parent = home
        .parent()
        .ok_or_else(|| invalid("Hardknock home has no quarantine parent"))?;
    if quarantine.parent() != Some(parent) || quarantine == home {
        return Err(invalid(
            "Setup quarantine must be a distinct sibling of the Hardknock home",
        ));
    }
    let name = quarantine
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Setup quarantine name is invalid"))?;
    if !name.starts_with(".hardknock-remove-") || name.contains('/') {
        return Err(invalid(
            "Setup quarantine must use the reserved .hardknock-remove- prefix",
        ));
    }
    Ok(())
}

fn validate_quarantine_deletion(
    action: &QuarantineAction,
    deletion: &QuarantineDeletion,
) -> Result<()> {
    let parent = action
        .quarantine
        .parent()
        .ok_or_else(|| invalid("Setup quarantine has no parent"))?;
    if deletion.namespace.path.parent() != Some(parent)
        || deletion.root != deletion.namespace.path.join("root")
    {
        return Err(invalid(
            "Setup quarantine deletion namespace escaped its transaction parent",
        ));
    }
    let name = deletion
        .namespace
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Setup quarantine deletion namespace has an invalid name"))?;
    if !name.starts_with(".hardknock-remove-delete-") || name.contains('/') {
        return Err(invalid(
            "Setup quarantine deletion namespace has an invalid reserved name",
        ));
    }
    Ok(())
}

fn prepare_quarantine_deletion(
    recovery_path: &Path,
    transaction_id: &str,
    action: &mut QuarantineAction,
) -> Result<()> {
    if action.recovery != QuarantineRecovery::ResumeDeletion
        || action.state != QuarantineState::Applied
    {
        return Err(invalid(
            "Setup quarantine is not committed for resumed deletion",
        ));
    }
    if let Some(deletion) = &action.deletion {
        validate_quarantine_deletion(action, deletion)?;
        return Ok(());
    }
    let parent_path = action
        .quarantine
        .parent()
        .ok_or_else(|| invalid("Setup quarantine has no parent"))?;
    let parent = BoundDirectory::open(parent_path)?;
    let namespace =
        PrivateMutationDirectory::create(&parent.descriptor, parent_path, "remove-delete")?;
    let deletion = QuarantineDeletion {
        namespace: namespace.namespace.clone(),
        root: namespace.namespace.path.join("root"),
    };
    validate_quarantine_deletion(action, &deletion)?;
    action.deletion = Some(deletion);
    update_recovery_quarantine(recovery_path, transaction_id, Some(action.clone()))
}

fn validate_applied_quarantine(action: &QuarantineAction) -> Result<()> {
    match fs::symlink_metadata(&action.original) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(invalid(
                "Original Hardknock home still exists after quarantine rename",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&action.quarantine)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || !same_identity(&metadata, &action.identity)
    {
        return Err(invalid("Quarantined Hardknock home is unsafe"));
    }
    sync_directory(
        action
            .quarantine
            .parent()
            .ok_or_else(|| invalid("Setup quarantine has no parent"))?,
    )
}

fn validate_planned_quarantine_not_applied(action: &QuarantineAction) -> Result<()> {
    if action.state != QuarantineState::Planned {
        return Err(invalid(
            "Only a still-planned setup quarantine can be cancelled",
        ));
    }
    let original = fs::symlink_metadata(&action.original)?;
    if original.file_type().is_symlink()
        || !original.is_dir()
        || !same_identity(&original, &action.identity)
    {
        return Err(invalid(
            "Original Hardknock home changed after quarantine preparation",
        ));
    }
    match fs::symlink_metadata(&action.quarantine) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(invalid(
            "Setup quarantine path exists; planned quarantine state is ambiguous",
        )),
        Err(error) => Err(error.into()),
    }
}

fn validate_resumed_deletion(action: &QuarantineAction) -> Result<()> {
    if action.recovery != QuarantineRecovery::ResumeDeletion {
        return Err(invalid(
            "Setup quarantine policy requires restoration rather than deletion",
        ));
    }
    let deletion = action
        .deletion
        .as_ref()
        .ok_or_else(|| invalid("Setup quarantine deletion namespace was not recorded"))?;
    validate_quarantine_deletion(action, deletion)?;
    for path in [
        action.original.as_path(),
        action.quarantine.as_path(),
        deletion.namespace.path.as_path(),
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "Setup quarantine deletion is incomplete: {} still exists",
                    path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }
    sync_directory(
        action
            .original
            .parent()
            .ok_or_else(|| invalid("Setup quarantine has no parent"))?,
    )
}

fn path_exists_no_follow(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn directory_entry_names(directory: &File) -> Result<Vec<OsString>> {
    let mut entries = Dir::read_from(directory).map_err(errno_error)?;
    let mut names = Vec::new();
    for entry in &mut entries {
        let entry = entry.map_err(errno_error)?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    Ok(names)
}

fn verify_directory_binding(
    directory: &File,
    parent: &File,
    name: &OsStr,
    identity: &FileIdentity,
) -> Result<()> {
    let opened = fstat(directory).map_err(errno_error)?;
    let named = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
        || opened.st_dev as u64 != identity.dev
        || opened.st_ino != identity.ino
        || opened.st_uid != identity.uid
        || opened.st_mode as u32 != identity.mode
        || !same_stat_identity(&opened, &named)
    {
        return Err(invalid(
            "Quarantined directory binding changed; ambiguous data was preserved",
        ));
    }
    Ok(())
}

fn remove_bound_directory_entry(
    directory: &File,
    parent: &File,
    directory_name: &OsStr,
    directory_identity: &FileIdentity,
    name: &OsStr,
    guard: &dyn Fn() -> Result<()>,
) -> Result<()> {
    guard()?;
    verify_directory_binding(directory, parent, directory_name, directory_identity)?;
    let before = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    let captured_name = final_delete_name("delete-entry");
    renameat_with(
        directory,
        name,
        directory,
        &captured_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    fsync(directory).map_err(errno_error)?;
    guard()?;
    verify_directory_binding(directory, parent, directory_name, directory_identity)?;
    let captured =
        statat(directory, &captured_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !same_stat_identity(&before, &captured) {
        match renameat_with(
            directory,
            &captured_name,
            directory,
            name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(_) => {
                return Err(invalid(format!(
                    "Quarantined entry changed during atomic capture; unexpected data was preserved as {}",
                    captured_name.to_string_lossy()
                )));
            }
        }
        return Err(invalid(
            "Quarantined entry changed before descriptor-bound removal",
        ));
    }
    if FileType::from_raw_mode(captured.st_mode) == FileType::Directory {
        let child = openat(
            directory,
            &captured_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?;
        let child = File::from(child);
        let opened = fstat(&child).map_err(errno_error)?;
        if !same_stat_identity(&before, &opened) {
            return Err(invalid(
                "Quarantined data directory changed while it was opened",
            ));
        }
        let child_identity = FileIdentity {
            dev: opened.st_dev as u64,
            ino: opened.st_ino,
            uid: opened.st_uid,
            mode: opened.st_mode as u32,
        };
        let child_guard = || {
            guard()?;
            verify_directory_binding(directory, parent, directory_name, directory_identity)
        };
        remove_bound_directory_contents(
            &child,
            directory,
            &captured_name,
            &child_identity,
            &child_guard,
        )?;
        guard()?;
        verify_directory_binding(directory, parent, directory_name, directory_identity)?;
        let opened_after = fstat(&child).map_err(errno_error)?;
        let named_after =
            statat(directory, &captured_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if !same_stat_identity(&opened, &opened_after)
            || !same_stat_identity(&opened_after, &named_after)
        {
            return Err(invalid(
                "Quarantined data directory changed before descriptor-bound removal",
            ));
        }
        let final_name = final_delete_name("delete-directory");
        renameat_with(
            directory,
            &captured_name,
            directory,
            &final_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(errno_error)?;
        fsync(directory).map_err(errno_error)?;
        guard()?;
        verify_directory_binding(directory, parent, directory_name, directory_identity)?;
        let final_stat =
            statat(directory, &final_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if !same_stat_identity(&opened_after, &final_stat) {
            return Err(invalid(
                "Quarantined data directory changed during final atomic capture",
            ));
        }
        unlinkat(directory, &final_name, AtFlags::REMOVEDIR).map_err(errno_error)?;
        fsync(directory).map_err(errno_error)?;
        return Ok(());
    }

    guard()?;
    verify_directory_binding(directory, parent, directory_name, directory_identity)?;
    let final_stat =
        statat(directory, &captured_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !same_stat_identity(&captured, &final_stat) {
        return Err(invalid(
            "Quarantined data entry changed before final removal",
        ));
    }
    unlinkat(directory, &captured_name, AtFlags::empty()).map_err(errno_error)?;
    fsync(directory).map_err(errno_error)?;
    Ok(())
}

fn remove_bound_directory_contents(
    directory: &File,
    parent: &File,
    name: &OsStr,
    identity: &FileIdentity,
    guard: &dyn Fn() -> Result<()>,
) -> Result<()> {
    loop {
        guard()?;
        verify_directory_binding(directory, parent, name, identity)?;
        let names = directory_entry_names(directory)?;
        if names.is_empty() {
            return Ok(());
        }
        for entry in names {
            remove_bound_directory_entry(directory, parent, name, identity, &entry, guard)?;
        }
    }
}

fn open_quarantine_deletion_namespace(
    parent: &BoundDirectory,
    deletion: &QuarantineDeletion,
) -> Result<Option<File>> {
    let name = deletion
        .namespace
        .path
        .file_name()
        .ok_or_else(|| invalid("Setup quarantine deletion namespace has no name"))?;
    let descriptor = match openat(
        &parent.descriptor,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(errno_error(error)),
    };
    let opened = descriptor.metadata()?;
    let named = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if !same_identity(&opened, &deletion.namespace.identity)
        || !metadata_matches_stat(&opened, &named)
        || opened.mode() & 0o7777 != 0o700
    {
        return Err(invalid(format!(
            "Setup quarantine deletion namespace changed; ambiguous data was preserved: {}",
            deletion.namespace.path.display()
        )));
    }
    Ok(Some(descriptor))
}

fn delete_resumed_quarantine(action: &QuarantineAction) -> Result<()> {
    delete_resumed_quarantine_with_hook(action, || {})
}

fn delete_resumed_quarantine_with_hook(
    action: &QuarantineAction,
    before_capture: impl FnOnce(),
) -> Result<()> {
    if action.recovery != QuarantineRecovery::ResumeDeletion
        || action.state != QuarantineState::Applied
    {
        return Err(invalid(
            "Setup quarantine is not committed for resumed deletion",
        ));
    }
    let parent_path = action
        .quarantine
        .parent()
        .ok_or_else(|| invalid("Setup quarantine has no parent"))?;
    let parent = BoundDirectory::open(parent_path)?;
    let quarantine_name = action
        .quarantine
        .file_name()
        .ok_or_else(|| invalid("Setup quarantine name is invalid"))?;
    let deletion = action
        .deletion
        .as_ref()
        .ok_or_else(|| invalid("Setup quarantine deletion namespace was not recorded"))?;
    validate_quarantine_deletion(action, deletion)?;
    let namespace = match open_quarantine_deletion_namespace(&parent, deletion)? {
        Some(namespace) => namespace,
        None => {
            if !path_exists_no_follow(&action.quarantine)? {
                return validate_resumed_deletion(action);
            }
            return Err(invalid(
                "Setup quarantine deletion namespace disappeared; quarantined data was preserved",
            ));
        }
    };
    let namespace_name = deletion
        .namespace
        .path
        .file_name()
        .ok_or_else(|| invalid("Setup quarantine deletion namespace has no name"))?;
    let namespace_guard = || {
        parent.verify()?;
        let opened = namespace.metadata()?;
        let named = statat(
            &parent.descriptor,
            namespace_name,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(errno_error)?;
        if !same_identity(&opened, &deletion.namespace.identity)
            || !metadata_matches_stat(&opened, &named)
        {
            return Err(invalid(
                "Setup quarantine deletion parent binding changed; ambiguous data was preserved",
            ));
        }
        Ok(())
    };

    let quarantine_state = statat(
        &parent.descriptor,
        quarantine_name,
        AtFlags::SYMLINK_NOFOLLOW,
    );
    let root_state = statat(&namespace, "root", AtFlags::SYMLINK_NOFOLLOW);
    if quarantine_state.is_ok() && root_state.is_ok() {
        return Err(invalid(
            "Setup quarantine deletion is ambiguous; both source and private root were preserved",
        ));
    }
    if root_state.is_err_and(|error| error != rustix::io::Errno::NOENT) {
        return Err(errno_error(root_state.unwrap_err()));
    }
    if root_state.is_err() {
        match quarantine_state {
            Err(rustix::io::Errno::NOENT) => {
                if !directory_entry_names(&namespace)?.is_empty() {
                    return Err(invalid(
                        "Setup deletion namespace contains ambiguous data and was preserved",
                    ));
                }
            }
            Err(error) => return Err(errno_error(error)),
            Ok(_) => {
                before_capture();
                namespace_guard()?;
                renameat_with(
                    &parent.descriptor,
                    quarantine_name,
                    &namespace,
                    "root",
                    RenameFlags::NOREPLACE,
                )
                .map_err(errno_error)?;
                parent.sync()?;
                fsync(&namespace).map_err(errno_error)?;
            }
        }
    }

    if statat(&namespace, "root", AtFlags::SYMLINK_NOFOLLOW).is_ok() {
        let root = File::from(
            openat(
                &namespace,
                "root",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(errno_error)?,
        );
        namespace_guard()?;
        verify_directory_binding(&root, &namespace, OsStr::new("root"), &action.identity)?;
        remove_bound_directory_contents(
            &root,
            &namespace,
            OsStr::new("root"),
            &action.identity,
            &namespace_guard,
        )?;
        namespace_guard()?;
        verify_directory_binding(&root, &namespace, OsStr::new("root"), &action.identity)?;
        let captured_root = final_delete_name("quarantine-root");
        renameat_with(
            &namespace,
            "root",
            &namespace,
            &captured_root,
            RenameFlags::NOREPLACE,
        )
        .map_err(errno_error)?;
        fsync(&namespace).map_err(errno_error)?;
        namespace_guard()?;
        let final_root =
            statat(&namespace, &captured_root, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if final_root.st_dev as u64 != action.identity.dev
            || final_root.st_ino != action.identity.ino
            || final_root.st_uid != action.identity.uid
        {
            return Err(invalid(format!(
                "Setup quarantine final capture changed; ambiguous root was preserved as {}",
                deletion.namespace.path.join(&captured_root).display()
            )));
        }
        unlinkat(&namespace, &captured_root, AtFlags::REMOVEDIR).map_err(errno_error)?;
        fsync(&namespace).map_err(errno_error)?;
    }

    namespace_guard()?;
    if !directory_entry_names(&namespace)?.is_empty() {
        return Err(invalid(
            "Setup deletion namespace contains unexpected data and was preserved",
        ));
    }
    let captured_namespace = final_delete_name("quarantine-namespace");
    renameat_with(
        &parent.descriptor,
        namespace_name,
        &parent.descriptor,
        &captured_namespace,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    parent.sync()?;
    let final_namespace = statat(
        &parent.descriptor,
        &captured_namespace,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(errno_error)?;
    if final_namespace.st_dev as u64 != deletion.namespace.identity.dev
        || final_namespace.st_ino != deletion.namespace.identity.ino
        || final_namespace.st_uid != deletion.namespace.identity.uid
    {
        return Err(invalid(format!(
            "Setup deletion namespace changed during final capture; it was preserved as {}",
            parent.path.join(&captured_namespace).display()
        )));
    }
    unlinkat(&parent.descriptor, &captured_namespace, AtFlags::REMOVEDIR).map_err(errno_error)?;
    parent.sync()?;
    validate_resumed_deletion(action)
}

fn journal_archive_path(home: &Path, transaction_id: &str) -> PathBuf {
    home.join("setup/transactions")
        .join(format!("{transaction_id}.jsonl"))
}

fn verify_persisted_rollback(home: &Path, transaction_id: &str) -> Result<()> {
    let recovery = load_active_recovery(home)?;
    if recovery.transaction_id != transaction_id {
        return Err(invalid(
            "Setup rollback belongs to a different recovery transaction",
        ));
    }
    recovery.snapshots.verify_rollback_complete()
}

fn begin_finalization(
    recovery_path: &Path,
    transaction_id: &str,
    outcome: &str,
    journal_destination: &Path,
) -> Result<()> {
    let mut state = read_recovery_state(&recovery_path.join(RECOVERY_STATE_FILE))?;
    if state.transaction_id != transaction_id || state.phase != RecoveryPhase::Active {
        return Err(invalid("Setup recovery cannot begin finalization"));
    }
    if state.quarantine.is_some() {
        return Err(invalid(
            "Setup recovery cannot begin ordinary finalization with an unresolved quarantine",
        ));
    }
    validate_finalization_destination(&state, journal_destination)?;
    state.phase = RecoveryPhase::Finalizing {
        outcome: outcome.to_owned(),
        journal_destination: journal_destination.to_path_buf(),
    };
    write_recovery_state(recovery_path, &state)
}

fn begin_resumed_deletion_finalization(
    recovery_path: &Path,
    transaction_id: &str,
    outcome: &str,
    journal_destination: &Path,
    action: &QuarantineAction,
) -> Result<()> {
    let mut state = read_recovery_state(&recovery_path.join(RECOVERY_STATE_FILE))?;
    if state.transaction_id != transaction_id || state.phase != RecoveryPhase::Active {
        return Err(invalid(
            "Setup recovery cannot begin data-removal finalization",
        ));
    }
    if state.quarantine.as_ref() != Some(action)
        || action.recovery != QuarantineRecovery::ResumeDeletion
        || action.state != QuarantineState::Applied
    {
        return Err(invalid(
            "Setup recovery does not contain the committed data-removal quarantine",
        ));
    }
    validate_finalization_destination(&state, journal_destination)?;
    state.phase = RecoveryPhase::Finalizing {
        outcome: outcome.to_owned(),
        journal_destination: journal_destination.to_path_buf(),
    };
    write_recovery_state(recovery_path, &state)
}

fn validate_finalization_destination(state: &RecoveryState, destination: &Path) -> Result<()> {
    let archived = journal_archive_path(&state.home, &state.transaction_id);
    if destination != state.journal_path && destination != archived {
        return Err(invalid("Setup recovery journal destination is invalid"));
    }
    Ok(())
}

fn validate_journal_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
    identity: &FileIdentity,
    allowed_links: u64,
) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || !same_identity(metadata, identity)
        || metadata.nlink() != allowed_links
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(invalid(format!(
            "Setup journal is unsafe or changed: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_journal(
    path: &Path,
    identity: &FileIdentity,
    append: bool,
    allowed_links: u64,
) -> Result<File> {
    let before = fs::symlink_metadata(path)?;
    validate_journal_metadata(path, &before, identity, allowed_links)?;
    let mut options = OpenOptions::new();
    options
        .read(!append)
        .write(append)
        .append(append)
        .custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    validate_journal_metadata(path, &opened, identity, allowed_links)?;
    if !same_file_state(&before, &opened) || !same_file_state(&opened, &current) {
        return Err(invalid("Setup journal changed while it was opened"));
    }
    Ok(file)
}

fn ensure_journal_archive_directory(home: &Path) -> Result<PathBuf> {
    let home_metadata = fs::symlink_metadata(home)?;
    if home_metadata.file_type().is_symlink()
        || !home_metadata.is_dir()
        || home_metadata.uid() != geteuid().as_raw()
        || home_metadata.mode() & 0o022 != 0
    {
        return Err(invalid("Hardknock home is unsafe for journal archival"));
    }
    let setup = home.join("setup");
    ensure_private_directory_component(&setup)?;
    let transactions = setup.join("transactions");
    ensure_private_directory_component(&transactions)?;
    Ok(transactions)
}

fn ensure_private_directory_component(path: &Path) -> Result<()> {
    ensure_directory_tree(path, true, &mut UnobservedMutation)
}

fn archive_journal(source: &Path, destination: &Path, identity: &FileIdentity) -> Result<()> {
    if source == destination {
        let file = open_journal(source, identity, false, 1)?;
        file.sync_all()?;
        sync_directory(
            source
                .parent()
                .ok_or_else(|| invalid("Setup journal has no parent"))?,
        )?;
        return Ok(());
    }
    let destination_parent = destination
        .parent()
        .ok_or_else(|| invalid("Setup journal destination has no parent"))?;
    let home = destination_parent
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("Setup journal destination has no home"))?;
    if ensure_journal_archive_directory(home)? != destination_parent {
        return Err(invalid("Setup journal destination escaped its archive"));
    }

    let source_state = fs::symlink_metadata(source);
    let destination_state = fs::symlink_metadata(destination);
    match (source_state, destination_state) {
        (Ok(source_metadata), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_journal_metadata(source, &source_metadata, identity, 1)?;
            open_journal(source, identity, false, 1)?.sync_all()?;
            fs::hard_link(source, destination)?;
            sync_directory(destination_parent)?;
            let source_linked = fs::symlink_metadata(source)?;
            let destination_linked = fs::symlink_metadata(destination)?;
            validate_journal_metadata(source, &source_linked, identity, 2)?;
            validate_journal_metadata(destination, &destination_linked, identity, 2)?;
            fs::remove_file(source)?;
            sync_directory(
                source
                    .parent()
                    .ok_or_else(|| invalid("Setup journal has no parent"))?,
            )?;
        }
        (Ok(source_metadata), Ok(destination_metadata)) => {
            validate_journal_metadata(source, &source_metadata, identity, 2)?;
            validate_journal_metadata(destination, &destination_metadata, identity, 2)?;
            fs::remove_file(source)?;
            sync_directory(
                source
                    .parent()
                    .ok_or_else(|| invalid("Setup journal has no parent"))?,
            )?;
        }
        (Err(error), Ok(destination_metadata)) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_journal_metadata(destination, &destination_metadata, identity, 1)?;
        }
        (Err(source_error), Err(destination_error)) => {
            return Err(invalid(format!(
                "Setup journal is unavailable at both recovery paths: {source_error}; {destination_error}"
            )));
        }
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(error.into()),
    }
    let archived = open_journal(destination, identity, false, 1)?;
    archived.sync_all()?;
    sync_directory(destination_parent)
}

fn append_finalization_event(
    journal_path: &Path,
    identity: &FileIdentity,
    transaction_id: &str,
    operation: &str,
    outcome: &str,
) -> Result<()> {
    let mut journal = open_journal(journal_path, identity, true, 1)?;
    serde_json::to_writer(
        &mut journal,
        &json!({
            "schema": JOURNAL_SCHEMA,
            "transaction_id": transaction_id,
            "at": Utc::now(),
            "event": "finished",
            "details": {"outcome":outcome,"operation":operation}
        }),
    )?;
    journal.write_all(b"\n")?;
    journal.sync_all()?;
    Ok(())
}

fn complete_finalization(
    recovery_path: &Path,
    journal_path: &Path,
    journal_identity: &FileIdentity,
    journal_destination: &Path,
    transaction_id: &str,
    operation: &str,
    outcome: &str,
) -> Result<PathBuf> {
    archive_journal(journal_path, journal_destination, journal_identity)?;
    append_finalization_event(
        journal_destination,
        journal_identity,
        transaction_id,
        operation,
        outcome,
    )?;
    tombstone_recovery_directory(recovery_path, transaction_id)?;
    Ok(journal_destination.to_path_buf())
}

fn tombstone_recovery_directory(recovery_path: &Path, transaction_id: &str) -> Result<()> {
    validate_recovery_directory(recovery_path)?;
    let parent = recovery_path
        .parent()
        .ok_or_else(|| invalid("Setup recovery directory has no parent"))?;
    let tombstone = parent.join(format!(
        ".hardknock-setup-{transaction_id}.recovery.finalized-{}",
        uuid::Uuid::new_v4()
    ));
    rename_no_replace(recovery_path, &tombstone)?;
    if let Err(error) = sync_directory(parent) {
        let _ = rename_no_replace(&tombstone, recovery_path);
        let _ = sync_directory(parent);
        return Err(error);
    }
    let _ = cleanup_recovery_directory(&tombstone);
    Ok(())
}

fn recovery_path(home: &Path) -> Result<PathBuf> {
    if !home.is_absolute() {
        return Err(invalid(
            "Hardknock home must be absolute for setup recovery",
        ));
    }
    let parent = home
        .parent()
        .ok_or_else(|| invalid("Hardknock home has no parent for setup recovery"))?;
    let identity = blake3::hash(home.as_os_str().as_bytes()).to_hex();
    Ok(parent.join(format!(".hardknock-setup-{}.recovery", &identity[..24])))
}

fn sync_directory(path: &Path) -> Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)?
        .sync_all()?;
    Ok(())
}

fn validate_recovery_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(invalid(format!(
            "Setup recovery directory is unsafe: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_recovery_file(
    path: &Path,
    metadata: &std::fs::Metadata,
    expected_len: u64,
) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() != expected_len
    {
        return Err(invalid(format!(
            "Setup recovery file is unsafe or changed: {}",
            path.display()
        )));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, expected_len: u64) -> Result<Vec<u8>> {
    if expected_len > MAX_SNAPSHOT_BYTES {
        return Err(invalid("Setup recovery blob exceeds its size limit"));
    }
    let before = fs::symlink_metadata(path)?;
    validate_recovery_file(path, &before, expected_len)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if !same_file_state(&before, &opened) {
        return Err(invalid("Setup recovery blob changed while it was opened"));
    }
    let mut bytes = Vec::with_capacity(expected_len.min(64 * 1024) as usize);
    Read::by_ref(&mut file)
        .take(expected_len.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected_len {
        return Err(invalid("Setup recovery blob length changed while reading"));
    }
    let after = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !same_file_state(&opened, &after) || !same_file_state(&after, &current) {
        return Err(invalid("Setup recovery blob changed while it was read"));
    }
    Ok(bytes)
}

fn read_recovery_state(path: &Path) -> Result<RecoveryState> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.uid() != geteuid().as_raw()
        || before.nlink() != 1
        || before.mode() & 0o777 != 0o600
        || before.len() > MAX_RECOVERY_STATE_BYTES
    {
        return Err(invalid("Setup recovery state is unsafe or oversized"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if !same_file_state(&before, &opened) {
        return Err(invalid("Setup recovery state changed while it was opened"));
    }
    let mut bytes = Vec::with_capacity(before.len().min(64 * 1024) as usize);
    Read::by_ref(&mut file)
        .take(MAX_RECOVERY_STATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECOVERY_STATE_BYTES {
        return Err(invalid("Setup recovery state exceeds its size limit"));
    }
    let after = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !same_file_state(&opened, &after) || !same_file_state(&after, &current) {
        return Err(invalid("Setup recovery state changed while it was read"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn cleanup_recovery_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => validate_recovery_directory(path)?,
    }
    let blobs = path.join(RECOVERY_BLOBS_DIRECTORY);
    match fs::symlink_metadata(&blobs) {
        Ok(_) => {
            validate_recovery_directory(&blobs)?;
            for entry in fs::read_dir(&blobs)? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                validate_recovery_file(&entry.path(), &metadata, metadata.len())?;
                fs::remove_file(entry.path())?;
            }
            fs::remove_dir(&blobs)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        validate_recovery_file(&entry.path(), &metadata, metadata.len())?;
        fs::remove_file(entry.path())?;
    }
    fs::remove_dir(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn validate_recovery_state_identity(home: &Path, state: &RecoveryState) -> Result<()> {
    if state.schema != RECOVERY_SCHEMA
        || state.home != home
        || !matches!(
            state.operation.as_str(),
            "setup" | "upgrade" | "repair" | "uninstall"
        )
        || uuid::Uuid::parse_str(&state.transaction_id).is_err()
        || state.files.len() > MAX_SNAPSHOT_FILES
    {
        return Err(invalid("Setup recovery state identity is invalid"));
    }
    let expected_journal = home
        .parent()
        .ok_or_else(|| invalid("Hardknock home has no setup recovery parent"))?
        .join(format!(
            ".hardknock-setup-{}.journal.jsonl",
            state.transaction_id
        ));
    if state.journal_path != expected_journal {
        return Err(invalid("Setup recovery journal identity is invalid"));
    }
    if let RecoveryPhase::Finalizing {
        journal_destination,
        ..
    } = &state.phase
    {
        validate_finalization_destination(state, journal_destination)?;
    }
    if let Some(action) = &state.quarantine {
        validate_quarantine_paths(home, &action.original, &action.quarantine)?;
        if let Some(deletion) = &action.deletion {
            validate_quarantine_deletion(action, deletion)?;
        } else if matches!(state.phase, RecoveryPhase::Finalizing { .. })
            && action.recovery == QuarantineRecovery::ResumeDeletion
        {
            return Err(invalid(
                "Finalizing data removal has no recorded private deletion namespace",
            ));
        }
    }
    Ok(())
}

fn validate_recovered_expected_state(path: &Path, expected: &ExpectedState) -> Result<()> {
    let validate_namespace = |namespace: &PrivateNamespace| -> Result<()> {
        if namespace.path.parent() != path.parent() {
            return Err(invalid(
                "Private mutation namespace escaped the managed target parent",
            ));
        }
        let name = namespace
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("Private mutation namespace name is invalid"))?;
        if !name.starts_with(".hardknock-") || name.contains('/') {
            return Err(invalid(
                "Private mutation namespace does not use a reserved name",
            ));
        }
        Ok(())
    };
    let validate_entry = |entry: &PrivateEntry| -> Result<()> {
        validate_namespace(&entry.namespace)?;
        if entry.path != entry.namespace.path.join("entry") {
            return Err(invalid(
                "Private mutation entry escaped its recorded namespace",
            ));
        }
        Ok(())
    };
    match expected {
        ExpectedState::Exact {
            pending_cleanup: Some(cleanup),
            ..
        } => {
            validate_namespace(&cleanup.namespace)?;
            if let Some(entry) = &cleanup.entry {
                validate_entry(entry)?;
                if entry.namespace != cleanup.namespace {
                    return Err(invalid(
                        "Private cleanup entry belongs to another namespace",
                    ));
                }
            }
        }
        ExpectedState::PlannedWrite { staged, .. } => validate_entry(staged)?,
        ExpectedState::PlannedRemoval { captured, .. } => validate_entry(captured)?,
        _ => {}
    }
    if let ExpectedState::MutableCreation {
        staging_path: Some(staging_path),
        ..
    } = expected
    {
        if staging_path.parent() != path.parent() {
            return Err(invalid(
                "Mutable creation staging path escaped the target parent",
            ));
        }
        let name = staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("Mutable creation staging path is invalid"))?;
        if !name.starts_with(".hardknock-mutable-") || !name.ends_with(".staging") {
            return Err(invalid(
                "Mutable creation staging path does not use its reserved name",
            ));
        }
    }
    Ok(())
}

fn load_active_recovery(home: &Path) -> Result<UnfinishedTransaction> {
    let recovery_path = recovery_path(home)?;
    validate_recovery_directory(&recovery_path)?;
    let state = read_recovery_state(&recovery_path.join(RECOVERY_STATE_FILE))?;
    validate_recovery_state_identity(home, &state)?;
    if state.phase != RecoveryPhase::Active {
        return Err(invalid("Setup recovery is already finalizing"));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    let mut files = Vec::with_capacity(state.files.len());
    for recovered in state.files.clone() {
        if !recovered.path.is_absolute() || !seen.insert(recovered.path.clone()) {
            return Err(invalid("Setup recovery contains an invalid managed path"));
        }
        validate_recovered_expected_state(&recovered.path, &recovered.expected)?;
        let previous = match recovered.previous {
            RecoveryPrevious::Missing => PreviousFile::Missing,
            RecoveryPrevious::Present {
                blob,
                mode,
                len,
                content_blake3,
            } => {
                if !blob.ends_with(".bin")
                    || blob.contains('/')
                    || blob.contains('\\')
                    || len > MAX_SNAPSHOT_BYTES
                {
                    return Err(invalid("Setup recovery blob identity is invalid"));
                }
                total = total.saturating_add(len);
                if total > MAX_SNAPSHOT_TOTAL_BYTES {
                    return Err(invalid("Setup recovery snapshots exceed their size limit"));
                }
                let bytes = read_bounded_file(
                    &recovery_path.join(RECOVERY_BLOBS_DIRECTORY).join(&blob),
                    len,
                )?;
                if blake3::hash(&bytes).to_hex().as_str() != content_blake3 {
                    return Err(invalid("Setup recovery snapshot checksum mismatch"));
                }
                PreviousFile::Present { bytes, mode }
            }
        };
        files.push(FileSnapshot {
            path: recovered.path,
            previous,
            expected: recovered.expected,
        });
    }
    if state.directories.len() > MAX_DIRECTORY_SNAPSHOTS
        || state
            .directories
            .iter()
            .map(|directory| directory.entries.len())
            .sum::<usize>()
            > MAX_DIRECTORY_ENTRIES
    {
        return Err(invalid(
            "Setup recovery runtime inventory exceeds its safety limit",
        ));
    }
    let directory_paths = state
        .directories
        .iter()
        .map(|directory| directory.path.clone())
        .collect::<BTreeSet<_>>();
    if directory_paths.len() != state.directories.len() {
        return Err(invalid(
            "Setup recovery contains duplicate parent directories",
        ));
    }
    let mut seen_entries = BTreeSet::new();
    for directory in &state.directories {
        let is_managed_ancestor = files
            .iter()
            .any(|file| file.path.starts_with(&directory.path));
        if !directory.path.is_absolute()
            || directory.path.parent().is_none()
            || (!directory.path.starts_with(home) && !is_managed_ancestor)
            || directory.identity.is_some_and(|identity| {
                identity.uid != geteuid().as_raw()
                    || FileType::from_raw_mode(identity.mode as _) != FileType::Directory
                    || identity.mode & 0o022 != 0
            })
            || (!directory.owns_contents && !directory.entries.is_empty())
        {
            return Err(invalid(
                "Setup recovery contains an invalid parent directory",
            ));
        }
        for entry in &directory.entries {
            let entry_type = FileType::from_raw_mode(entry.identity.mode as _);
            if !entry.path.is_absolute()
                || entry.path.parent() != Some(directory.path.as_path())
                || directory_paths.contains(&entry.path)
                || !seen_entries.insert(entry.path.clone())
                || entry.identity.uid != geteuid().as_raw()
                || !matches!(entry_type, FileType::RegularFile | FileType::Socket)
                || entry.identity.mode & 0o022 != 0
            {
                return Err(invalid(
                    "Setup recovery contains an invalid transaction-owned runtime file",
                ));
            }
        }
    }
    let mut directories = state
        .directories
        .into_iter()
        .map(|directory| DirectorySnapshot {
            path: directory.path,
            identity: directory.identity,
            owns_contents: directory.owns_contents,
            entries: directory.entries,
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|directory| std::cmp::Reverse(directory.path.components().count()));
    Ok(UnfinishedTransaction {
        operation: state.operation,
        initial_home: state.initial_home,
        snapshots: SnapshotSet { files, directories },
        recovery_path,
        journal_path: state.journal_path,
        journal_identity: state.journal_identity,
        quarantine: state.quarantine,
        transaction_id: state.transaction_id,
    })
}

pub(crate) fn unfinished(home: &Path) -> Result<Option<UnfinishedTransaction>> {
    let recovery_path = recovery_path(home)?;
    match fs::symlink_metadata(&recovery_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => validate_recovery_directory(&recovery_path)?,
    }
    let state = read_recovery_state(&recovery_path.join(RECOVERY_STATE_FILE))?;
    validate_recovery_state_identity(home, &state)?;
    if let RecoveryPhase::Finalizing {
        outcome,
        journal_destination,
    } = &state.phase
    {
        if let Some(action) = &state.quarantine {
            if action.recovery != QuarantineRecovery::ResumeDeletion
                || action.state != QuarantineState::Applied
            {
                return Err(invalid(
                    "Finalizing setup recovery contains an invalid quarantine action",
                ));
            }
            delete_resumed_quarantine(action)?;
        }
        complete_finalization(
            &recovery_path,
            &state.journal_path,
            &state.journal_identity,
            journal_destination,
            &state.transaction_id,
            &state.operation,
            outcome,
        )?;
        return Ok(None);
    }
    load_active_recovery(home).map(Some)
}

impl UnfinishedTransaction {
    pub(crate) fn operation(&self) -> &str {
        &self.operation
    }

    pub(crate) fn initial_home(&self) -> InitialHome {
        self.initial_home
    }

    #[allow(dead_code)] // Read by setup recovery before choosing restore or deletion.
    pub(crate) fn quarantine_action(&self) -> Option<&QuarantineAction> {
        self.quarantine.as_ref()
    }

    #[allow(dead_code)] // Used when recovery observes that the planned rename completed.
    pub(crate) fn checkpoint_quarantine_applied(&mut self) -> Result<()> {
        let mut action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        validate_applied_quarantine(&action)?;
        action.state = QuarantineState::Applied;
        update_recovery_quarantine(
            &self.recovery_path,
            &self.transaction_id,
            Some(action.clone()),
        )?;
        self.quarantine = Some(action);
        Ok(())
    }

    #[allow(dead_code)] // Used after a crash between quarantine preparation and rename.
    pub(crate) fn cancel_planned_quarantine(&mut self) -> Result<()> {
        let action = self
            .quarantine
            .as_ref()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        validate_planned_quarantine_not_applied(action)?;
        update_recovery_quarantine(&self.recovery_path, &self.transaction_id, None)?;
        self.quarantine = None;
        Ok(())
    }

    #[allow(dead_code)] // Used by rollback-oriented quarantine recovery.
    pub(crate) fn restore_quarantine(&mut self) -> Result<()> {
        let action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine action was prepared"))?;
        if action.recovery != QuarantineRecovery::Restore {
            return Err(invalid(
                "Setup quarantine policy requires deletion rather than restoration",
            ));
        }
        validate_applied_quarantine(&action)?;
        rename_no_replace(&action.quarantine, &action.original)?;
        sync_directory(
            action
                .original
                .parent()
                .ok_or_else(|| invalid("Setup quarantine has no parent"))?,
        )?;
        let restored = fs::symlink_metadata(&action.original)?;
        if !same_identity(&restored, &action.identity) {
            return Err(invalid("Restored Hardknock home identity changed"));
        }
        update_recovery_quarantine(&self.recovery_path, &self.transaction_id, None)?;
        self.quarantine = None;
        Ok(())
    }

    #[allow(dead_code)] // Called after restore or resumed deletion reaches its durable end state.
    pub(crate) fn checkpoint_quarantine_resolved(&mut self) -> Result<()> {
        let action = self
            .quarantine
            .as_ref()
            .ok_or_else(|| invalid("No setup quarantine action requires resolution"))?;
        let original = fs::symlink_metadata(&action.original);
        let quarantine = fs::symlink_metadata(&action.quarantine);
        let resolved = match action.recovery {
            QuarantineRecovery::Restore => {
                original
                    .as_ref()
                    .is_ok_and(|metadata| same_identity(metadata, &action.identity))
                    && quarantine
                        .as_ref()
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            }
            QuarantineRecovery::ResumeDeletion => {
                return Err(invalid(
                    "Resume-deletion quarantine must use finish_resumed_deletion",
                ));
            }
        };
        if !resolved {
            return Err(invalid(
                "Setup quarantine has not reached its configured recovery state",
            ));
        }
        update_recovery_quarantine(&self.recovery_path, &self.transaction_id, None)?;
        self.quarantine = None;
        Ok(())
    }

    #[allow(dead_code)] // Completes remove-data recovery after the quarantine is deleted.
    pub(crate) fn finish_resumed_deletion(mut self, outcome: &str) -> Result<PathBuf> {
        let mut action = self
            .quarantine
            .clone()
            .ok_or_else(|| invalid("No setup quarantine deletion requires finalization"))?;
        prepare_quarantine_deletion(&self.recovery_path, &self.transaction_id, &mut action)?;
        self.quarantine = Some(action.clone());
        let destination = self.journal_path.clone();
        begin_resumed_deletion_finalization(
            &self.recovery_path,
            &self.transaction_id,
            outcome,
            &destination,
            &action,
        )?;
        delete_resumed_quarantine(&action)?;
        complete_finalization(
            &self.recovery_path,
            &self.journal_path,
            &self.journal_identity,
            &destination,
            &self.transaction_id,
            &self.operation,
            outcome,
        )
    }

    pub(crate) fn rollback_files(&self) -> Result<()> {
        if self.quarantine.is_some() {
            return Err(invalid(
                "Resolve the pending setup quarantine action before file rollback",
            ));
        }
        self.snapshots
            .rollback_for_transaction(&self.transaction_id, |_| {})
    }

    pub(crate) fn finish(self, home: &Path, outcome: &str) -> Result<PathBuf> {
        if self.quarantine.is_some() {
            return Err(invalid(
                "Setup recovery has an unresolved quarantine action; recovery was preserved",
            ));
        }
        self.snapshots.verify_rollback_complete()?;
        let destination = if self.initial_home == InitialHome::Existing && home.is_dir() {
            journal_archive_path(home, &self.transaction_id)
        } else {
            self.journal_path.clone()
        };
        begin_finalization(
            &self.recovery_path,
            &self.transaction_id,
            outcome,
            &destination,
        )?;
        complete_finalization(
            &self.recovery_path,
            &self.journal_path,
            &self.journal_identity,
            &destination,
            &self.transaction_id,
            &self.operation,
            outcome,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestObserver<'a> {
        snapshots: &'a mut SnapshotSet,
        journal: &'a Journal,
        fail_receipt: bool,
    }

    impl MutationObserver for TestObserver<'_> {
        fn prepare_write(&mut self, intent: PreparedWrite) -> Result<()> {
            self.snapshots.prepare_write(self.journal, intent)
        }

        fn prepare_removal(&mut self, intent: PreparedRemoval) -> Result<()> {
            self.snapshots.prepare_removal(self.journal, intent)
        }

        fn record_write(&mut self, receipt: WriteReceipt) -> Result<()> {
            if self.fail_receipt {
                Err(invalid("simulated crash before write receipt"))
            } else {
                self.snapshots.checkpoint_receipt(self.journal, receipt)
            }
        }

        fn record_removal(&mut self, receipt: WriteReceipt) -> Result<()> {
            if self.fail_receipt {
                Err(invalid("simulated crash before removal receipt"))
            } else {
                self.snapshots.checkpoint_receipt(self.journal, receipt)
            }
        }

        fn abort_mutation(&mut self, path: &Path, cleanup: PrivateCleanup) -> Result<()> {
            self.snapshots.abort_mutation(self.journal, path, cleanup)
        }

        fn record_directory(&mut self, receipt: DirectoryReceipt) -> Result<()> {
            self.snapshots.record_directory(self.journal, receipt)
        }
    }

    #[test]
    fn rollback_restores_existing_files_and_removes_new_files() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let existing = temporary.path().join("existing.json");
        let created = temporary.path().join("new/deep/created.json");
        fs::write(&existing, b"before").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![existing.clone(), created.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Missing,
            &snapshots,
        )
        .unwrap();

        fs::write(&existing, b"after").unwrap();
        {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: false,
            };
            ensure_directory_tree(created.parent().unwrap(), true, &mut observer).unwrap();
        }
        fs::write(&created, b"created").unwrap();
        snapshots
            .checkpoint(&journal, vec![existing.clone(), created.clone()])
            .unwrap();
        snapshots.rollback(&journal).unwrap();
        journal.finish_beside_home("rolled_back").unwrap();

        assert_eq!(fs::read(&existing).unwrap(), b"before");
        assert_eq!(
            fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!created.exists());
        assert!(!temporary.path().join("new").exists());
    }

    #[test]
    fn rollback_preserves_files_changed_after_checkpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let path = temporary.path().join("managed.json");
        fs::write(&path, b"before").unwrap();
        let mut snapshots = SnapshotSet::capture(vec![path.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Missing,
            &snapshots,
        )
        .unwrap();
        fs::write(&path, b"transaction").unwrap();
        snapshots.checkpoint(&journal, vec![path.clone()]).unwrap();
        fs::write(&path, b"concurrent").unwrap();

        let error = snapshots.rollback(&journal).unwrap_err();
        assert!(error.to_string().contains("preserved current content"));
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
        journal.finish_beside_home("preserved_concurrent").unwrap();
    }

    #[test]
    fn setup_lock_serializes_transactions_without_creating_a_lock_file() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let first = SetupLock::acquire(&home).unwrap();
        let error = SetupLock::acquire_with_timeout(&home, Duration::from_millis(50)).unwrap_err();
        assert!(error.to_string().contains("transaction is active"));
        assert!(fs::read_dir(temporary.path()).unwrap().next().is_none());
        drop(first);
        SetupLock::acquire_with_timeout(&home, Duration::from_millis(50)).unwrap();
    }

    #[test]
    fn snapshot_refuses_symlinks_and_oversized_files() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let link = temporary.path().join("link");
        fs::write(&target, b"value").unwrap();
        symlink(&target, &link).unwrap();
        assert!(SnapshotSet::capture(vec![link]).is_err());

        let oversized = temporary.path().join("oversized");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_SNAPSHOT_BYTES + 1).unwrap();
        assert!(SnapshotSet::capture(vec![oversized]).is_err());
    }

    #[test]
    fn journal_moves_into_private_managed_history() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let temporary_path = journal.path().to_path_buf();
        journal.record("applied", json!({"step":"home"})).unwrap();
        let destination = journal.finish(&home, "succeeded").unwrap();

        assert!(!temporary_path.exists());
        assert!(destination.is_file());
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn unfinished_transaction_restores_private_snapshot_during_repair() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        fs::write(&managed, b"after").unwrap();
        snapshots
            .checkpoint(&journal, vec![managed.clone()])
            .unwrap();
        let journal_path = journal.path().to_path_buf();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        recovery.rollback_files().unwrap();
        let archived = recovery.finish(&home, "rolled_back_by_repair").unwrap();

        assert_eq!(fs::read(managed).unwrap(), b"before");
        assert!(!journal_path.exists());
        assert!(archived.is_file());
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn write_ahead_intent_recovers_a_crash_before_receipt_checkpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();

        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let error = {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: true,
            };
            crate::integrations::install::write_managed_bytes_observed(
                &managed,
                b"after",
                true,
                Some(b"before"),
                &mut observer,
            )
            .unwrap_err()
        };
        assert!(error.to_string().contains("simulated crash"));
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        recovery.rollback_files().unwrap();
        recovery.finish(&home, "rolled_back_by_repair").unwrap();

        assert_eq!(fs::read(managed).unwrap(), b"before");
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn write_ahead_intent_never_adopts_an_independent_same_content_file() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        let transaction_inode = temporary.path().join("transaction-inode");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();

        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: true,
            };
            crate::integrations::install::write_managed_bytes_observed(
                &managed,
                b"after",
                true,
                Some(b"before"),
                &mut observer,
            )
            .unwrap_err();
        }
        fs::rename(&managed, &transaction_inode).unwrap();
        fs::write(&managed, b"after").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        let error = recovery.rollback_files().unwrap_err();

        assert!(error.to_string().contains("preserved current content"));
        assert_eq!(fs::read(&managed).unwrap(), b"after");
        assert_eq!(fs::read(&transaction_inode).unwrap(), b"after");
        assert!(unfinished(&home).unwrap().is_some());
    }

    #[test]
    fn planned_removal_requires_the_recorded_captured_inode() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();

        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let parent = BoundDirectory::open(temporary.path()).unwrap();
        let current = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&managed)
            .unwrap();
        let namespace =
            PrivateMutationDirectory::create(&parent.descriptor, temporary.path(), "test-remove")
                .unwrap();
        snapshots
            .prepare_removal(
                &journal,
                namespace
                    .planned_removal(&managed, &current, b"before")
                    .unwrap(),
            )
            .unwrap();
        fs::remove_file(&managed).unwrap();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        let error = recovery.rollback_files().unwrap_err();

        assert!(error.to_string().contains("preserved current content"));
        assert!(!managed.exists());
        assert!(namespace.namespace.path.exists());
        assert!(unfinished(&home).unwrap().is_some());
    }

    #[test]
    fn rollback_finalization_refuses_incomplete_restoration_and_preserves_recovery() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();

        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: true,
            };
            crate::integrations::install::write_managed_bytes_observed(
                &managed,
                b"transaction",
                true,
                Some(b"before"),
                &mut observer,
            )
            .unwrap_err();
        }

        let error = journal.finish_after_rollback(&home).unwrap_err();
        assert!(error.to_string().contains("rollback is incomplete"));
        assert_eq!(fs::read(&managed).unwrap(), b"transaction");
        assert!(unfinished(&home).unwrap().is_some());
    }

    #[test]
    fn finalizing_recovery_is_resumed_without_rolling_back_committed_files() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let destination = journal_archive_path(&home, &journal.transaction_id);
        begin_finalization(
            &journal.recovery_path,
            &journal.transaction_id,
            "succeeded",
            &destination,
        )
        .unwrap();
        drop(journal);

        assert!(unfinished(&home).unwrap().is_none());
        assert!(destination.is_file());
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn recovery_bundle_is_published_without_exposing_a_staging_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();

        assert!(journal.recovery_path.is_dir());
        assert_eq!(
            fs::metadata(&journal.recovery_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            fs::read_dir(temporary.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".staging"))
        );
        drop(journal);
    }

    #[test]
    fn recovered_journal_symlink_is_rejected_without_losing_recovery() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let journal_path = journal.temporary_path.clone();
        drop(journal);
        let recovery = unfinished(&home).unwrap().unwrap();

        fs::remove_file(&journal_path).unwrap();
        let victim = temporary.path().join("victim");
        fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, &journal_path).unwrap();
        let error = recovery.finish(&home, "rolled_back_by_repair").unwrap_err();

        assert!(error.to_string().contains("journal is unsafe"));
        assert_eq!(fs::read(&victim).unwrap(), b"unchanged");
        assert!(recovery_path(&home).unwrap().is_dir());
    }

    #[test]
    fn mutable_creation_tracks_inode_while_allowing_sqlite_style_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let database = home.join("hardknock.db-wal");
        let mut snapshots = SnapshotSet::capture(vec![database.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        let mut file = snapshots.create_mutable_file(&journal, &database).unwrap();
        file.write_all(b"sqlite changed length and timestamps")
            .unwrap();
        file.sync_all().unwrap();

        snapshots.rollback(&journal).unwrap();
        snapshots.verify_rollback_complete().unwrap();
        assert!(!database.exists());
        journal.finish_after_rollback(&home).unwrap();
    }

    #[test]
    fn recovery_preserves_mutable_staging_without_a_durable_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let database = home.join("hardknock.db-shm");
        let staging = home.join(format!(
            ".hardknock-mutable-{}.staging",
            uuid::Uuid::new_v4()
        ));
        let mut snapshots = SnapshotSet::capture(vec![database.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        snapshots.snapshot_mut(&database).unwrap().expected = ExpectedState::MutableCreation {
            before: FileState::Missing,
            identity: None,
            staging_path: Some(staging.clone()),
        };
        journal.persist_snapshot(&snapshots).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
            .unwrap()
            .sync_all()
            .unwrap();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        let error = recovery.rollback_files().unwrap_err();

        assert!(!database.exists());
        assert!(staging.exists());
        assert!(
            error
                .to_string()
                .contains("identity was never durably recorded")
        );
        assert!(unfinished(&home).unwrap().is_some());
    }

    #[test]
    fn planned_creation_does_not_claim_an_unrecorded_concurrent_file() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let database = home.join("hardknock.db");
        let mut snapshots = SnapshotSet::capture(vec![database.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        snapshots.prepare_creation(&journal, &database).unwrap();
        fs::write(&database, b"concurrent").unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

        let error = snapshots.rollback(&journal).unwrap_err();
        assert!(error.to_string().contains("preserved current content"));
        assert_eq!(fs::read(&database).unwrap(), b"concurrent");
        drop(journal);
        assert!(unfinished(&home).unwrap().is_some());
    }

    #[test]
    fn descriptor_bound_directory_creation_rejects_a_symlink_replacement() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("managed");
        let victim = temporary.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), b"keep").unwrap();
        let mut observer = UnobservedMutation;

        let error = ensure_directory_tree_with_hook(&target, true, &mut observer, |created| {
            if created == target {
                fs::remove_dir(created).unwrap();
                symlink(&victim, created).unwrap();
            }
        })
        .unwrap_err();

        assert!(error.to_string().contains("directory"));
        assert_eq!(fs::read(victim.join("keep")).unwrap(), b"keep");
        assert!(
            fs::symlink_metadata(target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn rollback_preserves_a_replaced_transaction_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("created/managed.json");
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: false,
            };
            ensure_directory_tree(managed.parent().unwrap(), true, &mut observer).unwrap();
        }
        let displaced = temporary.path().join("created.transaction");
        fs::rename(managed.parent().unwrap(), &displaced).unwrap();
        fs::create_dir(managed.parent().unwrap()).unwrap();
        fs::set_permissions(managed.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();

        let error = snapshots.rollback(&journal).unwrap_err();

        assert!(error.to_string().contains("binding changed"));
        assert!(managed.parent().unwrap().is_dir());
        assert!(displaced.is_dir());
        drop(journal);
    }

    #[test]
    fn private_cleanup_preserves_a_late_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = BoundDirectory::open(temporary.path()).unwrap();
        let namespace =
            PrivateMutationDirectory::create(&parent.descriptor, temporary.path(), "test-cleanup")
                .unwrap();
        let expected = namespace.create_file(b"expected", 0o600).unwrap();
        let cleanup = namespace.cleanup_for_entry(&expected).unwrap();
        let moved_expected = temporary.path().join("expected-entry");

        let error = cleanup_private_namespace_with_hook(&cleanup, || {
            fs::rename(namespace.entry_path(), &moved_expected).unwrap();
            fs::write(namespace.entry_path(), b"concurrent").unwrap();
            fs::set_permissions(namespace.entry_path(), fs::Permissions::from_mode(0o600)).unwrap();
        })
        .unwrap_err();

        assert!(error.to_string().contains("unexpected data"));
        assert_eq!(fs::read(moved_expected).unwrap(), b"expected");
        let preserved = fs::read_dir(&namespace.namespace.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| fs::read(path).is_ok_and(|bytes| bytes == b"concurrent"))
            .unwrap();
        assert_eq!(fs::read(preserved).unwrap(), b"concurrent");
    }

    #[test]
    fn recursive_deletion_stops_when_an_ancestor_binding_changes() {
        use std::cell::Cell;

        let temporary = tempfile::tempdir().unwrap();
        let namespace_path = temporary.path().join(".hardknock-remove-delete-test");
        let displaced = temporary.path().join("displaced");
        fs::create_dir(&namespace_path).unwrap();
        fs::set_permissions(&namespace_path, fs::Permissions::from_mode(0o700)).unwrap();
        let root_path = namespace_path.join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root_path.join("child")).unwrap();
        fs::write(root_path.join("child/data"), b"preserve").unwrap();

        let parent = BoundDirectory::open(temporary.path()).unwrap();
        let namespace = File::open(&namespace_path).unwrap();
        let root = File::open(&root_path).unwrap();
        let namespace_identity = file_identity(&namespace.metadata().unwrap());
        let root_identity = file_identity(&root.metadata().unwrap());
        let calls = Cell::new(0_u32);
        let guard = || {
            let call = calls.get() + 1;
            calls.set(call);
            if call == 2 {
                fs::rename(&namespace_path, &displaced).unwrap();
                fs::create_dir(&namespace_path).unwrap();
                fs::set_permissions(&namespace_path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let opened = namespace.metadata()?;
            let named = statat(
                &parent.descriptor,
                namespace_path.file_name().unwrap(),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(errno_error)?;
            if !same_identity(&opened, &namespace_identity)
                || !metadata_matches_stat(&opened, &named)
            {
                return Err(invalid("ancestor binding changed"));
            }
            Ok(())
        };

        let error = remove_bound_directory_contents(
            &root,
            &namespace,
            OsStr::new("root"),
            &root_identity,
            &guard,
        )
        .unwrap_err();

        assert!(error.to_string().contains("ancestor binding changed"));
        assert_eq!(
            fs::read(displaced.join("root/child/data")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn quarantine_state_survives_rename_and_requires_explicit_resolution() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-test");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        assert_eq!(
            recovery.quarantine_action().unwrap().state,
            QuarantineState::Applied
        );
        assert!(recovery.rollback_files().is_err());
        fs::remove_dir(&quarantine).unwrap();
        sync_directory(temporary.path()).unwrap();
        recovery.finish_resumed_deletion("succeeded").unwrap();
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn live_quarantine_deletion_finalizes_without_file_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-live");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        fs::remove_dir(&quarantine).unwrap();
        sync_directory(temporary.path()).unwrap();

        let journal_path = journal.finish_resumed_deletion("succeeded").unwrap();
        assert!(journal_path.is_file());
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn recovery_can_checkpoint_a_quarantine_rename_interrupted_before_receipt() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-interrupted");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::Restore)
            .unwrap();
        rename_no_replace(&home, &quarantine).unwrap();
        sync_directory(temporary.path()).unwrap();
        drop(journal);

        let mut recovery = unfinished(&home).unwrap().unwrap();
        assert_eq!(
            recovery.quarantine_action().unwrap().state,
            QuarantineState::Planned
        );
        recovery.checkpoint_quarantine_applied().unwrap();
        recovery.restore_quarantine().unwrap();
        recovery.rollback_files().unwrap();
        recovery.finish(&home, "rolled_back_by_repair").unwrap();

        assert!(home.is_dir());
        assert!(!quarantine.exists());
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn recovery_can_cancel_a_quarantine_that_was_never_renamed() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-not-applied");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::Restore)
            .unwrap();
        drop(journal);

        let mut recovery = unfinished(&home).unwrap().unwrap();
        recovery.cancel_planned_quarantine().unwrap();
        recovery.rollback_files().unwrap();
        recovery.finish(&home, "rolled_back_by_repair").unwrap();

        assert!(home.is_dir());
        assert!(!quarantine.exists());
        assert!(unfinished(&home).unwrap().is_none());
    }

    #[test]
    fn planned_quarantine_cancellation_fails_closed_when_both_paths_exist() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-ambiguous");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::Restore)
            .unwrap();
        fs::create_dir(&quarantine).unwrap();
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
        drop(journal);

        let mut recovery = unfinished(&home).unwrap().unwrap();
        let error = recovery.cancel_planned_quarantine().unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
        assert!(recovery_path(&home).unwrap().is_dir());
    }

    #[test]
    fn rollback_removal_preserves_a_replacement_swapped_before_atomic_capture() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("created.json");
        let displaced = temporary.path().join("transaction-created.json");
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        fs::write(&managed, b"transaction").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        snapshots
            .checkpoint(&journal, vec![managed.clone()])
            .unwrap();

        let mut swapped = false;
        let error = snapshots
            .rollback_with_hook(&journal, |path| {
                if path == managed && !swapped {
                    swapped = true;
                    fs::rename(&managed, &displaced).unwrap();
                    fs::write(&managed, b"concurrent").unwrap();
                    fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
                }
            })
            .unwrap_err();

        assert!(error.to_string().contains("atomic removal"));
        assert_eq!(fs::read(&managed).unwrap(), b"concurrent");
        assert_eq!(fs::read(&displaced).unwrap(), b"transaction");
        assert!(
            fs::read_dir(temporary.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".hardknock-rollback-"))
        );
        drop(journal);
    }

    #[test]
    fn rollback_cleanup_preserves_a_replacement_before_final_capture() {
        let temporary = tempfile::tempdir().unwrap();
        let display_path = temporary.path().join("managed.json");
        let scratch_name = transaction_scratch_name("transaction", &display_path, "remove");
        let scratch_path = temporary.path().join(&scratch_name);
        let delete_path = temporary.path().join(scratch_delete_name(&scratch_name));
        let moved_expected = temporary.path().join("expected-scratch");
        fs::write(&scratch_path, b"expected").unwrap();
        fs::set_permissions(&scratch_path, fs::Permissions::from_mode(0o600)).unwrap();
        let parent = BoundDirectory::open(temporary.path()).unwrap();
        let expected = inspect_bound_regular(&parent, &scratch_name, &scratch_path)
            .unwrap()
            .unwrap();

        let error = delete_transaction_scratch_with_hook(
            &parent,
            &scratch_name,
            &display_path,
            &|state| file_state_matches_after_rename(state, &expected),
            || {
                fs::rename(&delete_path, &moved_expected).unwrap();
                fs::write(&delete_path, b"concurrent").unwrap();
                fs::set_permissions(&delete_path, fs::Permissions::from_mode(0o600)).unwrap();
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("preserved"));
        assert_eq!(fs::read(&delete_path).unwrap(), b"concurrent");
        assert_eq!(fs::read(moved_expected.join("entry")).unwrap(), b"expected");
    }

    #[test]
    fn rollback_restoration_preserves_a_replacement_swapped_before_exchange() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        let displaced = temporary.path().join("transaction-settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        fs::write(&managed, b"transaction").unwrap();
        snapshots
            .checkpoint(&journal, vec![managed.clone()])
            .unwrap();

        let mut swapped = false;
        let error = snapshots
            .rollback_with_hook(&journal, |path| {
                if path == managed && !swapped {
                    swapped = true;
                    fs::rename(&managed, &displaced).unwrap();
                    fs::write(&managed, b"concurrent").unwrap();
                    fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
                }
            })
            .unwrap_err();

        assert!(error.to_string().contains("concurrent replacement"));
        assert_eq!(fs::read(&managed).unwrap(), b"concurrent");
        assert_eq!(fs::read(&displaced).unwrap(), b"transaction");
        assert!(
            fs::read_dir(temporary.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".hardknock-rollback-"))
        );
        drop(journal);
    }

    #[test]
    fn quarantine_deletion_preserves_a_replacement_swapped_before_capture() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-race");
        let displaced = temporary.path().join("expected-quarantine");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(home.join("managed.db"), b"managed").unwrap();
        let snapshots = SnapshotSet::capture(Vec::<PathBuf>::new()).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        let mut action = journal.quarantine.as_ref().unwrap().clone();
        prepare_quarantine_deletion(&journal.recovery_path, &journal.transaction_id, &mut action)
            .unwrap();

        let error = delete_resumed_quarantine_with_hook(&action, || {
            fs::rename(&quarantine, &displaced).unwrap();
            fs::create_dir(&quarantine).unwrap();
            fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(quarantine.join("concurrent"), b"keep").unwrap();
        })
        .unwrap_err();

        assert!(error.to_string().contains("binding changed"));
        assert_eq!(
            fs::read(action.deletion.as_ref().unwrap().root.join("concurrent")).unwrap(),
            b"keep"
        );
        assert_eq!(fs::read(displaced.join("managed.db")).unwrap(), b"managed");
        assert!(action.deletion.as_ref().unwrap().namespace.path.is_dir());
        drop(journal);
    }

    #[test]
    fn live_remove_data_finalizing_crash_resumes_deletion_without_file_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-live-crash");
        let managed = temporary.path().join("agent-settings.json");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join("nested")).unwrap();
        fs::write(home.join("nested/data"), b"delete").unwrap();
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        fs::write(&managed, b"uninstalled").unwrap();
        snapshots
            .checkpoint(&journal, vec![managed.clone()])
            .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        let mut action = journal.quarantine.as_ref().unwrap().clone();
        prepare_quarantine_deletion(&journal.recovery_path, &journal.transaction_id, &mut action)
            .unwrap();
        journal.quarantine = Some(action.clone());
        let destination = journal.temporary_path.clone();
        begin_resumed_deletion_finalization(
            &journal.recovery_path,
            &journal.transaction_id,
            "succeeded",
            &destination,
            &action,
        )
        .unwrap();
        drop(journal);

        assert!(unfinished(&home).unwrap().is_none());
        assert_eq!(fs::read(&managed).unwrap(), b"uninstalled");
        assert!(!home.exists());
        assert!(!quarantine.exists());
        assert!(!action.deletion.as_ref().unwrap().namespace.path.exists());
        assert!(destination.is_file());
    }

    #[test]
    fn recovered_remove_data_finalizing_crash_resumes_after_deletion_without_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let quarantine = temporary.path().join(".hardknock-remove-recovery-crash");
        let managed = temporary.path().join("agent-settings.json");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(home.join("hardknock.db"), b"delete").unwrap();
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let mut journal = Journal::begin(
            &home,
            "uninstall",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        fs::write(&managed, b"uninstalled").unwrap();
        snapshots
            .checkpoint(&journal, vec![managed.clone()])
            .unwrap();
        journal
            .prepare_quarantine(&home, &quarantine, QuarantineRecovery::ResumeDeletion)
            .unwrap();
        journal.apply_quarantine().unwrap();
        drop(journal);

        let recovery = unfinished(&home).unwrap().unwrap();
        let mut action = recovery.quarantine.as_ref().unwrap().clone();
        prepare_quarantine_deletion(
            &recovery.recovery_path,
            &recovery.transaction_id,
            &mut action,
        )
        .unwrap();
        let destination = recovery.journal_path.clone();
        begin_resumed_deletion_finalization(
            &recovery.recovery_path,
            &recovery.transaction_id,
            "data_removal_completed_by_repair",
            &destination,
            &action,
        )
        .unwrap();
        delete_resumed_quarantine(&action).unwrap();
        drop(recovery);

        assert!(unfinished(&home).unwrap().is_none());
        assert_eq!(fs::read(&managed).unwrap(), b"uninstalled");
        assert!(!home.exists());
        assert!(!quarantine.exists());
        assert!(!action.deletion.as_ref().unwrap().namespace.path.exists());
        assert!(destination.is_file());
    }

    #[test]
    fn rollback_restoration_is_idempotent_without_an_unlink_window() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let managed = temporary.path().join("settings.json");
        fs::write(&managed, b"before").unwrap();
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o600)).unwrap();
        let mut snapshots = SnapshotSet::capture(vec![managed.clone()]).unwrap();
        let journal = Journal::begin(
            &home,
            "setup",
            &json!({"changes":[]}),
            InitialHome::Existing,
            &snapshots,
        )
        .unwrap();
        {
            let mut observer = TestObserver {
                snapshots: &mut snapshots,
                journal: &journal,
                fail_receipt: true,
            };
            crate::integrations::install::write_managed_bytes_observed(
                &managed,
                b"after",
                true,
                Some(b"before"),
                &mut observer,
            )
            .unwrap_err();
        }

        snapshots.rollback(&journal).unwrap();
        snapshots.rollback(&journal).unwrap();
        assert_eq!(fs::read(&managed).unwrap(), b"before");
        journal.finish_after_rollback(&home).unwrap();
    }
}
