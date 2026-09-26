// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    cli::integrations::AdapterCommand,
    setup::transaction::{
        MutationObserver, PrivateMutationDirectory, UnobservedMutation, WriteReceipt,
        ensure_directory_tree,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsStr,
    fs,
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::OwnedFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, RenameFlags, Stat, fstat, open, openat, renameat_with,
    statat,
};

const MAX_INTEGRATION_JSON_BYTES: u64 = 1024 * 1024;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SessionEnd",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationAction {
    Install,
    Uninstall,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntegrationPlan {
    pub agent: String,
    pub action: IntegrationAction,
    pub action_summary: String,
    pub home_path: PathBuf,
    pub target_path: PathBuf,
    pub config_path: PathBuf,
    pub manifest_path: PathBuf,
    pub managed_paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardknock_executable: Option<PathBuf>,
}

impl IntegrationPlan {
    pub fn description(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}

struct OpenedUserFile {
    file: File,
    metadata: fs::Metadata,
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn validate_user_file_metadata(metadata: &fs::Metadata, label: &str) -> Result<()> {
    if !metadata.is_file() {
        return Err(invalid(&format!("{label} must be a regular file")));
    }
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.nlink() != 1 {
        return Err(invalid(&format!(
            "{label} must be singly linked and owned by the effective user"
        )));
    }
    Ok(())
}

fn validate_read_metadata(
    metadata: &fs::Metadata,
    label: &str,
    required_mode: Option<u32>,
) -> Result<()> {
    validate_user_file_metadata(metadata, label)?;
    if let Some(required_mode) = required_mode
        && metadata.permissions().mode() & 0o7777 != required_mode
    {
        return Err(invalid(&format!(
            "{label} permissions must be {required_mode:04o}"
        )));
    }
    Ok(())
}

fn open_bounded_user_file(
    path: &Path,
    label: &str,
    maximum_bytes: u64,
    required_mode: Option<u32>,
) -> Result<Option<OpenedUserFile>> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if path_metadata.file_type().is_symlink() {
        return Err(invalid(&format!("{label} must not be a symlink")));
    }
    validate_read_metadata(&path_metadata, label, required_mode)?;
    if path_metadata.len() > maximum_bytes {
        return Err(invalid(&format!(
            "{label} exceeds the {maximum_bytes}-byte limit"
        )));
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    validate_read_metadata(&metadata, label, required_mode)?;
    if !same_identity(&path_metadata, &metadata) {
        return Err(invalid(&format!(
            "{label} changed while it was being opened"
        )));
    }
    if metadata.len() > maximum_bytes {
        return Err(invalid(&format!(
            "{label} exceeds the {maximum_bytes}-byte limit"
        )));
    }
    Ok(Some(OpenedUserFile { file, metadata }))
}

fn read_opened_user_file(
    path: &Path,
    opened: &mut OpenedUserFile,
    label: &str,
    maximum_bytes: u64,
    required_mode: Option<u32>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened.metadata.len())
            .unwrap_or(0)
            .min(8192),
    );
    (&mut opened.file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;

    let descriptor_metadata = opened.file.metadata()?;
    if bytes.len() as u64 > maximum_bytes || descriptor_metadata.len() > maximum_bytes {
        return Err(invalid(&format!(
            "{label} exceeds the {maximum_bytes}-byte limit"
        )));
    }
    validate_read_metadata(&descriptor_metadata, label, required_mode)?;
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() {
        return Err(invalid(&format!("{label} changed while it was being read")));
    }
    validate_read_metadata(&path_metadata, label, required_mode)?;
    if !same_identity(&opened.metadata, &descriptor_metadata)
        || !same_identity(&opened.metadata, &path_metadata)
        || opened.metadata.len() != descriptor_metadata.len()
        || opened.metadata.mtime() != descriptor_metadata.mtime()
        || opened.metadata.mtime_nsec() != descriptor_metadata.mtime_nsec()
    {
        return Err(invalid(&format!("{label} changed while it was being read")));
    }
    Ok(bytes)
}

fn read_bounded_user_file(
    path: &Path,
    label: &str,
    maximum_bytes: u64,
    required_mode: Option<u32>,
) -> Result<Option<Vec<u8>>> {
    let Some(mut opened) = open_bounded_user_file(path, label, maximum_bytes, required_mode)?
    else {
        return Ok(None);
    };
    read_opened_user_file(path, &mut opened, label, maximum_bytes, required_mode).map(Some)
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).map(|p| p.join(name)).find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
fn user_home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME unavailable; provide --config"))
}

fn path_for(agent: &str, override_path: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path.clone());
    }
    Ok(match agent {
        "claude" => user_home()?.join(".claude/settings.json"),
        "hermes" => user_home()?.join(".hermes/plugins/hardknock"),
        "openclaw" => user_home()?.join(".openclaw/extensions/hardknock"),
        _ => return Err(invalid("Unknown adapter")),
    })
}

fn validate_existing_private_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid(&format!("{label} must be a regular directory")));
    }
    if metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(invalid(&format!(
            "{label} must be owned by the effective user"
        )));
    }
    if metadata.permissions().mode() & 0o7777 != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(&format!(
            "{label} permissions must be {PRIVATE_DIRECTORY_MODE:04o}"
        )));
    }
    Ok(())
}

fn validate_existing_user_file(path: &Path, label: &str) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid(&format!("{label} must be a regular file")));
    }
    validate_user_file_metadata(&metadata, label)
}

fn read_json(path: &Path) -> Result<Value> {
    read_json_with_bytes(path).map(|(value, _)| value)
}

fn read_json_with_bytes(path: &Path) -> Result<(Value, Option<Vec<u8>>)> {
    let Some(bytes) = read_bounded_user_file(
        path,
        "Integration configuration",
        MAX_INTEGRATION_JSON_BYTES,
        None,
    )?
    else {
        return Ok((json!({}), None));
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(invalid("Integration configuration must be a JSON object"));
    }
    Ok((value, Some(bytes)))
}

fn read_manifest(path: &Path) -> Result<Value> {
    read_manifest_with_bytes(path).map(|(value, _)| value)
}

fn read_manifest_with_bytes(path: &Path) -> Result<(Value, Option<Vec<u8>>)> {
    let Some(bytes) = read_bounded_user_file(
        path,
        "Managed integration manifest",
        MAX_INTEGRATION_JSON_BYTES,
        Some(PRIVATE_FILE_MODE),
    )?
    else {
        return Ok((json!({}), None));
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(invalid(
            "Managed integration manifest must be a JSON object",
        ));
    }
    Ok((value, Some(bytes)))
}

fn validate_existing_private_file(path: &Path, label: &str) -> Result<()> {
    validate_existing_user_file(path, label)?;
    if path.exists()
        && fs::symlink_metadata(path)?.permissions().mode() & 0o7777 != PRIVATE_FILE_MODE
    {
        return Err(invalid(&format!("{label} permissions must be 0600")));
    }
    Ok(())
}

struct BoundParent {
    descriptor: OwnedFd,
    path: PathBuf,
    stat: Stat,
}

impl BoundParent {
    fn open(path: &Path) -> Result<Self> {
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?;
        let stat = fstat(&descriptor).map_err(errno_error)?;
        let named = statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
            || !same_stat_identity(&stat, &named)
            || stat.st_uid != nix::unistd::geteuid().as_raw()
        {
            return Err(invalid(&format!(
                "Integration parent changed or is not owned by the effective user: {}",
                path.display()
            )));
        }
        Ok(Self {
            descriptor,
            path: path.to_path_buf(),
            stat,
        })
    }

    fn verify(&self) -> Result<()> {
        let opened = fstat(&self.descriptor).map_err(errno_error)?;
        let named = statat(CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
            || !same_stat_identity(&self.stat, &opened)
            || !same_stat_identity(&self.stat, &named)
        {
            return Err(invalid(&format!(
                "Integration parent changed during mutation: {}",
                self.path.display()
            )));
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        rustix::fs::fsync(&self.descriptor).map_err(errno_error)?;
        Ok(())
    }
}

fn errno_error(error: rustix::io::Errno) -> Error {
    Error::Io(std::io::Error::from(error))
}

fn same_stat_identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn path_parts(path: &Path) -> Result<(&Path, &OsStr)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Integration path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Integration path has no file name"))?;
    if name == OsStr::new(".") || name == OsStr::new("..") {
        return Err(invalid("Integration path has an unsafe file name"));
    }
    Ok((parent, name))
}

fn open_expected_file(
    parent: &BoundParent,
    name: &OsStr,
    path: &Path,
    expected: Option<&[u8]>,
    required_mode: Option<u32>,
) -> Result<Option<File>> {
    let descriptor = match openat(
        &parent.descriptor,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) if expected.is_none() => return Ok(None),
        Err(rustix::io::Errno::NOENT) => {
            return Err(invalid(&format!(
                "Integration file disappeared before mutation: {}",
                path.display()
            )));
        }
        Err(error) => return Err(errno_error(error)),
    };
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    validate_read_metadata(&metadata, "Integration mutation target", required_mode)?;
    let Some(expected) = expected else {
        return Err(invalid(&format!(
            "Integration file appeared before mutation: {}",
            path.display()
        )));
    };
    let bytes = read_bounded_file(&file, MAX_INTEGRATION_JSON_BYTES as usize + 1)?;
    if bytes != expected {
        return Err(invalid(&format!(
            "Integration file changed before mutation: {}",
            path.display()
        )));
    }
    validate_named_file(parent, name, &file, path)?;
    Ok(Some(file))
}

fn read_bounded_file(file: &File, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    file.try_clone()?
        .take(limit as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() == limit {
        return Err(invalid(
            "Integration mutation target exceeds its bounded size",
        ));
    }
    Ok(bytes)
}

fn validate_named_file(
    parent: &BoundParent,
    name: &OsStr,
    file: &File,
    path: &Path,
) -> Result<Stat> {
    parent.verify()?;
    let opened = fstat(file).map_err(errno_error)?;
    let named = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno_error)?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
        || !same_stat_identity(&opened, &named)
    {
        return Err(invalid(&format!(
            "Integration file changed during mutation: {}",
            path.display()
        )));
    }
    Ok(opened)
}

pub(crate) fn write_managed_bytes_observed(
    path: &Path,
    bytes: &[u8],
    private: bool,
    expected: Option<&[u8]>,
    observer: &mut impl MutationObserver,
) -> Result<()> {
    let receipt = atomic_write_observed_with_hook(path, bytes, private, expected, observer, || {})?;
    observer.record_write(receipt)
}

#[cfg(test)]
fn atomic_write_with_hook(
    path: &Path,
    bytes: &[u8],
    private: bool,
    expected: Option<&[u8]>,
    before_commit: impl FnOnce(),
) -> Result<WriteReceipt> {
    let mut observer = UnobservedMutation;
    let receipt = atomic_write_observed_with_hook(
        path,
        bytes,
        private,
        expected,
        &mut observer,
        before_commit,
    )?;
    observer.record_write(receipt.clone())?;
    Ok(receipt.without_cleanup())
}

fn atomic_write_observed_with_hook(
    path: &Path,
    bytes: &[u8],
    private: bool,
    expected: Option<&[u8]>,
    observer: &mut impl MutationObserver,
    before_commit: impl FnOnce(),
) -> Result<WriteReceipt> {
    let (parent_path, target_name) = path_parts(path)?;
    ensure_directory_tree(parent_path, false, observer)?;
    let parent = BoundParent::open(parent_path)?;
    let required_mode = private.then_some(PRIVATE_FILE_MODE);
    let current = open_expected_file(&parent, target_name, path, expected, required_mode)?;
    let namespace =
        PrivateMutationDirectory::create(&parent.descriptor, parent_path, "integration-write")?;
    let mode = if private {
        PRIVATE_FILE_MODE
    } else {
        current
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .map(|metadata| metadata.permissions().mode() & 0o7777)
            .unwrap_or(PRIVATE_FILE_MODE)
    };
    let file = namespace.create_file(bytes, mode)?;
    namespace.verify_parent(&parent.descriptor)?;
    let intent = namespace.staged_write(
        path,
        current
            .as_ref()
            .map(|file| (file, expected.unwrap_or_default())),
        &file,
        bytes,
    )?;
    observer.prepare_write(intent)?;
    parent.verify()?;

    let pending_cleanup = if let Some(current) = current {
        validate_named_file(&parent, target_name, &current, path)?;
        before_commit();
        if let Err(error) = renameat_with(
            namespace.descriptor(),
            "entry",
            &parent.descriptor,
            target_name,
            RenameFlags::EXCHANGE,
        ) {
            let cleanup = namespace.cleanup_for_entry(&file)?;
            if let Err(cleanup_error) = observer.abort_mutation(path, cleanup) {
                return Err(Error::Cleanup {
                    primary: Box::new(errno_error(error)),
                    cleanup: Box::new(cleanup_error),
                });
            }
            return Err(errno_error(error));
        }

        let displaced_matches = (|| -> Result<bool> {
            let displaced = openat(
                namespace.descriptor(),
                "entry",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(errno_error)?;
            let displaced_stat = fstat(&displaced).map_err(errno_error)?;
            let expected_stat = fstat(&current).map_err(errno_error)?;
            let displaced_bytes =
                read_bounded_file(&displaced, MAX_INTEGRATION_JSON_BYTES as usize + 1)?;
            Ok(same_stat_identity(&displaced_stat, &expected_stat)
                && displaced_bytes == expected.unwrap_or_default())
        })();
        if !matches!(displaced_matches, Ok(true)) {
            renameat_with(
                namespace.descriptor(),
                "entry",
                &parent.descriptor,
                target_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(errno_error)?;
            parent.sync()?;
            let cleanup = namespace.cleanup_for_entry(&file)?;
            observer.abort_mutation(path, cleanup)?;
            displaced_matches?;
            return Err(invalid(&format!(
                "Integration file changed during compare-and-swap: {}",
                path.display()
            )));
        }
        parent.sync()?;
        namespace.cleanup_for_entry(&current)?
    } else {
        before_commit();
        match renameat_with(
            namespace.descriptor(),
            "entry",
            &parent.descriptor,
            target_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => parent.sync()?,
            Err(error) => {
                let cleanup = namespace.cleanup_for_entry(&file)?;
                let aborted = observer.abort_mutation(path, cleanup);
                if let Err(cleanup_error) = aborted {
                    return Err(Error::Cleanup {
                        primary: Box::new(if error == rustix::io::Errno::EXIST {
                            invalid(&format!(
                                "Integration file appeared during compare-and-swap: {}",
                                path.display()
                            ))
                        } else {
                            errno_error(error)
                        }),
                        cleanup: Box::new(cleanup_error),
                    });
                }
                if error == rustix::io::Errno::EXIST {
                    return Err(invalid(&format!(
                        "Integration file appeared during compare-and-swap: {}",
                        path.display()
                    )));
                }
                return Err(errno_error(error));
            }
        }
        namespace.cleanup_empty()
    };
    parent.verify()?;
    WriteReceipt::persisted_with_cleanup(path, &file, bytes, Some(pending_cleanup))
}

pub(crate) fn remove_managed_bytes_observed(
    path: &Path,
    expected: &[u8],
    observer: &mut impl MutationObserver,
) -> Result<()> {
    let receipt = remove_managed_path_observed_with_hook(path, expected, observer, || {})?;
    observer.record_removal(receipt)
}

#[cfg(test)]
fn remove_managed_path_with_hook(
    path: &Path,
    expected: &[u8],
    before_commit: impl FnOnce(),
) -> Result<WriteReceipt> {
    let mut observer = UnobservedMutation;
    let receipt =
        remove_managed_path_observed_with_hook(path, expected, &mut observer, before_commit)?;
    observer.record_removal(receipt.clone())?;
    Ok(receipt.without_cleanup())
}

fn remove_managed_path_observed_with_hook(
    path: &Path,
    expected: &[u8],
    observer: &mut impl MutationObserver,
    before_commit: impl FnOnce(),
) -> Result<WriteReceipt> {
    let (parent_path, target_name) = path_parts(path)?;
    let parent = BoundParent::open(parent_path)?;
    let current = open_expected_file(&parent, target_name, path, Some(expected), None)?
        .ok_or_else(|| {
            invalid(&format!(
                "Managed integration path disappeared: {}",
                path.display()
            ))
        })?;
    let namespace =
        PrivateMutationDirectory::create(&parent.descriptor, parent_path, "integration-remove")?;
    validate_named_file(&parent, target_name, &current, path)?;
    observer.prepare_removal(namespace.planned_removal(path, &current, expected)?)?;
    before_commit();
    renameat_with(
        &parent.descriptor,
        target_name,
        namespace.descriptor(),
        "entry",
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_error)?;
    let quarantined_matches = (|| -> Result<bool> {
        let quarantined = openat(
            namespace.descriptor(),
            "entry",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(errno_error)?;
        let current_stat = fstat(&current).map_err(errno_error)?;
        let quarantined_stat = fstat(&quarantined).map_err(errno_error)?;
        let quarantined_bytes =
            read_bounded_file(&quarantined, MAX_INTEGRATION_JSON_BYTES as usize + 1)?;
        Ok(same_stat_identity(&current_stat, &quarantined_stat) && quarantined_bytes == expected)
    })();
    if !matches!(quarantined_matches, Ok(true)) {
        renameat_with(
            namespace.descriptor(),
            "entry",
            &parent.descriptor,
            target_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(errno_error)?;
        parent.sync()?;
        observer.abort_mutation(path, namespace.cleanup_empty())?;
        quarantined_matches?;
        return Err(invalid(&format!(
            "Managed integration path changed during compare-and-swap: {}",
            path.display()
        )));
    }
    parent.sync()?;
    let cleanup = namespace.cleanup_for_entry(&current)?;
    WriteReceipt::removed_with_cleanup(path, Some(cleanup))
}

fn manifest_path(home: &Path, agent: &str) -> PathBuf {
    home.join("integrations").join(format!("{agent}.json"))
}

fn resolve_target(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink integration target"));
    }
    crate::dojo::resolve_home(path)
}

fn validate_hardknock_executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(invalid(
            "Hardknock executable path must be absolute and normalized",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(invalid(
            "Hardknock executable must be a regular, executable, non-writable-by-others file",
        ));
    }
    let uid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != uid && metadata.uid() != 0 {
        return Err(invalid(
            "Hardknock executable must be owned by the effective user or root",
        ));
    }
    Ok(path.to_owned())
}

fn adapter_files(agent: &str) -> Result<Vec<(&'static str, &'static str)>> {
    Ok(match agent {
        "hermes" => vec![
            (
                "plugin.yaml",
                include_str!("../../integrations/hermes/plugin.yaml"),
            ),
            (
                "__init__.py",
                include_str!("../../integrations/hermes/__init__.py"),
            ),
        ],
        "openclaw" => vec![
            (
                "package.json",
                include_str!("../../integrations/openclaw/package.json"),
            ),
            (
                "openclaw.plugin.json",
                include_str!("../../integrations/openclaw/openclaw.plugin.json"),
            ),
            (
                "index.ts",
                include_str!("../../integrations/openclaw/index.ts"),
            ),
            (
                "hooks.mjs",
                include_str!("../../integrations/openclaw/hooks.mjs"),
            ),
            (
                "bridge.mjs",
                include_str!("../../integrations/openclaw/bridge.mjs"),
            ),
        ],
        _ => return Err(invalid("Unknown plugin")),
    })
}

pub fn plan(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<IntegrationPlan> {
    let action = match command {
        AdapterCommand::Check => {
            return Err(invalid(
                "Check is observational and does not produce an integration mutation plan",
            ));
        }
        AdapterCommand::Install { .. } => IntegrationAction::Install,
        AdapterCommand::Uninstall { .. } => IntegrationAction::Uninstall,
    };
    if !matches!(agent, "claude" | "hermes" | "openclaw") {
        return Err(invalid("Unknown adapter"));
    }

    let home = crate::dojo::resolve_home(home)?;
    validate_existing_private_directory(&home, "HARDKNOCK_HOME")?;
    let integration_directory = home.join("integrations");
    validate_existing_private_directory(&integration_directory, "Managed integration directory")?;
    let manifest_path = manifest_path(&home, agent);
    let previous = read_manifest(&manifest_path)?;
    let override_path = match command {
        AdapterCommand::Install { config } | AdapterCommand::Uninstall { config } => config,
        AdapterCommand::Check => unreachable!(),
    };
    let requested_path = if action == IntegrationAction::Uninstall && override_path.is_none() {
        previous["path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(path_for(agent, override_path)?)
    } else {
        path_for(agent, override_path)?
    };
    let target_path = resolve_target(&requested_path)?;
    if let Some(previous_path) = previous["path"].as_str() {
        let previous_path = resolve_target(Path::new(previous_path))?;
        if previous_path != target_path {
            return Err(invalid(
                "Uninstall the existing managed integration before changing its location",
            ));
        }
    }

    let executable = if agent == "claude" && action == IntegrationAction::Install {
        Some(validate_hardknock_executable(hardknock_executable)?)
    } else {
        None
    };
    let (config_path, managed_paths) = if agent == "claude" {
        read_json(&target_path)?;
        (
            target_path.clone(),
            vec![target_path.clone(), manifest_path.clone()],
        )
    } else {
        if target_path.exists() {
            if previous["path"].is_null()
                && action == IntegrationAction::Install
                && fs::read_dir(&target_path)?.next().is_some()
            {
                return Err(invalid(
                    "Refusing to overwrite an unmanaged plugin directory",
                ));
            }
            validate_existing_private_directory(&target_path, "Managed plugin directory")?;
        }
        let files = adapter_files(agent)?;
        let config_name = if agent == "hermes" {
            "plugin.yaml"
        } else {
            "openclaw.plugin.json"
        };
        let mut managed_paths = files
            .iter()
            .map(|(name, _)| target_path.join(name))
            .collect::<Vec<_>>();
        for managed_path in &managed_paths {
            validate_existing_private_file(managed_path, "Managed plugin file")?;
        }
        managed_paths.push(manifest_path.clone());
        (target_path.join(config_name), managed_paths)
    };

    let action_summary = match (action, agent) {
        (IntegrationAction::Install, "claude") => {
            "Install managed Claude lifecycle hooks".to_owned()
        }
        (IntegrationAction::Uninstall, "claude") => {
            "Remove managed Claude lifecycle hooks".to_owned()
        }
        (IntegrationAction::Install, _) => format!("Install managed {agent} plugin files"),
        (IntegrationAction::Uninstall, _) => format!("Remove managed {agent} plugin files"),
    };
    Ok(IntegrationPlan {
        agent: agent.to_owned(),
        action,
        action_summary,
        home_path: home,
        target_path,
        config_path,
        manifest_path,
        managed_paths,
        hardknock_executable: executable,
    })
}

pub fn describe_plan(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<Value> {
    plan(agent, home, command, hardknock_executable)?.description()
}

pub fn installed(agent: &str, home: &Path) -> bool {
    if agent == "codex" {
        return find_executable("codex").is_some();
    }
    let Ok(manifest) = read_manifest(&manifest_path(home, agent)) else {
        return false;
    };
    let Some(path) = manifest["path"].as_str() else {
        return false;
    };
    if agent == "claude" {
        let Ok(settings) = read_json(Path::new(path)) else {
            return false;
        };
        let Some(command) = manifest["command"].as_str() else {
            return false;
        };
        return CLAUDE_EVENTS.iter().all(|event| {
            settings["hooks"][event].as_array().is_some_and(|groups| {
                groups.iter().any(|g| {
                    g["hooks"]
                        .as_array()
                        .is_some_and(|h| h.iter().any(|h| h["command"] == command))
                })
            })
        });
    }
    let Ok(files) = adapter_files(agent) else {
        return false;
    };
    files.iter().all(|(name, expected)| {
        let target = Path::new(path).join(name);
        read_bounded_user_file(
            &target,
            "Managed plugin file",
            MAX_INTEGRATION_JSON_BYTES,
            Some(PRIVATE_FILE_MODE),
        )
        .is_ok_and(|bytes| bytes.as_deref() == Some(expected.as_bytes()))
    })
}

pub fn manage(agent: &str, home: &Path, command: &AdapterCommand) -> Result<Value> {
    if matches!(command, AdapterCommand::Check) {
        return Ok(
            json!({"agent":agent,"executable_found":find_executable(agent).is_some(),"installed":installed(agent,home)}),
        );
    }
    let executable = env::current_exe()?;
    manage_with_executable(agent, home, command, &executable)
}

pub fn manage_with_executable(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<Value> {
    if matches!(command, AdapterCommand::Check) {
        return manage(agent, home, command);
    }
    let integration_plan = plan(agent, home, command, hardknock_executable)?;
    apply(&integration_plan)
}

pub fn apply(integration_plan: &IntegrationPlan) -> Result<Value> {
    apply_observed(integration_plan, &mut UnobservedMutation)
}

pub(crate) fn apply_observed(
    integration_plan: &IntegrationPlan,
    observer: &mut impl MutationObserver,
) -> Result<Value> {
    let command = match integration_plan.action {
        IntegrationAction::Install => AdapterCommand::Install {
            config: Some(integration_plan.target_path.clone()),
        },
        IntegrationAction::Uninstall => AdapterCommand::Uninstall {
            config: Some(integration_plan.target_path.clone()),
        },
    };
    let executable = integration_plan
        .hardknock_executable
        .as_deref()
        .unwrap_or(Path::new("/"));
    let current = plan(
        &integration_plan.agent,
        &integration_plan.home_path,
        &command,
        executable,
    )?;
    if current != *integration_plan {
        return Err(invalid(
            "Integration plan no longer matches the filesystem; create a new plan",
        ));
    }

    let install = integration_plan.action == IntegrationAction::Install;
    let path = &integration_plan.target_path;
    let manifest_path = &integration_plan.manifest_path;
    let (previous, previous_manifest_bytes) = read_manifest_with_bytes(manifest_path)?;
    let mut preserved_modified = Vec::new();

    // Do not open a domain Store from an adapter installer.
    if install {
        let home = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("Managed integration manifest has no home directory"))?;
        ensure_directory_tree(home, true, observer)?;
        let integration_directory = manifest_path
            .parent()
            .ok_or_else(|| invalid("Managed integration manifest has no parent"))?;
        ensure_directory_tree(integration_directory, true, observer)?;
    }

    if integration_plan.agent == "claude" {
        let (mut value, previous_settings_bytes) = read_json_with_bytes(path)?;
        if let Some(hooks) = value.get("hooks")
            && !hooks.is_object()
        {
            return Err(invalid("Claude hooks must be an object"));
        }
        if value.get("hooks").is_none() {
            value["hooks"] = json!({});
        }
        let hooks = value["hooks"]
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid hooks"))?;
        // Remove only the exact previously installed command, retaining other hooks in each group.
        if let Some(old) = previous["command"].as_str() {
            for groups in hooks.values_mut() {
                if let Some(groups) = groups.as_array_mut() {
                    for group in groups.iter_mut() {
                        if let Some(items) = group["hooks"].as_array_mut() {
                            items.retain(|h| h["command"] != old);
                        }
                    }
                    groups.retain(|group| {
                        group["hooks"]
                            .as_array()
                            .is_none_or(|items| !items.is_empty())
                    });
                }
            }
        }
        let command = format!(
            "{} --home {} integration-event --agent claude",
            shell_words::quote(
                &integration_plan
                    .hardknock_executable
                    .as_deref()
                    .unwrap_or(executable)
                    .to_string_lossy()
            ),
            shell_words::quote(&integration_plan.home_path.to_string_lossy())
        );
        if install {
            for event in CLAUDE_EVENTS {
                let groups = hooks
                    .entry((*event).to_owned())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or_else(|| invalid("Claude hook event must be an array"))?;
                groups.push(json!({"matcher":"","hooks":[{"type":"command","command":command,"timeout":10}]}));
            }
        }
        // Validate and fully serialize before replacing user settings.
        let settings = serde_json::to_vec_pretty(&value)?;
        write_managed_bytes_observed(
            path,
            &settings,
            false,
            previous_settings_bytes.as_deref(),
            observer,
        )?;
        if install {
            let manifest = serde_json::to_vec(&json!({"path":path,"command":command}))?;
            write_managed_bytes_observed(
                manifest_path,
                &manifest,
                true,
                previous_manifest_bytes.as_deref(),
                observer,
            )?;
        }
    } else {
        let files = adapter_files(&integration_plan.agent)?;
        if install {
            ensure_directory_tree(path, true, observer)?;
            for (name, contents) in &files {
                let target = path.join(name);
                let previous_file = read_bounded_user_file(
                    &target,
                    "Managed plugin file",
                    MAX_INTEGRATION_JSON_BYTES,
                    Some(PRIVATE_FILE_MODE),
                )?;
                write_managed_bytes_observed(
                    &target,
                    contents.as_bytes(),
                    true,
                    previous_file.as_deref(),
                    observer,
                )?;
            }
            let manifest = serde_json::to_vec(&json!({
                "path":path,
                "files":files.iter().map(|(name, contents)| json!({
                    "name":name,
                    "blake3":blake3::hash(contents.as_bytes()).to_hex().to_string()
                })).collect::<Vec<_>>()
            }))?;
            write_managed_bytes_observed(
                manifest_path,
                &manifest,
                true,
                previous_manifest_bytes.as_deref(),
                observer,
            )?;
        } else if !previous["path"].is_null() {
            // Never recursively delete a plugin directory that might contain user files.
            for (name, expected) in &files {
                let target = path.join(name);
                if let Some(bytes) = read_bounded_user_file(
                    &target,
                    "Managed plugin file",
                    MAX_INTEGRATION_JSON_BYTES,
                    Some(PRIVATE_FILE_MODE),
                )? {
                    if bytes == expected.as_bytes() {
                        remove_managed_bytes_observed(&target, &bytes, observer)?;
                    } else {
                        preserved_modified.push(target);
                    }
                }
            }
        }
    }
    if !install {
        match fs::symlink_metadata(manifest_path) {
            Ok(_) => {
                remove_managed_bytes_observed(
                    manifest_path,
                    previous_manifest_bytes.as_deref().ok_or_else(|| {
                        invalid("Managed integration manifest appeared during uninstall")
                    })?,
                    observer,
                )?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(
        json!({"agent":integration_plan.agent,"installed":install,"path":path,"preserved_modified":preserved_modified,"note":if integration_plan.agent=="openclaw"{"Files installed. Enable hardknock with OpenClaw plugin allow/enable configuration; this command does not broaden trust."}else{"Restart the agent to load changed hooks/plugins."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn executable(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn plan_is_exact_redacted_and_does_not_mutate() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock home");
        let config = temporary.path().join("claude settings.json");
        let executable = executable(temporary.path(), "stable hardknock");
        fs::write(&config, br#"{"api_token":"do-not-serialize","hooks":{}}"#).unwrap();

        let plan = plan(
            "claude",
            &home,
            &AdapterCommand::Install {
                config: Some(config.clone()),
            },
            &executable,
        )
        .unwrap();
        let resolved_home = crate::dojo::resolve_home(&home).unwrap();
        let resolved_config = config.canonicalize().unwrap();

        assert_eq!(plan.action, IntegrationAction::Install);
        assert_eq!(plan.home_path, resolved_home);
        assert_eq!(plan.target_path, resolved_config);
        assert_eq!(plan.config_path, resolved_config);
        assert_eq!(
            plan.manifest_path,
            resolved_home.join("integrations/claude.json")
        );
        assert_eq!(
            plan.managed_paths,
            vec![
                resolved_config,
                resolved_home.join("integrations/claude.json")
            ]
        );
        assert_eq!(
            plan.action_summary,
            "Install managed Claude lifecycle hooks"
        );
        let description = serde_json::to_string(&plan.description().unwrap()).unwrap();
        assert!(!description.contains("do-not-serialize"));
        assert!(!resolved_home.exists());
    }

    #[test]
    fn apply_uses_explicit_executable_and_secures_managed_state() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let config = temporary.path().join("settings.json");
        let executable = executable(temporary.path(), "hardknock stable");
        let plan = plan(
            "claude",
            &home,
            &AdapterCommand::Install {
                config: Some(config.clone()),
            },
            &executable,
        )
        .unwrap();

        apply(&plan).unwrap();

        let settings = read_json(&config).unwrap();
        let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let executable_text = executable.to_string_lossy().into_owned();
        let home_text = home.to_string_lossy().into_owned();
        let quoted_executable = shell_words::quote(&executable_text);
        let quoted_home = shell_words::quote(&home_text);
        assert!(command.starts_with(quoted_executable.as_ref()));
        assert!(command.contains(quoted_home.as_ref()));
        assert_eq!(
            fs::symlink_metadata(home.join("integrations"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(home.join("integrations/claude.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn apply_refuses_a_plan_that_became_unmanaged() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let target = temporary.path().join("hermes");
        let executable = executable(temporary.path(), "stable-hardknock");
        let plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Install {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("user.txt"), "user data").unwrap();

        assert!(apply(&plan).is_err());
        assert!(!target.join("plugin.yaml").exists());
        assert!(!home.exists());
    }

    #[test]
    fn bounded_json_read_rejects_symlinks_non_regular_files_and_sparse_oversize() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target.json");
        let linked = temporary.path().join("linked.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(
            read_json(&linked)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );

        let hardlinked = temporary.path().join("hardlinked.json");
        fs::hard_link(&target, &hardlinked).unwrap();
        assert!(
            read_json(&target)
                .unwrap_err()
                .to_string()
                .contains("singly linked")
        );

        let directory = temporary.path().join("directory.json");
        fs::create_dir(&directory).unwrap();
        assert!(
            read_json(&directory)
                .unwrap_err()
                .to_string()
                .contains("regular file")
        );

        let oversized = temporary.path().join("oversized.json");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_INTEGRATION_JSON_BYTES + 1)
            .unwrap();
        assert!(
            read_json(&oversized)
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
    }

    #[test]
    fn bounded_json_read_enforces_the_limit_when_the_open_file_grows() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");
        fs::write(&path, "{}").unwrap();
        let mut opened = open_bounded_user_file(
            &path,
            "Integration configuration",
            MAX_INTEGRATION_JSON_BYTES,
            None,
        )
        .unwrap()
        .unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_INTEGRATION_JSON_BYTES + 1)
            .unwrap();

        assert!(
            read_opened_user_file(
                &path,
                &mut opened,
                "Integration configuration",
                MAX_INTEGRATION_JSON_BYTES,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("byte limit")
        );
    }

    #[test]
    fn bounded_json_read_rejects_path_replacement_after_open() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");
        let original = temporary.path().join("original.json");
        fs::write(&path, "{}").unwrap();
        let mut opened = open_bounded_user_file(
            &path,
            "Integration configuration",
            MAX_INTEGRATION_JSON_BYTES,
            None,
        )
        .unwrap()
        .unwrap();
        fs::rename(&path, &original).unwrap();
        fs::write(&path, "{}").unwrap();

        assert!(
            read_opened_user_file(
                &path,
                &mut opened,
                "Integration configuration",
                MAX_INTEGRATION_JSON_BYTES,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("changed while it was being read")
        );
    }

    #[test]
    fn planning_refuses_insecure_manifests() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = executable(temporary.path(), "stable-hardknock");
        let home = temporary.path().join("hardknock");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join("integrations")).unwrap();
        fs::set_permissions(home.join("integrations"), fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = home.join("integrations/hermes.json");
        fs::write(&manifest, "{}").unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            plan(
                "hermes",
                &home,
                &AdapterCommand::Uninstall { config: None },
                &executable,
            )
            .is_err()
        );
    }

    #[test]
    fn uninstall_preserves_modified_managed_plugin_files() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let target = temporary.path().join("hermes");
        let executable = executable(temporary.path(), "stable-hardknock");
        let install_plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Install {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        apply(&install_plan).unwrap();
        fs::write(target.join("plugin.yaml"), "user-modified").unwrap();

        let uninstall_plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Uninstall {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        let report = apply(&uninstall_plan).unwrap();

        assert_eq!(
            fs::read_to_string(target.join("plugin.yaml")).unwrap(),
            "user-modified"
        );
        assert!(!target.join("__init__.py").exists());
        assert_eq!(report["preserved_modified"].as_array().unwrap().len(), 1);
        assert!(!home.join("integrations/hermes.json").exists());
    }

    #[test]
    fn atomic_write_preserves_a_concurrent_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");
        let original = temporary.path().join("settings.original");
        fs::write(&path, b"old").unwrap();

        let error = atomic_write_with_hook(&path, b"managed", false, Some(b"old"), || {
            fs::rename(&path, &original).unwrap();
            fs::write(&path, b"concurrent").unwrap();
        })
        .unwrap_err();

        assert!(
            error.to_string().contains("compare-and-swap"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"concurrent");
        assert_eq!(fs::read(&original).unwrap(), b"old");
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".hardknock-integration-write-")
        }));
    }

    #[test]
    fn atomic_create_refuses_a_concurrent_file() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");

        let error = atomic_write_with_hook(&path, b"managed", false, None, || {
            fs::write(&path, b"concurrent").unwrap();
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("appeared during compare-and-swap")
        );
        assert_eq!(fs::read(&path).unwrap(), b"concurrent");
    }

    #[test]
    fn managed_remove_preserves_a_concurrent_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("plugin.yaml");
        let original = temporary.path().join("plugin.original");
        fs::write(&path, b"managed").unwrap();

        let error = remove_managed_path_with_hook(&path, b"managed", || {
            fs::rename(&path, &original).unwrap();
            fs::write(&path, b"concurrent").unwrap();
        })
        .unwrap_err();

        assert!(
            error.to_string().contains("compare-and-swap"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"concurrent");
        assert_eq!(fs::read(&original).unwrap(), b"managed");
    }
}
