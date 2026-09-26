// SPDX-License-Identifier: Apache-2.0

//! Safe planning and management of the per-user Hardknock Bridge service.
//!
//! The setup command can build a [`ServicePlan`] without changing the host,
//! present that plan to the caller, and then call [`ServicePlan::apply`] or
//! [`ServicePlan::uninstall`]. Every mutating operation repeats the ownership,
//! permission, and symlink checks performed while planning.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, killpg},
    unistd::{Pid, geteuid},
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const SYSTEMD_UNIT_NAME: &str = "hardknock-bridge.service";
const LAUNCHD_LABEL: &str = "dev.openkedge.hardknock.bridge";
const MANAGED_MARKER: &str = "Managed by Hardknock service setup; schema=1";
const COMMAND_TIMEOUT_MS: u64 = 10_000;
const CAPTURE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}

/// Host family used to select the native per-user service manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePlatform {
    Linux,
    Macos,
    Other,
}

impl ServicePlatform {
    pub fn native() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Other
        }
    }
}

/// Caller preference for service-manager selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedService {
    #[default]
    Auto,
    SystemdUser,
    Launchd,
    OnDemand,
}

/// Selected service mechanism and the executable used to control it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceManager {
    SystemdUser { executable: PathBuf },
    Launchd { executable: PathBuf },
    OnDemand { reason: String },
}

/// Expected filesystem change when a plan is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceChange {
    Create,
    ReplaceManaged,
    Unchanged,
    OnDemand,
}

/// One bounded service-manager command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

/// Serializable, mutation-free result of service planning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePlan {
    pub manager: ServiceManager,
    pub target: Option<PathBuf>,
    pub security_root: Option<PathBuf>,
    #[serde(skip)]
    pub content: Option<String>,
    pub change: ServiceChange,
    pub on_demand_command: String,
    pub fallback: String,
}

impl ServicePlan {
    /// Detect and plan the requested per-user service without changing state.
    pub fn detect(
        user_home: PathBuf,
        hardknock_home: PathBuf,
        executable: PathBuf,
        requested: RequestedService,
    ) -> Result<Self> {
        plan(&ServiceOptions {
            platform: ServicePlatform::native(),
            requested,
            user_home,
            hardknock_home,
            executable,
            config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            manager_executable: None,
        })
    }

    /// Files owned by this plan. On-demand plans own no service definition.
    pub fn managed_paths(&self) -> Vec<PathBuf> {
        self.target.iter().cloned().collect()
    }

    /// Install or refresh the definition and optionally start the service.
    pub async fn apply(&self, start: bool) -> Result<ServiceReport> {
        self.apply_with_options(start, false).await
    }

    /// Apply with an explicit dry-run switch for setup previews.
    pub async fn apply_with_options(&self, start: bool, dry_run: bool) -> Result<ServiceReport> {
        let plan = self.clone();
        tokio::task::spawn_blocking(move || apply_sync(&plan, start, dry_run))
            .await
            .map_err(|error| Error::Intervention(format!("Service apply task failed: {error}")))?
    }

    /// Stop the managed service when requested, then remove its exact definition.
    pub async fn uninstall(&self, stop: bool) -> Result<ServiceReport> {
        self.uninstall_with_options(stop, false).await
    }

    /// Uninstall with an explicit dry-run switch for setup previews.
    pub async fn uninstall_with_options(&self, stop: bool, dry_run: bool) -> Result<ServiceReport> {
        let plan = self.clone();
        tokio::task::spawn_blocking(move || remove_sync(&plan, stop, dry_run))
            .await
            .map_err(|error| Error::Intervention(format!("Service removal task failed: {error}")))?
    }
}

/// Inputs used to render native paths and service definitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceOptions {
    pub platform: ServicePlatform,
    pub requested: RequestedService,
    pub user_home: PathBuf,
    pub hardknock_home: PathBuf,
    pub executable: PathBuf,
    pub config_home: Option<PathBuf>,
    pub manager_executable: Option<PathBuf>,
}

impl ServiceOptions {
    pub fn native(
        user_home: PathBuf,
        hardknock_home: PathBuf,
        executable: PathBuf,
        requested: RequestedService,
    ) -> Self {
        Self {
            platform: ServicePlatform::native(),
            requested,
            user_home,
            hardknock_home,
            executable,
            config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            manager_executable: None,
        }
    }
}

/// Outcome of one manager command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Succeeded,
    Failed,
    TimedOut,
    Unavailable,
}

/// Bounded command evidence suitable for JSON output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReport {
    pub command: ManagerCommand,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Structured result of applying or removing a service plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReport {
    pub manager: ServiceManager,
    pub target: Option<PathBuf>,
    pub dry_run: bool,
    pub changed: bool,
    pub removed: bool,
    pub commands: Vec<CommandReport>,
    pub manager_ready: bool,
    pub fallback: Option<String>,
}

/// Detect the native manager from the current `PATH` without changing state.
pub fn detect_manager(platform: ServicePlatform) -> ServiceManager {
    detect_manager_in_path(platform, std::env::var_os("PATH").as_deref())
}

/// Detect a manager in an explicitly supplied search path.
///
/// This is useful to make setup planning deterministic and to test it without
/// invoking a host service manager.
pub fn detect_manager_in_path(platform: ServicePlatform, path: Option<&OsStr>) -> ServiceManager {
    let (name, constructor): (&str, fn(PathBuf) -> ServiceManager) = match platform {
        ServicePlatform::Linux => ("systemctl", |executable| ServiceManager::SystemdUser {
            executable,
        }),
        ServicePlatform::Macos => ("launchctl", |executable| ServiceManager::Launchd {
            executable,
        }),
        ServicePlatform::Other => {
            return ServiceManager::OnDemand {
                reason: "No supported per-user service manager exists on this platform".into(),
            };
        }
    };
    let Some(path) = path else {
        return ServiceManager::OnDemand {
            reason: format!("{name} was not found because PATH is unavailable"),
        };
    };
    for directory in std::env::split_paths(path) {
        let candidate = directory.join(name);
        if is_executable_file(&candidate) {
            return constructor(candidate);
        }
    }
    ServiceManager::OnDemand {
        reason: format!("{name} was not found in PATH"),
    }
}

/// Build a complete service plan without changing files or starting services.
pub fn plan(options: &ServiceOptions) -> Result<ServicePlan> {
    validate_absolute_path(&options.user_home, "user home")?;
    validate_absolute_path(&options.hardknock_home, "Hardknock home")?;
    validate_absolute_path(&options.executable, "Hardknock executable")?;
    validate_executable(&options.executable)?;

    let on_demand_command = render_on_demand_command(&options.executable, &options.hardknock_home)?;
    let manager = select_manager(options)?;
    if let ServiceManager::OnDemand { reason } = &manager {
        return Ok(ServicePlan {
            manager: manager.clone(),
            target: None,
            security_root: None,
            content: None,
            change: ServiceChange::OnDemand,
            on_demand_command: on_demand_command.clone(),
            fallback: format!("{reason}. Start the Bridge on demand with: {on_demand_command}"),
        });
    }

    let (target, security_root, content) = match &manager {
        ServiceManager::SystemdUser { .. } => {
            let config_home = options
                .config_home
                .clone()
                .unwrap_or_else(|| options.user_home.join(".config"));
            validate_absolute_path(&config_home, "configuration home")?;
            (
                config_home.join("systemd/user").join(SYSTEMD_UNIT_NAME),
                if config_home.starts_with(&options.user_home) {
                    options.user_home.clone()
                } else {
                    config_home
                },
                render_systemd(&options.executable, &options.hardknock_home)?,
            )
        }
        ServiceManager::Launchd { .. } => (
            options
                .user_home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            options.user_home.clone(),
            render_launchd(&options.executable, &options.hardknock_home)?,
        ),
        ServiceManager::OnDemand { .. } => unreachable!(),
    };
    validate_managed_path(&security_root, &target, geteuid().as_raw(), false)?;
    let change = classify_target(&target, &content, geteuid().as_raw())?;
    let fallback = format!(
        "If the service manager is unavailable, start the Bridge with: {on_demand_command}"
    );
    Ok(ServicePlan {
        manager,
        target: Some(target),
        security_root: Some(security_root),
        content: Some(content),
        change,
        on_demand_command,
        fallback,
    })
}

/// Apply a previously rendered plan.
///
/// Manager failures are returned in [`ServiceReport::commands`] so callers can
/// preserve the installed definition and present the on-demand fallback.
fn apply_sync(plan: &ServicePlan, start: bool, dry_run: bool) -> Result<ServiceReport> {
    let mut report = base_report(plan, dry_run);
    if matches!(plan.manager, ServiceManager::OnDemand { .. }) {
        report.fallback = Some(plan.fallback.clone());
        return Ok(report);
    }
    let (target, root, content) = plan_parts(plan)?;
    validate_managed_path(root, target, geteuid().as_raw(), false)?;
    let current = classify_target(target, content, geteuid().as_raw())?;
    if current != plan.change {
        return Err(invalid(format!(
            "Service target changed after planning: planned {:?}, now {:?}; create a new service plan",
            plan.change, current
        )));
    }
    if dry_run {
        report.changed = !matches!(current, ServiceChange::Unchanged);
        return Ok(report);
    }
    ensure_secure_directory_tree(
        root,
        target
            .parent()
            .ok_or_else(|| invalid(format!("Service target {} has no parent", target.display())))?,
    )?;
    validate_managed_path(root, target, geteuid().as_raw(), true)?;
    if !matches!(current, ServiceChange::Unchanged) {
        atomic_write_managed(target, content)?;
        report.changed = true;
    }
    validate_exact_target(target, content, geteuid().as_raw())?;
    let (commands, _) = manager_commands(&plan.manager, target, start, current);
    report.commands = run_commands(&commands);
    report.manager_ready = commands_succeeded(&report.commands);
    if !report.manager_ready {
        report.fallback = Some(plan.fallback.clone());
    }
    Ok(report)
}

/// Remove only the exact service definition represented by the plan.
fn remove_sync(plan: &ServicePlan, stop: bool, dry_run: bool) -> Result<ServiceReport> {
    let mut report = base_report(plan, dry_run);
    if matches!(plan.manager, ServiceManager::OnDemand { .. }) {
        return Ok(report);
    }
    let (target, root, content) = plan_parts(plan)?;
    validate_managed_path(root, target, geteuid().as_raw(), false)?;
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error.into()),
    };
    validate_target_metadata(target, &metadata, geteuid().as_raw())?;
    let existing = read_bounded(target, content.len().max(4096).saturating_add(1))?;
    if existing != content.as_bytes() {
        return Err(invalid(format!(
            "Refusing to remove {}; its content does not exactly match the managed service plan",
            target.display()
        )));
    }
    if dry_run {
        report.changed = true;
        report.removed = true;
        return Ok(report);
    }
    let (_, commands) = manager_commands(&plan.manager, target, stop, plan.change);
    report.commands = run_commands(&commands);
    report.manager_ready = commands_succeeded(&report.commands);
    if !report.manager_ready && !commands.is_empty() {
        report.fallback = Some(format!(
            "The service definition is still present. Stop it manually, then retry removal. {}",
            plan.fallback
        ));
        return Ok(report);
    }
    validate_exact_target(target, content, geteuid().as_raw())?;
    fs::remove_file(target)?;
    sync_directory(
        target
            .parent()
            .ok_or_else(|| invalid(format!("Service target {} has no parent", target.display())))?,
    )?;
    let reload_reports = run_commands(&post_remove_commands(&plan.manager, stop));
    report.manager_ready &= commands_succeeded(&reload_reports);
    report.commands.extend(reload_reports);
    if !report.manager_ready {
        report.fallback = Some(format!(
            "The service definition was removed, but the service manager could not reload it. {}",
            plan.fallback
        ));
    }
    report.changed = true;
    report.removed = true;
    Ok(report)
}

fn select_manager(options: &ServiceOptions) -> Result<ServiceManager> {
    if options.requested == RequestedService::OnDemand {
        return Ok(ServiceManager::OnDemand {
            reason: "On-demand operation was requested".into(),
        });
    }
    let platform = match options.requested {
        RequestedService::Auto => options.platform,
        RequestedService::SystemdUser => ServicePlatform::Linux,
        RequestedService::Launchd => ServicePlatform::Macos,
        RequestedService::OnDemand => unreachable!(),
    };
    if let Some(executable) = &options.manager_executable {
        validate_absolute_path(executable, "service manager executable")?;
        if !is_executable_file(executable) {
            return Ok(ServiceManager::OnDemand {
                reason: format!(
                    "Configured service manager {} is unavailable or not executable",
                    executable.display()
                ),
            });
        }
        return Ok(match platform {
            ServicePlatform::Linux => ServiceManager::SystemdUser {
                executable: executable.clone(),
            },
            ServicePlatform::Macos => ServiceManager::Launchd {
                executable: executable.clone(),
            },
            ServicePlatform::Other => ServiceManager::OnDemand {
                reason: "No supported per-user service manager exists on this platform".into(),
            },
        });
    }
    Ok(detect_manager(platform))
}

fn manager_commands(
    manager: &ServiceManager,
    target: &Path,
    start: bool,
    change: ServiceChange,
) -> (Vec<ManagerCommand>, Vec<ManagerCommand>) {
    match manager {
        ServiceManager::SystemdUser { executable } => {
            let mut apply = Vec::new();
            if start {
                apply.push(command(executable, &["--user", "daemon-reload"]));
                apply.push(command(
                    executable,
                    &["--user", "enable", "--now", SYSTEMD_UNIT_NAME],
                ));
            }
            let remove = if start {
                vec![command(
                    executable,
                    &["--user", "disable", "--now", SYSTEMD_UNIT_NAME],
                )]
            } else {
                Vec::new()
            };
            (apply, remove)
        }
        ServiceManager::Launchd { executable } => {
            let domain = format!("gui/{}", geteuid().as_raw());
            let apply = if !start {
                Vec::new()
            } else if matches!(change, ServiceChange::Unchanged) {
                vec![command(
                    executable,
                    &["kickstart", "-k", &format!("{domain}/{LAUNCHD_LABEL}")],
                )]
            } else {
                vec![command(
                    executable,
                    &["bootstrap", &domain, &target.to_string_lossy()],
                )]
            };
            let remove = if start {
                vec![command(
                    executable,
                    &["bootout", &domain, &target.to_string_lossy()],
                )]
            } else {
                Vec::new()
            };
            (apply, remove)
        }
        ServiceManager::OnDemand { .. } => (Vec::new(), Vec::new()),
    }
}

fn post_remove_commands(manager: &ServiceManager, stopped: bool) -> Vec<ManagerCommand> {
    if !stopped {
        return Vec::new();
    }
    match manager {
        ServiceManager::SystemdUser { executable } => {
            vec![command(executable, &["--user", "daemon-reload"])]
        }
        ServiceManager::Launchd { .. } | ServiceManager::OnDemand { .. } => Vec::new(),
    }
}

fn command(program: &Path, args: &[&str]) -> ManagerCommand {
    ManagerCommand {
        program: program.to_path_buf(),
        args: args.iter().map(|value| (*value).to_owned()).collect(),
        timeout_ms: COMMAND_TIMEOUT_MS,
    }
}

fn base_report(plan: &ServicePlan, dry_run: bool) -> ServiceReport {
    ServiceReport {
        manager: plan.manager.clone(),
        target: plan.target.clone(),
        dry_run,
        changed: false,
        removed: false,
        commands: Vec::new(),
        manager_ready: true,
        fallback: None,
    }
}

fn plan_parts(plan: &ServicePlan) -> Result<(&Path, &Path, &str)> {
    Ok((
        plan.target
            .as_deref()
            .ok_or_else(|| invalid("Native service plan has no target"))?,
        plan.security_root
            .as_deref()
            .ok_or_else(|| invalid("Native service plan has no security root"))?,
        plan.content
            .as_deref()
            .ok_or_else(|| invalid("Native service plan has no rendered content"))?,
    ))
}

fn commands_succeeded(reports: &[CommandReport]) -> bool {
    reports
        .iter()
        .all(|report| report.status == CommandStatus::Succeeded)
}

fn run_commands(commands: &[ManagerCommand]) -> Vec<CommandReport> {
    let mut reports = Vec::new();
    for command in commands {
        let report = run_command(command);
        let succeeded = report.status == CommandStatus::Succeeded;
        reports.push(report);
        if !succeeded {
            break;
        }
    }
    reports
}

fn run_command(spec: &ManagerCommand) -> CommandReport {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandReport {
                command: spec.clone(),
                status: CommandStatus::Unavailable,
                exit_code: None,
                stdout: String::new(),
                stderr: format!(
                    "Could not start service manager {}: {error}",
                    spec.program.display()
                ),
                stdout_truncated: false,
                stderr_truncated: false,
            };
        }
    };
    let process_group = i32::try_from(child.id()).ok().map(Pid::from_raw);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = spawn_capture(stdout);
    let stderr_reader = spawn_capture(stderr);
    let started = Instant::now();
    let deadline = Duration::from_millis(spec.timeout_ms.clamp(1, COMMAND_TIMEOUT_MS));
    let (status, exit_code) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_remaining_process_group(process_group);
                break (
                    if status.success() {
                        CommandStatus::Succeeded
                    } else {
                        CommandStatus::Failed
                    },
                    status.code(),
                );
            }
            Ok(None) if started.elapsed() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_command(&mut child, process_group);
                let exit_code = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code());
                break (CommandStatus::TimedOut, exit_code);
            }
            Err(error) => {
                terminate_command(&mut child, process_group);
                let (stdout, stdout_truncated) =
                    receive_capture(stdout_reader, "stdout capture stream");
                let (mut stderr, stderr_truncated) =
                    receive_capture(stderr_reader, "stderr capture stream");
                if !stderr.is_empty() {
                    stderr.push('\n');
                }
                stderr.push_str(&format!("Could not wait for service manager: {error}"));
                return CommandReport {
                    command: spec.clone(),
                    status: CommandStatus::Failed,
                    exit_code: None,
                    stdout,
                    stderr,
                    stdout_truncated,
                    stderr_truncated,
                };
            }
        }
    };
    let (stdout, stdout_truncated) = receive_capture(stdout_reader, "stdout capture stream");
    let (stderr, stderr_truncated) = receive_capture(stderr_reader, "stderr capture stream");
    CommandReport {
        command: spec.clone(),
        status,
        exit_code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    }
}

fn spawn_capture<T>(stream: Option<T>) -> Receiver<(String, bool)>
where
    T: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(capture_output(stream));
    });
    receiver
}

fn kill_remaining_process_group(process_group: Option<Pid>) {
    if let Some(process_group) = process_group {
        let _ = killpg(process_group, Signal::SIGKILL);
    }
}

fn terminate_command(child: &mut Child, process_group: Option<Pid>) {
    kill_remaining_process_group(process_group);
    let _ = child.kill();
    let _ = child.wait();
}

fn capture_output<T: Read>(stream: Option<T>) -> (String, bool) {
    let Some(mut stream) = stream else {
        return (String::new(), false);
    };
    let mut retained = Vec::with_capacity(MAX_COMMAND_OUTPUT_BYTES.min(4096));
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let available = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(retained.len());
                let keep = read.min(available);
                retained.extend_from_slice(&buffer[..keep]);
                truncated |= keep < read;
            }
        }
    }
    (String::from_utf8_lossy(&retained).into_owned(), truncated)
}

fn receive_capture(receiver: Receiver<(String, bool)>, label: &str) -> (String, bool) {
    match receiver.recv_timeout(CAPTURE_SHUTDOWN_TIMEOUT) {
        Ok(captured) => captured,
        Err(mpsc::RecvTimeoutError::Timeout) => (
            format!("{label} did not close after process termination"),
            true,
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => (format!("{label} failed"), true),
    }
}

fn is_executable_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

fn validate_executable(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        invalid(format!(
            "Hardknock executable {} is unavailable: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(invalid(format!(
            "Hardknock executable {} must be a regular, non-symlink executable",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(format!(
            "Hardknock executable {} must not be group- or world-writable",
            path.display()
        )));
    }
    Ok(())
}

fn validate_absolute_path(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid(format!("{label} must be an absolute path")));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid(format!("{label} must not contain '..' components")));
    }
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(invalid(format!("{label} must not contain NUL bytes")));
    }
    Ok(())
}

fn validate_managed_path(
    root: &Path,
    target: &Path,
    expected_uid: u32,
    parent_must_exist: bool,
) -> Result<()> {
    validate_absolute_path(root, "service security root")?;
    validate_absolute_path(target, "service target")?;
    let parent = target
        .parent()
        .ok_or_else(|| invalid("Service target has no parent"))?;
    if !target.starts_with(root) || target == root {
        return Err(invalid(format!(
            "Service target {} must be below security root {}",
            target.display(),
            root.display()
        )));
    }
    validate_existing_component(root, expected_uid, true)?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| invalid("Service target escaped its security root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid("Service target contains an unsafe path component"));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => validate_existing_component(&current, expected_uid, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if parent_must_exist {
                    return Err(invalid(format!(
                        "Service parent {} disappeared during apply",
                        current.display()
                    )));
                }
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(target) {
        validate_target_metadata(target, &metadata, expected_uid)?;
    }
    Ok(())
}

fn validate_existing_component(path: &Path, expected_uid: u32, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        invalid(format!(
            "Could not inspect service path {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "Refusing symlink in service path: {}",
            path.display()
        )));
    }
    if directory && !metadata.is_dir() {
        return Err(invalid(format!(
            "Service parent {} is not a directory",
            path.display()
        )));
    }
    if metadata.uid() != expected_uid {
        return Err(invalid(format!(
            "Service path {} is owned by uid {}, expected {}",
            path.display(),
            metadata.uid(),
            expected_uid
        )));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(format!(
            "Service path {} must not be group- or world-writable",
            path.display()
        )));
    }
    Ok(())
}

fn validate_target_metadata(path: &Path, metadata: &fs::Metadata, expected_uid: u32) -> Result<()> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "Service target {} must be a regular, non-symlink file",
            path.display()
        )));
    }
    if metadata.uid() != expected_uid {
        return Err(invalid(format!(
            "Service target {} is owned by uid {}, expected {}",
            path.display(),
            metadata.uid(),
            expected_uid
        )));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid(format!(
            "Service target {} must not be group- or world-writable",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_secure_directory_tree(root: &Path, parent: &Path) -> Result<()> {
    let expected_uid = geteuid().as_raw();
    validate_existing_component(root, expected_uid, true)?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| invalid("Service parent escaped its security root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid("Service parent contains an unsafe path component"));
        }
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        validate_existing_component(&current, expected_uid, true)?;
    }
    Ok(())
}

fn classify_target(path: &Path, expected: &str, expected_uid: u32) -> Result<ServiceChange> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ServiceChange::Create);
        }
        Err(error) => return Err(error.into()),
    };
    validate_target_metadata(path, &metadata, expected_uid)?;
    let existing = read_bounded(path, expected.len().max(4096).saturating_add(1))?;
    if existing == expected.as_bytes() {
        return Ok(ServiceChange::Unchanged);
    }
    let existing = String::from_utf8(existing)
        .map_err(|_| invalid(format!("Service target {} is not UTF-8", path.display())))?;
    if is_managed_content(&existing) {
        Ok(ServiceChange::ReplaceManaged)
    } else {
        Err(invalid(format!(
            "Refusing to overwrite unmanaged service target {}",
            path.display()
        )))
    }
}

fn is_managed_content(content: &str) -> bool {
    content
        .lines()
        .take(8)
        .any(|line| line.contains(MANAGED_MARKER))
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit.min(1024 * 1024) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() >= limit.min(1024 * 1024) {
        return Err(invalid(format!(
            "Service target {} exceeds the expected bounded size",
            path.display()
        )));
    }
    Ok(bytes)
}

fn atomic_write_managed(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Service target has no parent"))?;
    let before = fs::symlink_metadata(parent)?;
    validate_existing_component(parent, geteuid().as_raw(), true)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(content.as_bytes())?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let after = fs::symlink_metadata(parent)?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.uid() != after.uid()
        || after.permissions().mode() & 0o022 != 0
    {
        return Err(invalid(format!(
            "Service parent {} changed while writing",
            parent.display()
        )));
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        validate_target_metadata(path, &metadata, geteuid().as_raw())?;
        let existing = read_bounded(path, content.len().max(4096).saturating_add(1))?;
        if existing != content.as_bytes()
            && !is_managed_content(
                &String::from_utf8(existing).map_err(|_| {
                    invalid(format!("Service target {} is not UTF-8", path.display()))
                })?,
            )
        {
            return Err(invalid(format!(
                "Service target {} became unmanaged while writing",
                path.display()
            )));
        }
    }
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    sync_directory(parent)
}

fn validate_exact_target(path: &Path, expected: &str, expected_uid: u32) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    validate_target_metadata(path, &metadata, expected_uid)?;
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(invalid(format!(
            "Managed service target {} must have mode 0600",
            path.display()
        )));
    }
    if read_bounded(path, expected.len().saturating_add(1))? != expected.as_bytes() {
        return Err(invalid(format!(
            "Managed service target {} changed unexpectedly",
            path.display()
        )));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn render_on_demand_command(executable: &Path, home: &Path) -> Result<String> {
    Ok(format!(
        "{} --home {} bridge start --foreground",
        shell_quote(path_text(executable, "Hardknock executable")?),
        shell_quote(path_text(home, "Hardknock home")?)
    ))
}

fn render_systemd(executable: &Path, home: &Path) -> Result<String> {
    let executable = systemd_quote(path_text(executable, "Hardknock executable")?);
    let home = systemd_quote(path_text(home, "Hardknock home")?);
    Ok(format!(
        "# {MANAGED_MARKER}\n\
[Unit]\n\
Description=Hardknock local agent lifecycle Bridge\n\
StartLimitIntervalSec=60s\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={executable} --home {home} bridge start --foreground\n\
Restart=on-failure\n\
RestartSec=5s\n\
UMask=0077\n\
StandardOutput=journal\n\
StandardError=journal\n\
SyslogIdentifier=hardknock-bridge\n\
KillSignal=SIGTERM\n\
KillMode=control-group\n\
TimeoutStopSec=30s\n\
NoNewPrivileges=true\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    ))
}

fn render_launchd(executable: &Path, home: &Path) -> Result<String> {
    let executable = xml_escape(path_text(executable, "Hardknock executable")?);
    let home = xml_escape(path_text(home, "Hardknock home")?);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<!-- {MANAGED_MARKER} -->\n\
<plist version=\"1.0\">\n\
<dict>\n\
    <key>Label</key>\n\
    <string>{LAUNCHD_LABEL}</string>\n\
    <key>ProgramArguments</key>\n\
    <array>\n\
        <string>{executable}</string>\n\
        <string>--home</string>\n\
        <string>{home}</string>\n\
        <string>bridge</string>\n\
        <string>start</string>\n\
        <string>--foreground</string>\n\
    </array>\n\
    <key>RunAtLoad</key>\n\
    <true/>\n\
    <key>KeepAlive</key>\n\
    <dict><key>SuccessfulExit</key><false/></dict>\n\
    <key>ThrottleInterval</key>\n\
    <integer>5</integer>\n\
    <key>Umask</key>\n\
    <integer>63</integer>\n\
    <key>ProcessType</key>\n\
    <string>Background</string>\n\
    <key>AbandonProcessGroup</key>\n\
    <false/>\n\
    <key>ExitTimeOut</key>\n\
    <integer>30</integer>\n\
    <key>StandardOutPath</key>\n\
    <string>/dev/null</string>\n\
    <key>StandardErrorPath</key>\n\
    <string>/dev/null</string>\n\
</dict>\n\
</plist>\n"
    ))
}

fn path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| invalid(format!("{label} must be valid UTF-8")))
}

fn systemd_quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            '\n' => escaped.push_str("\\x0a"),
            '\r' => escaped.push_str("\\x0d"),
            '\t' => escaped.push_str("\\x09"),
            character if character.is_control() => {
                for byte in character.to_string().as_bytes() {
                    escaped.push_str(&format!("\\x{byte:02x}"));
                }
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            character => escaped.push(character),
        }
    }
    escaped
}

fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        _temp: tempfile::TempDir,
        user_home: PathBuf,
        hardknock_home: PathBuf,
        executable: PathBuf,
        manager: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let user_home = temp.path().join("home");
            let hardknock_home = user_home.join(".hardknock");
            let bin = user_home.join("bin");
            fs::create_dir_all(&hardknock_home).unwrap();
            fs::create_dir_all(&bin).unwrap();
            fs::set_permissions(&user_home, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&hardknock_home, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            let executable = bin.join("hardknock");
            let manager = bin.join("manager");
            fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
            fs::write(&manager, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&manager, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _temp: temp,
                user_home,
                hardknock_home,
                executable,
                manager,
            }
        }

        fn options(&self, platform: ServicePlatform) -> ServiceOptions {
            ServiceOptions {
                platform,
                requested: RequestedService::Auto,
                user_home: self.user_home.clone(),
                hardknock_home: self.hardknock_home.clone(),
                executable: self.executable.clone(),
                config_home: None,
                manager_executable: Some(self.manager.clone()),
            }
        }
    }

    #[test]
    fn systemd_plan_uses_standard_path_and_safe_escaping() {
        let fixture = Fixture::new();
        let escaped_home = fixture.user_home.join("hard knock%home");
        fs::create_dir(&escaped_home).unwrap();
        fs::set_permissions(&escaped_home, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = escaped_home.join("hard\"knock");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = fixture.options(ServicePlatform::Linux);
        options.hardknock_home = escaped_home;
        options.executable = executable;
        let plan = plan(&options).unwrap();
        assert_eq!(
            plan.target.as_deref(),
            Some(
                fixture
                    .user_home
                    .join(".config/systemd/user/hardknock-bridge.service")
                    .as_path()
            )
        );
        let content = plan.content.unwrap();
        assert!(content.contains("hard\\\"knock"));
        assert!(content.contains("hard knock%%home"));
        assert!(content.contains(MANAGED_MARKER));
    }

    #[test]
    fn launchd_plan_escapes_xml_and_uses_launch_agents() {
        let fixture = Fixture::new();
        let special_home = fixture.user_home.join("hard&<knock>");
        fs::create_dir(&special_home).unwrap();
        fs::set_permissions(&special_home, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = fixture.options(ServicePlatform::Macos);
        options.hardknock_home = special_home;
        let plan = plan(&options).unwrap();
        assert_eq!(
            plan.target.as_deref(),
            Some(
                fixture
                    .user_home
                    .join("Library/LaunchAgents/dev.openkedge.hardknock.bridge.plist")
                    .as_path()
            )
        );
        assert!(plan.content.unwrap().contains("hard&amp;&lt;knock&gt;"));
    }

    #[test]
    fn missing_manager_produces_actionable_on_demand_plan() {
        let fixture = Fixture::new();
        let manager = detect_manager_in_path(ServicePlatform::Linux, Some(OsStr::new("")));
        assert!(matches!(manager, ServiceManager::OnDemand { .. }));
        let mut options = fixture.options(ServicePlatform::Other);
        options.manager_executable = None;
        let plan = plan(&options).unwrap();
        assert_eq!(plan.change, ServiceChange::OnDemand);
        assert!(plan.target.is_none());
        assert!(plan.fallback.contains("bridge start --foreground"));
    }

    #[tokio::test]
    async fn dry_run_does_not_create_service_directories() {
        let fixture = Fixture::new();
        let plan = plan(&fixture.options(ServicePlatform::Linux)).unwrap();
        let report = plan.apply_with_options(false, true).await.unwrap();
        assert!(report.changed);
        assert!(report.dry_run);
        assert!(!fixture.user_home.join(".config").exists());
    }

    #[tokio::test]
    async fn apply_is_atomic_private_and_idempotent() {
        let fixture = Fixture::new();
        let options = fixture.options(ServicePlatform::Linux);
        let first_plan = plan(&options).unwrap();
        let first = first_plan.apply(false).await.unwrap();
        assert!(first.changed);
        let target = first_plan.target.as_ref().unwrap();
        assert_eq!(
            fs::symlink_metadata(target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let second_plan = plan(&options).unwrap();
        assert_eq!(second_plan.change, ServiceChange::Unchanged);
        let second = second_plan.apply(false).await.unwrap();
        assert!(!second.changed);
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            second_plan.content.unwrap()
        );
    }

    #[test]
    fn unmanaged_conflict_is_refused() {
        let fixture = Fixture::new();
        let target = fixture
            .user_home
            .join(".config/systemd/user/hardknock-bridge.service");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::set_permissions(
            fixture.user_home.join(".config"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::set_permissions(
            fixture.user_home.join(".config/systemd"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::set_permissions(
            fixture.user_home.join(".config/systemd/user"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(&target, "[Service]\nExecStart=/something-else\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let error = plan(&fixture.options(ServicePlatform::Linux)).unwrap_err();
        assert!(error.to_string().contains("unmanaged"));
    }

    #[test]
    fn symlink_and_writable_parent_are_refused() {
        let fixture = Fixture::new();
        let config = fixture.user_home.join(".config");
        let elsewhere = fixture.user_home.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, &config).unwrap();
        let error = plan(&fixture.options(ServicePlatform::Linux)).unwrap_err();
        assert!(error.to_string().contains("symlink"));

        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o777)).unwrap();
        let error = plan(&fixture.options(ServicePlatform::Linux)).unwrap_err();
        assert!(error.to_string().contains("world-writable"));
    }

    #[tokio::test]
    async fn remove_requires_exact_managed_content() {
        let fixture = Fixture::new();
        let options = fixture.options(ServicePlatform::Linux);
        let plan = plan(&options).unwrap();
        plan.apply(false).await.unwrap();
        let target = plan.target.as_ref().unwrap();
        let mut changed = fs::read_to_string(target).unwrap();
        changed.push_str("# user change\n");
        fs::write(target, changed).unwrap();
        fs::set_permissions(target, fs::Permissions::from_mode(0o600)).unwrap();
        let error = plan.uninstall(false).await.unwrap_err();
        assert!(error.to_string().contains("exactly match"));
        assert!(target.exists());

        fs::write(target, plan.content.as_ref().unwrap()).unwrap();
        fs::set_permissions(target, fs::Permissions::from_mode(0o600)).unwrap();
        let report = plan.uninstall(false).await.unwrap();
        assert!(report.removed);
        assert!(!target.exists());
    }

    #[test]
    fn fake_manager_output_and_timeout_are_bounded() {
        let fixture = Fixture::new();
        let noisy = fixture.user_home.join("bin/noisy-manager");
        fs::write(
            &noisy,
            "#!/bin/sh\nhead -c 70000 /dev/zero | tr '\\000' x\nsleep 2\n",
        )
        .unwrap();
        fs::set_permissions(&noisy, fs::Permissions::from_mode(0o700)).unwrap();
        let report = run_command(&ManagerCommand {
            program: noisy,
            args: Vec::new(),
            timeout_ms: 500,
        });
        assert_eq!(report.status, CommandStatus::TimedOut);
        assert_eq!(report.stdout.len(), MAX_COMMAND_OUTPUT_BYTES);
        assert!(report.stdout_truncated);
    }

    #[test]
    fn manager_descendants_cannot_hold_capture_pipes_open() {
        let fixture = Fixture::new();
        let background = fixture.user_home.join("bin/background-manager");
        fs::write(
            &background,
            "#!/bin/sh\nsleep 20 &\nprintf 'manager exited\\n'\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&background, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let report = run_command(&ManagerCommand {
            program: background,
            args: Vec::new(),
            timeout_ms: 1_000,
        });
        assert_eq!(report.status, CommandStatus::Succeeded);
        assert!(report.stdout.contains("manager exited"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn plans_and_reports_are_serializable_without_file_content() {
        let fixture = Fixture::new();
        let plan = plan(&fixture.options(ServicePlatform::Linux)).unwrap();
        let serialized = serde_json::to_value(&plan).unwrap();
        assert!(serialized.get("content").is_none());
        serde_json::to_value(plan.apply_with_options(false, true).await.unwrap()).unwrap();
    }
}
