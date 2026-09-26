// SPDX-License-Identifier: Apache-2.0

use crate::{Error, Result};
use chrono::Utc;
use fs2::FileExt;
use nix::unistd::geteuid;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const MAX_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FINGERPRINT_BYTES: u64 = 64 * 1024 * 1024;
const SETUP_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_SCHEMA: &str = "hardknock-setup-journal-v1";

#[derive(Debug)]
enum PreviousFile {
    Missing,
    Present { bytes: Vec<u8>, mode: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileState {
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

#[derive(Debug)]
struct FileSnapshot {
    path: PathBuf,
    previous: PreviousFile,
    expected: FileState,
}

#[derive(Debug, Serialize)]
pub(crate) struct SnapshotDescription {
    pub path: PathBuf,
    pub previous: &'static str,
}

#[derive(Debug)]
pub(crate) struct SnapshotSet {
    files: Vec<FileSnapshot>,
    missing_parents: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct SetupLock {
    directory: File,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Intervention(message.into())
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
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
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
    Ok((
        FileState::Present {
            dev: current.dev(),
            ino: current.ino(),
            uid: current.uid(),
            nlink: current.nlink(),
            mode: current.mode(),
            len: current.len(),
            mtime: current.mtime(),
            mtime_nsec: current.mtime_nsec(),
            ctime: current.ctime(),
            ctime_nsec: current.ctime_nsec(),
            content_blake3,
        },
        retained,
    ))
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

impl SnapshotSet {
    pub(crate) fn capture(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let paths = paths.into_iter().collect::<BTreeSet<_>>();
        let mut files = Vec::with_capacity(paths.len());
        let mut missing_parents = BTreeSet::new();
        for path in paths {
            if !path.is_absolute() {
                return Err(invalid(format!(
                    "Managed setup path must be absolute: {}",
                    path.display()
                )));
            }
            missing_parents.extend(validate_parent_chain(&path)?);
            let (previous, expected) = match inspect_file(&path, true)? {
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
            files.push(FileSnapshot {
                path,
                previous,
                expected,
            });
        }
        let mut missing_parents = missing_parents.into_iter().collect::<Vec<_>>();
        missing_parents.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        Ok(Self {
            files,
            missing_parents,
        })
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

    pub(crate) fn checkpoint(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
        for path in paths.into_iter().collect::<BTreeSet<_>>() {
            let snapshot = self
                .files
                .iter_mut()
                .find(|snapshot| snapshot.path == path)
                .ok_or_else(|| {
                    invalid(format!(
                        "Cannot checkpoint a path outside the rollback set: {}",
                        path.display()
                    ))
                })?;
            snapshot.expected = inspect_file(&path, false)?.0;
        }
        Ok(())
    }

    pub(crate) fn rollback(&self) -> Result<()> {
        let mut failures = Vec::new();
        for snapshot in self.files.iter().rev() {
            let current = match inspect_file(&snapshot.path, false) {
                Ok((state, _)) => state,
                Err(error) => {
                    failures.push(format!("{}: {error}", snapshot.path.display()));
                    continue;
                }
            };
            if current != snapshot.expected {
                failures.push(format!(
                    "{}: changed after the setup transaction wrote it; preserved current content",
                    snapshot.path.display()
                ));
                continue;
            }
            let result = match &snapshot.previous {
                PreviousFile::Missing => remove_managed_file(&snapshot.path),
                PreviousFile::Present { bytes, mode } => restore_file(&snapshot.path, bytes, *mode),
            };
            if let Err(error) = result {
                failures.push(format!("{}: {error}", snapshot.path.display()));
            }
        }
        for directory in &self.missing_parents {
            match fs::remove_dir(directory) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => failures.push(format!("{}: {error}", directory.display())),
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
}

fn remove_managed_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == geteuid().as_raw()
                && metadata.nlink() == 1 =>
        {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => Err(invalid(format!(
            "Rollback path changed into a non-file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn restore_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid(format!("Rollback path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent)?;
    remove_managed_file(path)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
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
    temporary_path: PathBuf,
    file: File,
}

impl Journal {
    pub(crate) fn begin(home: &Path, operation: &str, plan: &Value) -> Result<Self> {
        let parent = home
            .parent()
            .ok_or_else(|| invalid("Hardknock home has no parent for the setup journal"))?;
        let transaction_id = uuid::Uuid::new_v4().to_string();
        let temporary_path =
            parent.join(format!(".hardknock-setup-{transaction_id}.journal.jsonl"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)?;
        let mut journal = Self {
            transaction_id,
            temporary_path,
            file,
        };
        journal.record(
            "planned",
            json!({"operation":operation,"home":home,"plan":plan}),
        )?;
        Ok(journal)
    }

    pub(crate) fn path(&self) -> &Path {
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

    pub(crate) fn finish(mut self, home: &Path, outcome: &str) -> Result<PathBuf> {
        self.record("finished", json!({"outcome":outcome}))?;
        let directory = home.join("setup/transactions");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(home.join("setup"), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let destination = directory.join(format!("{}.jsonl", self.transaction_id));
        if destination.exists() {
            return Err(invalid("Setup journal destination already exists"));
        }
        drop(self.file);
        fs::rename(&self.temporary_path, &destination)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
        Ok(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_restores_existing_files_and_removes_new_files() {
        let temporary = tempfile::tempdir().unwrap();
        let existing = temporary.path().join("existing.json");
        let created = temporary.path().join("new/deep/created.json");
        fs::write(&existing, b"before").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();
        let snapshots = SnapshotSet::capture(vec![existing.clone(), created.clone()]).unwrap();

        fs::write(&existing, b"after").unwrap();
        fs::create_dir_all(created.parent().unwrap()).unwrap();
        fs::write(&created, b"created").unwrap();
        let mut snapshots = snapshots;
        snapshots
            .checkpoint(vec![existing.clone(), created.clone()])
            .unwrap();
        snapshots.rollback().unwrap();

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
        let path = temporary.path().join("managed.json");
        fs::write(&path, b"before").unwrap();
        let mut snapshots = SnapshotSet::capture(vec![path.clone()]).unwrap();
        fs::write(&path, b"transaction").unwrap();
        snapshots.checkpoint(vec![path.clone()]).unwrap();
        fs::write(&path, b"concurrent").unwrap();

        let error = snapshots.rollback().unwrap_err();
        assert!(error.to_string().contains("preserved current content"));
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
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
        let mut journal = Journal::begin(&home, "setup", &json!({"changes":[]})).unwrap();
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
}
