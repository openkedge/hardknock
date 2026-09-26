// SPDX-License-Identifier: Apache-2.0

use crate::{Error, Result, store::Store};
use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use nix::unistd::geteuid;
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, RenameFlags, Stat, fchmod, fstat, fsync, openat,
    renameat_with, statat, unlinkat,
};
use serde_json::{Value, json};
use std::{
    env,
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::{
        fd::OwnedFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const ACTIVE_LOG: &str = "bridge.jsonl";
const LAUNCHER_LOCK: &str = "bridge-diagnostics.lock";
const LOGGER_ENV: &str = "HARDKNOCK_INTERNAL_BRIDGE_LOGGER";
const LOGGER_READY: &str = "hardknock-bridge-logger-ready";
const LOGGER_ACK_PREFIX: &str = "hardknock-bridge-logger-flushed:";
const LOGGER_FLUSH_FIELD: &str = "_hardknock_bridge_logger_flush";
const RETAINED_ARCHIVES: usize = 4;
const MAX_LOG_BYTES: u64 = 1024 * 1024;
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o700);
const PRIVATE_FILE_MODE: Mode = Mode::from_raw_mode(0o600);

type AckResult = std::result::Result<String, String>;

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}

fn system<T>(result: rustix::io::Result<T>) -> Result<T> {
    result.map_err(|error| io::Error::from(error).into())
}

fn archive_name(index: usize) -> String {
    format!("bridge.{index}.jsonl")
}

#[cfg(test)]
fn archive_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(archive_name(index))
}

fn same_file(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn permission_bits(stat: &Stat) -> u32 {
    u32::from(stat.st_mode) & 0o7777
}

fn validate_regular_stat(stat: &Stat, path: &Path) -> Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != geteuid().as_raw()
        || stat.st_nlink != 1
    {
        return Err(invalid(format!(
            "Bridge diagnostic file failed secure regular-file validation: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_open_file(file: &File, path: &Path) -> Result<Stat> {
    let before = system(fstat(file))?;
    validate_regular_stat(&before, path)?;
    let repaired_permissions = permission_bits(&before) != 0o600;
    system(fchmod(file, PRIVATE_FILE_MODE))?;
    let after = system(fstat(file))?;
    validate_regular_stat(&after, path)?;
    if !same_file(&before, &after) || permission_bits(&after) != 0o600 {
        return Err(invalid(format!(
            "Bridge diagnostic file changed while it was being secured: {}",
            path.display()
        )));
    }
    if repaired_permissions {
        file.sync_all()?;
    }
    Ok(after)
}

struct DiagnosticsDirectory {
    path: PathBuf,
    file: File,
}

impl DiagnosticsDirectory {
    fn open(path: &Path) -> Result<Self> {
        let file = File::from(system(openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ))?);
        let before = system(fstat(&file))?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(invalid(format!(
                "Bridge diagnostics directory must be a directory, not a symlink: {}",
                path.display()
            )));
        }
        if before.st_uid != geteuid().as_raw() {
            return Err(invalid(format!(
                "Bridge diagnostics directory is not owned by the current user: {}",
                path.display()
            )));
        }

        system(fchmod(&file, PRIVATE_DIRECTORY_MODE))?;
        let after = system(fstat(&file))?;
        if FileType::from_raw_mode(after.st_mode) != FileType::Directory
            || after.st_uid != geteuid().as_raw()
            || !same_file(&before, &after)
            || permission_bits(&after) != 0o700
        {
            return Err(invalid(format!(
                "Bridge diagnostics directory changed while it was being secured: {}",
                path.display()
            )));
        }

        let named = system(statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW))?;
        if FileType::from_raw_mode(named.st_mode) != FileType::Directory
            || !same_file(&after, &named)
        {
            return Err(invalid(format!(
                "Bridge diagnostics directory changed while it was being opened: {}",
                path.display()
            )));
        }
        system(fsync(&file))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    fn entry_path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
            return Err(invalid(format!(
                "Invalid Bridge diagnostic file name: {name:?}"
            )));
        }
        Ok(self.path.join(name))
    }

    fn stat_entry(&self, name: &str) -> Result<Option<Stat>> {
        let path = self.entry_path(name)?;
        match statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(stat)),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
            Err(error) => Err(Error::Io(io::Error::from(error))),
        }
        .map_err(|error| match error {
            Error::Io(source) => Error::Io(io::Error::new(
                source.kind(),
                format!("could not inspect {}: {source}", path.display()),
            )),
            other => other,
        })
    }

    fn open_error(&self, name: &str, error: rustix::io::Errno) -> Error {
        let path = self.path.join(name);
        if self
            .stat_entry(name)
            .ok()
            .flatten()
            .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile)
        {
            return invalid(format!(
                "Bridge diagnostic path must be a regular file, not a symlink: {}",
                path.display()
            ));
        }
        Error::Io(io::Error::new(
            io::Error::from(error).kind(),
            format!("could not open {} securely: {error}", path.display()),
        ))
    }

    fn validate_named_file(&self, name: &str, file: &File) -> Result<Stat> {
        let path = self.entry_path(name)?;
        let opened = system(fstat(file))?;
        validate_regular_stat(&opened, &path)?;
        if permission_bits(&opened) != 0o600 {
            return Err(invalid(format!(
                "Bridge diagnostic file permissions changed while it was open: {}",
                path.display()
            )));
        }
        let named = self.stat_entry(name)?.ok_or_else(|| {
            invalid(format!(
                "Bridge diagnostic file disappeared: {}",
                path.display()
            ))
        })?;
        validate_regular_stat(&named, &path)?;
        if permission_bits(&named) != 0o600 || !same_file(&opened, &named) {
            return Err(invalid(format!(
                "Bridge diagnostic file changed while it was open: {}",
                path.display()
            )));
        }
        Ok(opened)
    }

    fn open_existing(&self, name: &str, flags: OFlags) -> Result<Option<File>> {
        let path = self.entry_path(name)?;
        let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = match openat(&self.file, name, flags, Mode::empty()) {
            Ok(file) => File::from(file),
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) => return Err(self.open_error(name, error)),
        };
        validate_open_file(&file, &path)?;
        self.validate_named_file(name, &file)?;
        Ok(Some(file))
    }

    fn open_or_create(&self, name: &str, flags: OFlags) -> Result<File> {
        let path = self.entry_path(name)?;
        let secure_flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = match openat(
            &self.file,
            name,
            secure_flags | OFlags::CREATE | OFlags::EXCL,
            PRIVATE_FILE_MODE,
        ) {
            Ok(file) => {
                let file = File::from(file);
                validate_open_file(&file, &path)?;
                self.validate_named_file(name, &file)?;
                file.sync_all()?;
                self.sync()?;
                return Ok(file);
            }
            Err(error) if error == rustix::io::Errno::EXIST => {
                self.open_existing(name, flags)?.ok_or_else(|| {
                    invalid(format!(
                        "Bridge diagnostic file changed during open: {}",
                        path.display()
                    ))
                })?
            }
            Err(error) => {
                return Err(Error::Io(io::Error::new(
                    io::Error::from(error).kind(),
                    format!("could not create {} securely: {error}", path.display()),
                )));
            }
        };
        Ok(file)
    }

    fn open_append(&self, name: &str) -> Result<File> {
        self.open_or_create(name, OFlags::WRONLY | OFlags::APPEND)
    }

    fn sync(&self) -> Result<()> {
        system(fsync(&self.file))
    }

    fn rename_noreplace(&self, source: &str, destination: &str, file: &File) -> Result<()> {
        let source_path = self.entry_path(source)?;
        let destination_path = self.entry_path(destination)?;
        self.validate_named_file(source, file)?;
        match renameat_with(
            &self.file,
            source,
            &self.file,
            destination,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {
                return Err(invalid(format!(
                    "Bridge diagnostic rotation collision; refusing to replace {}",
                    destination_path.display()
                )));
            }
            Err(error) => {
                return Err(Error::Io(io::Error::new(
                    io::Error::from(error).kind(),
                    format!(
                        "could not rotate {} to {} securely: {error}",
                        source_path.display(),
                        destination_path.display()
                    ),
                )));
            }
        }
        self.sync()?;
        self.validate_named_file(destination, file).map(|_| ())
    }

    fn unlink(&self, name: &str, file: &File) -> Result<()> {
        let path = self.entry_path(name)?;
        let before = self.validate_named_file(name, file)?;
        system(unlinkat(&self.file, name, AtFlags::empty()))?;
        self.sync()?;
        let after = system(fstat(file))?;
        if !same_file(&before, &after) || after.st_nlink != 0 {
            return Err(invalid(format!(
                "Bridge diagnostic file changed during removal: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

fn rotate(directory: &DiagnosticsDirectory, expected_active: Option<&File>) -> Result<()> {
    let active = directory.open_existing(ACTIVE_LOG, OFlags::RDONLY)?;
    let mut archives = Vec::with_capacity(RETAINED_ARCHIVES);
    for index in 1..=RETAINED_ARCHIVES {
        archives.push(directory.open_existing(&archive_name(index), OFlags::RDONLY)?);
    }

    let Some(active) = active else {
        if expected_active.is_some() {
            return Err(invalid(format!(
                "Bridge diagnostic file disappeared before rotation: {}",
                directory.path.join(ACTIVE_LOG).display()
            )));
        }
        return Ok(());
    };
    if let Some(expected) = expected_active {
        let actual = system(fstat(&active))?;
        let expected = directory.validate_named_file(ACTIVE_LOG, expected)?;
        if !same_file(&actual, &expected) {
            return Err(invalid(format!(
                "Bridge diagnostic file changed before rotation: {}",
                directory.path.join(ACTIVE_LOG).display()
            )));
        }
    }

    if let Some(oldest) = archives[RETAINED_ARCHIVES - 1].as_ref() {
        directory.unlink(&archive_name(RETAINED_ARCHIVES), oldest)?;
    }
    for index in (1..RETAINED_ARCHIVES).rev() {
        if let Some(archive) = archives[index - 1].as_ref() {
            directory.rename_noreplace(&archive_name(index), &archive_name(index + 1), archive)?;
        }
    }
    directory.rename_noreplace(ACTIVE_LOG, &archive_name(1), &active)
}

fn launcher_lock(directory: &DiagnosticsDirectory) -> Result<(File, String)> {
    let mut file = directory.open_or_create(LAUNCHER_LOCK, OFlags::RDWR)?;
    FileExt::try_lock_exclusive(&file)
        .map_err(|_| invalid("Bridge diagnostic launcher is already active"))?;
    directory.validate_named_file(LAUNCHER_LOCK, &file)?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(token.as_bytes())?;
    file.sync_data()?;
    directory.validate_named_file(LAUNCHER_LOCK, &file)?;
    Ok((file, token))
}

fn sidecar_token_matches(home: &Path, expected: &str) -> Result<bool> {
    let store = Store::open(home)?;
    let directory = DiagnosticsDirectory::open(&store.home.join("logs"))?;
    let Some(mut file) = directory.open_existing(LAUNCHER_LOCK, OFlags::RDONLY)? else {
        return Ok(false);
    };
    let mut actual = String::new();
    Read::by_ref(&mut file)
        .take(129)
        .read_to_string(&mut actual)?;
    if actual.len() > 128 {
        return Ok(false);
    }
    directory.validate_named_file(LAUNCHER_LOCK, &file)?;
    Ok(actual == expected)
}

enum IncomingRecord {
    Bounded(Vec<u8>),
    Oversized(u64),
}

fn read_record(input: &mut impl BufRead) -> io::Result<Option<IncomingRecord>> {
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return if total == 0 {
                Ok(None)
            } else if oversized {
                Ok(Some(IncomingRecord::Oversized(total)))
            } else {
                Ok(Some(IncomingRecord::Bounded(bytes)))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        total = total.saturating_add(consumed as u64);
        if !oversized {
            if total <= MAX_LOG_BYTES {
                bytes.extend_from_slice(&available[..consumed]);
            } else {
                bytes.clear();
                oversized = true;
            }
        }
        input.consume(consumed);
        if newline.is_some() {
            return if oversized {
                Ok(Some(IncomingRecord::Oversized(total)))
            } else {
                Ok(Some(IncomingRecord::Bounded(bytes)))
            };
        }
    }
}

fn normalized_record(record: IncomingRecord) -> Result<(Option<Vec<u8>>, Option<String>)> {
    let mut bytes = match record {
        IncomingRecord::Bounded(bytes) => bytes,
        IncomingRecord::Oversized(bytes) => {
            let mut dropped = serde_json::to_vec(&json!({
                "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                "event": "bridge_diagnostic_record_dropped",
                "component": "bridge_logger",
                "details": {
                    "bytes": bytes,
                    "reason": "record exceeds the Bridge diagnostic size limit",
                },
            }))?;
            dropped.push(b'\n');
            return Ok((Some(dropped), None));
        }
    };

    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    if let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) {
        let flush = value
            .as_object_mut()
            .and_then(|object| object.remove(LOGGER_FLUSH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));
        if flush.is_some() && value.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok((None, flush));
        }
        let mut structured = if flush.is_some() {
            serde_json::to_vec(&value)?
        } else {
            bytes
        };
        structured.push(b'\n');
        return Ok((Some(structured), flush));
    }

    let mut unstructured = serde_json::to_vec(&json!({
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "event": "bridge_diagnostic_unstructured",
        "component": "bridge_logger",
        "details": {
            "bytes": bytes.len(),
            "message": String::from_utf8_lossy(&bytes).chars().take(4096).collect::<String>(),
        },
    }))?;
    unstructured.push(b'\n');
    Ok((Some(unstructured), None))
}

fn write_record(directory: &DiagnosticsDirectory, output: &mut File, record: &[u8]) -> Result<()> {
    directory.validate_named_file(ACTIVE_LOG, output)?;
    if output.metadata()?.len().saturating_add(record.len() as u64) > MAX_LOG_BYTES {
        output.sync_data()?;
        rotate(directory, Some(output))?;
        *output = directory.open_append(ACTIVE_LOG)?;
    }
    output.write_all(record)?;
    output.flush()?;
    directory.validate_named_file(ACTIVE_LOG, output)?;
    Ok(())
}

fn copy_records(
    directory: &DiagnosticsDirectory,
    input: &mut impl BufRead,
    acknowledgements: &mut impl Write,
) -> Result<()> {
    let mut output = directory.open_append(ACTIVE_LOG)?;
    while let Some(incoming) = read_record(input)? {
        let (record, flush) = normalized_record(incoming)?;
        if let Some(record) = record {
            write_record(directory, &mut output, &record)?;
        }
        if let Some(token) = flush {
            output.sync_data()?;
            writeln!(acknowledgements, "{LOGGER_ACK_PREFIX}{token}")?;
            acknowledgements.flush()?;
        }
    }
    output.sync_data()?;
    Ok(())
}

fn run_logger(home: &Path) -> Result<()> {
    let store = Store::open(home)?;
    let directory = DiagnosticsDirectory::open(&store.home.join("logs"))?;
    directory.open_append(ACTIVE_LOG)?.sync_data()?;

    let mut acknowledgements = io::stderr().lock();
    writeln!(acknowledgements, "{LOGGER_READY}")?;
    acknowledgements.flush()?;
    let stdin = io::stdin();
    copy_records(
        &directory,
        &mut BufReader::new(stdin.lock()),
        &mut acknowledgements,
    )
}

fn sidecar_entry(home: &Path) -> ! {
    let result = run_logger(home);
    if let Err(error) = &result {
        let _ = writeln!(
            io::stderr().lock(),
            "{}",
            json!({"event": "bridge_logger_failed", "message": error.to_string()})
        );
    }
    std::process::exit(if result.is_ok() { 0 } else { 2 })
}

fn acknowledgement_reader(stderr: ChildStderr) -> Receiver<AckResult> {
    let (sender, receiver) = mpsc::channel();
    let _ = thread::spawn(move || {
        let mut input = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            match input.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.send(Err("Bridge diagnostic logger closed its channel".into()));
                    break;
                }
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    receiver
}

fn wait_for_ack(acks: &Receiver<AckResult>, expected: &str) -> Result<()> {
    let deadline = Instant::now() + ACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(invalid(format!(
                "Bridge diagnostic logger acknowledgement timeout: {expected}"
            )));
        }
        match acks.recv_timeout(remaining) {
            Ok(Ok(line)) if line.trim() == expected => return Ok(()),
            Ok(Ok(line)) if line.contains("bridge_logger_failed") => {
                return Err(invalid(format!(
                    "Bridge diagnostic logger failed: {}",
                    line.trim().chars().take(512).collect::<String>()
                )));
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(invalid(error)),
            Err(RecvTimeoutError::Timeout) => {
                return Err(invalid(format!(
                    "Bridge diagnostic logger acknowledgement timeout: {expected}"
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(invalid("Bridge diagnostic logger channel disconnected"));
            }
        }
    }
}

pub(crate) struct DetachedDiagnostics {
    path: PathBuf,
    writer: UnixStream,
    _lock: File,
    sidecar: Option<Child>,
    acknowledgements: Receiver<AckResult>,
}

impl DetachedDiagnostics {
    pub(crate) fn open(home: &Path, tcp: Option<u16>) -> Result<Self> {
        if let Some(token) = env::var_os(LOGGER_ENV) {
            let token = token.to_string_lossy();
            if sidecar_token_matches(home, &token)? {
                sidecar_entry(home);
            }
            return Err(invalid("Invalid internal Bridge diagnostic logger token"));
        }

        let store = Store::open(home)?;
        let directory = DiagnosticsDirectory::open(&store.home.join("logs"))?;
        let (lock, token) = launcher_lock(&directory)?;
        rotate(&directory, None)?;
        let path = directory.path.join(ACTIVE_LOG);
        directory.open_append(ACTIVE_LOG)?.sync_data()?;

        let (reader, writer) = UnixStream::pair()?;
        let mut child = Command::new(env::current_exe()?);
        child
            .arg("--json")
            .arg("--home")
            .arg(&store.home)
            .args(["bridge", "start"])
            .env(LOGGER_ENV, token)
            .stdin(Stdio::from(OwnedFd::from(reader)))
            // This inherited descriptor keeps the exclusive launcher lock for
            // the logger's lifetime. The sidecar exits before printing a CLI response.
            .stdout(Stdio::from(lock.try_clone()?))
            .stderr(Stdio::piped())
            .process_group(0);
        let mut sidecar = child.spawn()?;
        let acknowledgements = acknowledgement_reader(
            sidecar
                .stderr
                .take()
                .ok_or_else(|| invalid("Bridge diagnostic logger has no readiness channel"))?,
        );
        if let Err(error) = wait_for_ack(&acknowledgements, LOGGER_READY) {
            let _ = sidecar.kill();
            let _ = sidecar.wait();
            return Err(error);
        }

        let transport = tcp.map_or_else(
            || json!({"kind": "unix"}),
            |port| json!({"kind": "tcp", "port": port}),
        );
        let mut diagnostics = Self {
            path,
            writer,
            _lock: lock,
            sidecar: Some(sidecar),
            acknowledgements,
        };
        diagnostics.record(
            "bridge_detached_start",
            json!({"launcher_pid": std::process::id(), "transport": transport}),
        )?;
        Ok(diagnostics)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn stdio(&self) -> Result<(Stdio, Stdio)> {
        Ok((
            Stdio::from(OwnedFd::from(self.writer.try_clone()?)),
            Stdio::from(OwnedFd::from(self.writer.try_clone()?)),
        ))
    }

    pub(crate) fn record(&mut self, event: &str, details: Value) -> Result<()> {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut record = serde_json::to_vec(&json!({
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            "event": event,
            "component": "bridge_launcher",
            "details": details,
            "_hardknock_bridge_logger_flush": token,
        }))?;
        record.push(b'\n');
        self.writer.write_all(&record)?;
        wait_for_ack(
            &self.acknowledgements,
            &format!("{LOGGER_ACK_PREFIX}{token}"),
        )
    }
}

impl Drop for DetachedDiagnostics {
    fn drop(&mut self) {
        if let Some(mut sidecar) = self.sidecar.take() {
            let _ = thread::spawn(move || {
                let _ = sidecar.wait();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Cursor,
        os::unix::fs::{PermissionsExt, symlink},
        sync::Arc,
    };

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let store = Store::open(&home).unwrap();
        (temp, store.home.join("logs"))
    }

    fn diagnostic_paths(directory: &Path) -> Vec<PathBuf> {
        (0..=RETAINED_ARCHIVES)
            .map(|index| {
                if index == 0 {
                    directory.join(ACTIVE_LOG)
                } else {
                    archive_path(directory, index)
                }
            })
            .filter(|path| path.exists())
            .collect()
    }

    fn private_write(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn bridge_diagnostics_size_and_single_line_memory_are_bounded() {
        let (_temp, path) = setup();
        let directory = DiagnosticsDirectory::open(&path).unwrap();
        let padding = "x".repeat(64 * 1024);
        let mut input = Vec::new();
        for sequence in 0..40 {
            serde_json::to_writer(
                &mut input,
                &json!({"event": "size_test", "sequence": sequence, "padding": padding}),
            )
            .unwrap();
            input.push(b'\n');
        }
        input.extend(std::iter::repeat_n(b'x', MAX_LOG_BYTES as usize * 3));
        input.push(b'\n');
        serde_json::to_writer(&mut input, &json!({"event": "tail"})).unwrap();
        input.push(b'\n');

        copy_records(
            &directory,
            &mut BufReader::new(Cursor::new(input)),
            &mut Vec::new(),
        )
        .unwrap();

        let paths = diagnostic_paths(&path);
        assert!(paths.len() >= 2);
        let mut dropped = false;
        let mut tail = false;
        for path in paths {
            assert!(fs::metadata(&path).unwrap().len() <= MAX_LOG_BYTES);
            for line in fs::read_to_string(path).unwrap().lines() {
                let record: Value = serde_json::from_str(line).unwrap();
                dropped |= record["event"] == "bridge_diagnostic_record_dropped";
                tail |= record["event"] == "tail";
            }
        }
        assert!(dropped);
        assert!(tail);
    }

    #[test]
    fn bridge_diagnostics_rotation_retains_four_archives() {
        let (_temp, path) = setup();
        private_write(
            &path.join(ACTIVE_LOG),
            "{\"event\":\"oldest_generation\"}\n",
        );
        let directory = DiagnosticsDirectory::open(&path).unwrap();
        for generation in 0..6 {
            rotate(&directory, None).unwrap();
            let mut active = directory.open_append(ACTIVE_LOG).unwrap();
            writeln!(
                active,
                "{}",
                json!({"event": "generation", "generation": generation})
            )
            .unwrap();
        }

        let paths = diagnostic_paths(&path);
        assert_eq!(paths.len(), RETAINED_ARCHIVES + 1);
        assert!(!archive_path(&path, RETAINED_ARCHIVES + 1).exists());
        assert!(paths.iter().all(|path| {
            !fs::read_to_string(path)
                .unwrap()
                .contains("oldest_generation")
        }));
    }

    #[test]
    fn bridge_diagnostics_launcher_lock_serializes_open() {
        let (_temp, path) = setup();
        let directory = Arc::new(DiagnosticsDirectory::open(&path).unwrap());
        let (first, _) = launcher_lock(&directory).unwrap();
        let competing = {
            let directory = directory.clone();
            thread::spawn(move || launcher_lock(&directory).map(|_| ()))
        };
        assert!(
            competing
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("diagnostic launcher is already active")
        );
        drop(first);
        launcher_lock(&directory).unwrap();
    }

    #[test]
    fn bridge_diagnostics_directory_descriptor_survives_path_substitution() {
        let (temp, path) = setup();
        let directory = DiagnosticsDirectory::open(&path).unwrap();
        let moved = temp.path().join("original-logs");
        fs::rename(&path, &moved).unwrap();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let mut active = directory.open_append(ACTIVE_LOG).unwrap();
        writeln!(active, "{}", json!({"event": "descriptor_anchored"})).unwrap();
        active.sync_all().unwrap();

        assert!(
            fs::read_to_string(moved.join(ACTIVE_LOG))
                .unwrap()
                .contains("descriptor_anchored")
        );
        assert!(!path.join(ACTIVE_LOG).exists());
    }

    #[test]
    fn bridge_diagnostics_rejects_active_path_substitution() {
        let (_temp, path) = setup();
        let directory = DiagnosticsDirectory::open(&path).unwrap();
        let mut active = directory.open_append(ACTIVE_LOG).unwrap();
        private_write(&path.join("displaced.jsonl"), "placeholder\n");
        fs::rename(path.join(ACTIVE_LOG), path.join("original.jsonl")).unwrap();
        fs::rename(path.join("displaced.jsonl"), path.join(ACTIVE_LOG)).unwrap();

        let error = write_record(
            &directory,
            &mut active,
            b"{\"event\":\"must_not_be_written\"}\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed while it was open"));
        assert!(
            !fs::read_to_string(path.join(ACTIVE_LOG))
                .unwrap()
                .contains("must_not_be_written")
        );
        assert!(
            !fs::read_to_string(path.join("original.jsonl"))
                .unwrap()
                .contains("must_not_be_written")
        );
    }

    #[test]
    fn bridge_diagnostics_rotation_collision_does_not_overwrite() {
        let (_temp, path) = setup();
        private_write(&path.join("source.jsonl"), "source\n");
        private_write(&path.join("destination.jsonl"), "destination\n");
        let directory = DiagnosticsDirectory::open(&path).unwrap();
        let source = directory
            .open_existing("source.jsonl", OFlags::RDONLY)
            .unwrap()
            .unwrap();

        let error = directory
            .rename_noreplace("source.jsonl", "destination.jsonl", &source)
            .unwrap_err();

        assert!(error.to_string().contains("rotation collision"));
        assert_eq!(
            fs::read_to_string(path.join("source.jsonl")).unwrap(),
            "source\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("destination.jsonl")).unwrap(),
            "destination\n"
        );
    }

    #[test]
    fn bridge_diagnostics_never_follows_a_log_symlink() {
        let (temp, path) = setup();
        let outside = temp.path().join("outside.log");
        private_write(&outside, "outside\n");
        symlink(&outside, path.join(ACTIVE_LOG)).unwrap();
        let directory = DiagnosticsDirectory::open(&path).unwrap();

        let error = directory.open_append(ACTIVE_LOG).unwrap_err();

        assert!(error.to_string().contains("regular file, not a symlink"));
        assert_eq!(fs::read_to_string(outside).unwrap(), "outside\n");
    }
}
