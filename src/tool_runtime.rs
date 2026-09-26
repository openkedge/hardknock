// SPDX-License-Identifier: Apache-2.0

//! Per-invocation micro-sandbox providers and the Tool Router.  The host
//! provider is deliberately opt-in and reports `Observed` assurance; the
//! container provider refuses to fall back to host execution when Docker or
//! Podman is unavailable.

use crate::{
    Error, Result,
    capability::{CapabilityManifest, IsolationLevel, NetworkMode, container_bind_mount},
    core::{MicroSandboxId, Reality},
    store::{Store, ToolStore},
    tool::*,
};
use async_trait::async_trait;
use chrono::Utc;
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinSet,
    time::{Duration, Instant, timeout},
};

const DEFAULT_TOOL_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const CAPTURE_BUFFER_BYTES: usize = 64 * 1024;
const CAPTURE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_GROUP_SWEEP_WINDOW: Duration = Duration::from_millis(100);
const PROCESS_GROUP_SWEEP_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturedStream {
    Stdout,
    Stderr,
}

impl CapturedStream {
    const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug)]
enum CaptureCompletion {
    Complete,
    Limit(CapturedStream),
    Failed(CapturedStream, std::io::Error),
}

#[derive(Default)]
struct CaptureState {
    bytes: Vec<u8>,
    limit_exceeded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedCommandStop {
    OutputLimit(CapturedStream),
    TimedOut,
}

struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stop: Option<BoundedCommandStop>,
}

struct ProcessGroup(Option<Pid>);

impl ProcessGroup {
    fn signal(&self, leader_reaped: bool) -> Result<bool> {
        let Some(pid) = self.0 else {
            return Ok(false);
        };
        match killpg(pid, Signal::SIGKILL) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(Errno::EPERM) if leader_reaped => Ok(false),
            Err(error) => Err(Error::Io(std::io::Error::from_raw_os_error(error as i32))),
        }
    }

    async fn terminate_remaining(&self) -> Result<()> {
        let deadline = Instant::now() + PROCESS_GROUP_SWEEP_WINDOW;
        loop {
            if !self.signal(true)? || Instant::now() >= deadline {
                return Ok(());
            }
            tokio::time::sleep(PROCESS_GROUP_SWEEP_INTERVAL).await;
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Err(error) = self.signal(false) {
            tracing::error!(%error, "Could not stop tool process group");
        }
    }
}

async fn capture_stream<R>(
    mut input: R,
    stream: CapturedStream,
    maximum: usize,
    state: Arc<Mutex<CaptureState>>,
) -> CaptureCompletion
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    loop {
        let read = match input.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => return CaptureCompletion::Failed(stream, error),
        };
        if read == 0 {
            return CaptureCompletion::Complete;
        }
        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                return CaptureCompletion::Failed(
                    stream,
                    std::io::Error::other("tool output capture lock poisoned"),
                );
            }
        };
        let available = maximum.saturating_sub(state.bytes.len());
        let retained = read.min(available);
        state.bytes.extend_from_slice(&buffer[..retained]);
        if retained < read {
            state.limit_exceeded = true;
            return CaptureCompletion::Limit(stream);
        }
    }
}

fn take_capture(state: &Arc<Mutex<CaptureState>>) -> Result<(Vec<u8>, bool)> {
    let mut state = state
        .lock()
        .map_err(|_| Error::Intervention("Tool output capture lock poisoned".into()))?;
    Ok((std::mem::take(&mut state.bytes), state.limit_exceeded))
}

async fn terminate_process_group(
    child: &mut Child,
    exit: &mut Option<ExitStatus>,
    group: &mut ProcessGroup,
) -> Result<()> {
    let leader_reaped = exit.is_some();
    let signal_result = group.signal(leader_reaped);
    if exit.is_none() {
        let _ = child.start_kill();
        *exit = Some(child.wait().await?);
    }
    let sweep_result = group.terminate_remaining().await;
    if signal_result.is_ok() && sweep_result.is_ok() {
        group.0 = None;
    }
    signal_result?;
    sweep_result
}

async fn settle_capture_tasks(captures: &mut JoinSet<CaptureCompletion>) {
    if timeout(CAPTURE_SHUTDOWN_TIMEOUT, async {
        while captures.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        captures.shutdown().await;
    }
}

async fn bounded_command_output(
    mut child: Child,
    execution_timeout: Duration,
    maximum: usize,
) -> Result<BoundedCommandOutput> {
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| Error::InvalidInput("Spawned tool process has no valid PID".into()))?;
    let mut group = ProcessGroup(Some(Pid::from_raw(pid)));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::InvalidInput("Tool process has no stdout pipe".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::InvalidInput("Tool process has no stderr pipe".into()))?;
    let stdout_state = Arc::new(Mutex::new(CaptureState {
        bytes: Vec::with_capacity(maximum.min(CAPTURE_BUFFER_BYTES)),
        limit_exceeded: false,
    }));
    let stderr_state = Arc::new(Mutex::new(CaptureState {
        bytes: Vec::with_capacity(maximum.min(CAPTURE_BUFFER_BYTES)),
        limit_exceeded: false,
    }));
    let mut captures = JoinSet::new();
    captures.spawn(capture_stream(
        stdout,
        CapturedStream::Stdout,
        maximum,
        Arc::clone(&stdout_state),
    ));
    captures.spawn(capture_stream(
        stderr,
        CapturedStream::Stderr,
        maximum,
        Arc::clone(&stderr_state),
    ));

    let deadline = Instant::now() + execution_timeout;
    let mut exit = None;
    let mut stop = None;
    let mut primary_error = None;
    loop {
        if exit.is_some() && captures.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            completion = captures.join_next(), if !captures.is_empty() => {
                match completion {
                    Some(Ok(CaptureCompletion::Complete)) => {}
                    Some(Ok(CaptureCompletion::Limit(stream))) => {
                        stop = Some(BoundedCommandStop::OutputLimit(stream));
                        break;
                    }
                    Some(Ok(CaptureCompletion::Failed(stream, error))) => {
                        primary_error = Some(Error::Io(std::io::Error::new(
                            error.kind(),
                            format!("tool {} capture failed: {error}", stream.name()),
                        )));
                        break;
                    }
                    Some(Err(error)) => {
                        primary_error = Some(Error::Intervention(format!(
                            "Tool output capture task failed: {error}"
                        )));
                        break;
                    }
                    None => {}
                }
            }
            result = child.wait(), if exit.is_none() => {
                match result {
                    Ok(status) => {
                        exit = Some(status);
                        break;
                    }
                    Err(error) => {
                        primary_error = Some(Error::Io(error));
                        break;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                stop = Some(BoundedCommandStop::TimedOut);
                break;
            }
        }
    }

    let mut cleanup_error = None;
    if let Err(error) = terminate_process_group(&mut child, &mut exit, &mut group).await {
        cleanup_error = Some(error);
    }
    settle_capture_tasks(&mut captures).await;

    let (stdout, stdout_exceeded) = take_capture(&stdout_state)?;
    let (stderr, stderr_exceeded) = take_capture(&stderr_state)?;
    if stdout_exceeded {
        stop = Some(BoundedCommandStop::OutputLimit(CapturedStream::Stdout));
    } else if stderr_exceeded {
        stop = Some(BoundedCommandStop::OutputLimit(CapturedStream::Stderr));
    }

    if let Some(primary) = primary_error {
        return Err(if let Some(cleanup) = cleanup_error {
            Error::Cleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }
        } else {
            primary
        });
    }
    if let Some(error) = cleanup_error {
        return Err(error);
    }
    Ok(BoundedCommandOutput {
        status: exit.ok_or_else(|| Error::Intervention("Tool process was not reaped".into()))?,
        stdout,
        stderr,
        stop,
    })
}

fn tool_output_limit(sandbox: &MicroSandbox) -> Result<usize> {
    usize::try_from(
        sandbox
            .capabilities
            .resources
            .output_bytes
            .unwrap_or(DEFAULT_TOOL_OUTPUT_BYTES),
    )
    .map_err(|_| Error::InvalidInput("Tool output limit exceeds this platform".into()))
}

fn tool_execution_result(
    output: BoundedCommandOutput,
    started_at: chrono::DateTime<Utc>,
    limit_ms: u64,
) -> ToolExecutionResult {
    if output.stop == Some(BoundedCommandStop::TimedOut) {
        return ToolExecutionResult {
            status: ToolExecutionStatus::TimedOut,
            started_at: Some(started_at),
            completed_at: Some(Utc::now()),
            error: Some(format!("execution exceeded {limit_ms}ms")),
            ..Default::default()
        };
    }
    ToolExecutionResult {
        status: if output.status.success() {
            ToolExecutionStatus::Success
        } else {
            ToolExecutionStatus::Failed
        },
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        truncated: matches!(output.stop, Some(BoundedCommandStop::OutputLimit(_))),
        started_at: Some(started_at),
        completed_at: Some(Utc::now()),
        ..Default::default()
    }
}

#[async_trait]
pub trait MicroSandboxProvider: Send + Sync {
    async fn create(
        &self,
        reality: &Reality,
        tool: &ToolDefinition,
        capabilities: &EffectiveToolCapabilities,
    ) -> Result<MicroSandbox>;
    async fn execute(
        &self,
        sandbox: &MicroSandbox,
        invocation: &ResolvedToolInvocation,
    ) -> Result<ToolExecutionResult>;
    async fn destroy(&self, sandbox: &MicroSandbox) -> Result<()>;
    fn guarantees(&self) -> SandboxGuarantees;
}

#[derive(Clone, Debug)]
pub struct HostMicroSandboxProvider {
    pub allow_host_fallback: bool,
    workspaces: Arc<Mutex<BTreeMap<MicroSandboxId, PathBuf>>>,
}

impl HostMicroSandboxProvider {
    pub fn trusted_development() -> Self {
        Self {
            allow_host_fallback: true,
            workspaces: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn new(allow_host_fallback: bool) -> Self {
        Self {
            allow_host_fallback,
            workspaces: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[async_trait]
impl MicroSandboxProvider for HostMicroSandboxProvider {
    async fn create(
        &self,
        reality: &Reality,
        tool: &ToolDefinition,
        capabilities: &EffectiveToolCapabilities,
    ) -> Result<MicroSandbox> {
        if !self.allow_host_fallback {
            return Err(Error::Intervention("Host tool execution is disabled; configure explicit trusted development mode to allow it".into()));
        }
        if matches!(tool.invocation, ToolInvocation::WasiComponent { .. }) {
            return Err(Error::Intervention("WASI tool requested but no WASI runtime is configured; refusing a silent host downgrade".into()));
        }
        let runtime = if matches!(tool.invocation, ToolInvocation::EffectAdapter { .. }) {
            MicroSandboxRuntime::EffectBoundary
        } else {
            MicroSandboxRuntime::Host
        };
        let enforced = if runtime == MicroSandboxRuntime::EffectBoundary {
            effect_boundary_capabilities(capabilities)
        } else {
            capabilities.clone()
        };
        let sandbox = new_micro_sandbox(reality.id.clone(), tool, enforced, runtime);
        self.workspaces
            .lock()
            .map_err(|_| Error::Intervention("Host tool workspace lock poisoned".into()))?
            .insert(sandbox.id.clone(), reality.root.canonicalize()?);
        Ok(sandbox)
    }

    async fn execute(
        &self,
        sandbox: &MicroSandbox,
        invocation: &ResolvedToolInvocation,
    ) -> Result<ToolExecutionResult> {
        if sandbox.destroyed_at.is_some() || sandbox.expires_at <= Utc::now() {
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::Denied,
                error: Some("micro-sandbox expired or destroyed".into()),
                ..Default::default()
            });
        }
        if let Some((adapter, operation)) = &invocation.effect_adapter {
            let now = Utc::now();
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::Success,
                stdout: serde_json::json!({"effect_request":{"adapter":adapter,"operation":operation,"input":invocation.input}}).to_string(),
                started_at: Some(now),
                completed_at: Some(now),
                ..Default::default()
            });
        }
        let Some(executable) = invocation.executable.as_deref() else {
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::RuntimeFailure,
                error: Some("Invocation has no host executable".into()),
                ..Default::default()
            });
        };
        let workspace = self
            .workspaces
            .lock()
            .map_err(|_| Error::Intervention("Host tool workspace lock poisoned".into()))?
            .get(&sandbox.id)
            .cloned()
            .ok_or_else(|| {
                Error::NotFound(format!("Micro-sandbox {} is not active", sandbox.id))
            })?;
        let args = invocation
            .args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                let virtual_path = match invocation.tool.name.as_str() {
                    "read-file" => true,
                    "write-file" => index == 3,
                    "run-tests" | "shell-generic" => false,
                    _ => argument == "/workspace" || argument.starts_with("/workspace/"),
                };
                if virtual_path {
                    argument
                        .strip_prefix("/workspace")
                        .map(|suffix| format!("{}{suffix}", workspace.display()))
                        .unwrap_or_else(|| argument.clone())
                } else {
                    argument.clone()
                }
            })
            .collect::<Vec<_>>();
        let started_at = Utc::now();
        let maximum = tool_output_limit(sandbox)?;
        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .env_clear()
            .kill_on_drop(true);
        command.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        for (name, value) in &sandbox.capabilities.environment.values {
            command.env(name, value);
        }
        let child = command.spawn().map_err(|source| Error::ProcessStart {
            program: executable.into(),
            source,
        })?;
        let limit = sandbox
            .capabilities
            .duration
            .max_ms
            .or(sandbox.capabilities.resources.timeout_ms)
            .unwrap_or(300_000);
        let output = bounded_command_output(child, Duration::from_millis(limit), maximum).await?;
        Ok(tool_execution_result(output, started_at, limit))
    }

    async fn destroy(&self, sandbox: &MicroSandbox) -> Result<()> {
        self.workspaces
            .lock()
            .map_err(|_| Error::Intervention("Host tool workspace lock poisoned".into()))?
            .remove(&sandbox.id);
        Ok(())
    }
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::observed()
    }
}

#[derive(Clone, Debug)]
pub struct ContainerMicroSandboxProvider {
    pub runtime: String,
    pub image: String,
    containers: Arc<Mutex<BTreeMap<MicroSandboxId, String>>>,
}

impl ContainerMicroSandboxProvider {
    pub fn new(runtime: impl Into<String>, image: impl Into<String>) -> Result<Self> {
        let runtime = runtime.into();
        let image = image.into();
        if runtime.trim().is_empty()
            || image.trim().is_empty()
            || runtime.contains(['\0', '\n', '\r'])
            || image.contains(['\0', '\n', '\r'])
        {
            return Err(Error::InvalidInput(
                "Micro-sandbox runtime and image must be bounded".into(),
            ));
        }
        Ok(Self {
            runtime,
            image,
            containers: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn create_arguments(
        &self,
        reality: &Reality,
        tool: &ToolDefinition,
        capabilities: &EffectiveToolCapabilities,
    ) -> Result<Vec<String>> {
        let root = reality.root.canonicalize()?;
        let name = format!("hk-ms-{}", short_id(&MicroSandboxId::new().to_string()));
        let mut args = vec![
            "create".into(),
            "--name".into(),
            name,
            "--rm".into(),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--network".into(),
            network_mode(&capabilities.network).into(),
            "--workdir".into(),
            "/workspace".into(),
        ];
        let write_roots = capabilities
            .filesystem
            .write
            .iter()
            .map(|path| {
                path.trim_end_matches("/**")
                    .trim_end_matches("/*")
                    .to_owned()
            })
            .collect::<BTreeSet<_>>();
        let full_workspace_write = write_roots.contains("/workspace");
        args.extend([
            "--mount".into(),
            container_bind_mount(&root, "/workspace", !full_workspace_write),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,nodev,noexec,size=256m".into(),
            "--env".into(),
            "HOME=/tmp/hardknock".into(),
        ]);
        if !full_workspace_write {
            for target in write_roots
                .iter()
                .filter(|target| target.starts_with("/workspace/"))
            {
                let relative = target.trim_start_matches("/workspace/");
                let source = root.join(relative);
                if source.exists() {
                    args.extend([
                        "--mount".into(),
                        container_bind_mount(&source, target, false),
                    ]);
                } else {
                    args.extend([
                        "--tmpfs".into(),
                        format!("{target}:rw,nosuid,nodev,size=256m"),
                    ]);
                }
            }
        }
        for (name, value) in &capabilities.environment.values {
            args.extend(["--env".into(), format!("{name}={value}")]);
        }
        if let Some(cpu) = &capabilities.resources.cpu {
            args.extend(["--cpus".into(), cpu.clone()]);
        }
        if let Some(memory) = capabilities.resources.memory_mb {
            args.extend(["--memory".into(), format!("{memory}m")]);
        }
        if let Some(pids) = capabilities.resources.pids {
            args.extend(["--pids-limit".into(), pids.to_string()]);
        }
        let executable = match &tool.invocation {
            ToolInvocation::NativeBinary { .. } => "/bin/sh",
            ToolInvocation::Script { .. } => "/bin/sh",
            _ => "/bin/sh",
        };
        args.extend([
            self.image.clone(),
            executable.into(),
            "-c".into(),
            "while :; do sleep 3600; done".into(),
        ]);
        Ok(args)
    }

    async fn command(&self, args: &[String]) -> Result<BoundedCommandOutput> {
        let mut command = Command::new(&self.runtime);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn()?;
        let output = bounded_command_output(
            child,
            Duration::from_millis(300_000),
            DEFAULT_TOOL_OUTPUT_BYTES as usize,
        )
        .await?;
        match output.stop {
            Some(BoundedCommandStop::TimedOut) => Err(Error::Intervention(
                "Micro-sandbox container runtime command exceeded 300000ms".into(),
            )),
            Some(BoundedCommandStop::OutputLimit(stream)) => Err(Error::Intervention(format!(
                "Micro-sandbox container runtime {} exceeded the {} byte capture limit",
                stream.name(),
                DEFAULT_TOOL_OUTPUT_BYTES
            ))),
            None => Ok(output),
        }
    }
}

#[async_trait]
impl MicroSandboxProvider for ContainerMicroSandboxProvider {
    async fn create(
        &self,
        reality: &Reality,
        tool: &ToolDefinition,
        capabilities: &EffectiveToolCapabilities,
    ) -> Result<MicroSandbox> {
        if matches!(tool.invocation, ToolInvocation::EffectAdapter { .. }) {
            return Ok(new_micro_sandbox(
                reality.id.clone(),
                tool,
                effect_boundary_capabilities(capabilities),
                MicroSandboxRuntime::EffectBoundary,
            ));
        }
        let args = self.create_arguments(reality, tool, capabilities)?;
        let output = self.command(&args).await?;
        if !output.status.success() {
            return Err(Error::Intervention(format!(
                "Micro-sandbox container create failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if id.is_empty() {
            return Err(Error::Intervention(
                "Container runtime returned no micro-sandbox id".into(),
            ));
        }
        let mut enforced = capabilities.clone();
        if enforced.network.mode == NetworkMode::AllowList {
            // Arbitrary endpoint allow-listing is not enforceable with a plain
            // Docker bridge. Deny it until a runtime-specific network policy
            // provider is configured, and record the narrower actual grant.
            enforced.network.mode = NetworkMode::None;
            enforced.network.allow.clear();
        }
        let sandbox = new_micro_sandbox(
            reality.id.clone(),
            tool,
            enforced,
            MicroSandboxRuntime::Container,
        );
        self.containers
            .lock()
            .map_err(|_| Error::Intervention("Micro-sandbox registry lock poisoned".into()))?
            .insert(sandbox.id.clone(), id);
        let container_id = self
            .containers
            .lock()
            .map_err(|_| Error::Intervention("Micro-sandbox registry lock poisoned".into()))?
            .get(&sandbox.id)
            .cloned()
            .ok_or_else(|| {
                Error::Intervention("Micro-sandbox container was not registered".into())
            })?;
        let start = self.command(&["start".into(), container_id]).await?;
        if !start.status.success() {
            let _ = self.destroy(&sandbox).await;
            return Err(Error::Intervention(format!(
                "Micro-sandbox container start failed: {}",
                String::from_utf8_lossy(&start.stderr).trim()
            )));
        }
        Ok(sandbox)
    }

    async fn execute(
        &self,
        sandbox: &MicroSandbox,
        invocation: &ResolvedToolInvocation,
    ) -> Result<ToolExecutionResult> {
        if sandbox.destroyed_at.is_some() || sandbox.expires_at <= Utc::now() {
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::Denied,
                error: Some("micro-sandbox expired or destroyed".into()),
                ..Default::default()
            });
        }
        if let Some((adapter, operation)) = &invocation.effect_adapter {
            let now = Utc::now();
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::Success,
                stdout: serde_json::json!({"effect_request":{"adapter":adapter,"operation":operation,"input":invocation.input}}).to_string(),
                started_at: Some(now),
                completed_at: Some(now),
                ..Default::default()
            });
        }
        let id = self
            .containers
            .lock()
            .map_err(|_| Error::Intervention("Micro-sandbox registry lock poisoned".into()))?
            .get(&sandbox.id)
            .cloned()
            .ok_or_else(|| {
                Error::NotFound(format!("Micro-sandbox {} is not active", sandbox.id))
            })?;
        let Some(executable) = invocation.executable.as_deref() else {
            return Ok(ToolExecutionResult {
                status: ToolExecutionStatus::RuntimeFailure,
                error: Some("Container provider supports native/script tools only".into()),
                ..Default::default()
            });
        };
        let mut args = vec!["exec".into(), id, executable.into()];
        args.extend(invocation.args.clone());
        let started_at = Utc::now();
        let limit = sandbox
            .capabilities
            .duration
            .max_ms
            .or(sandbox.capabilities.resources.timeout_ms)
            .unwrap_or(300_000);
        let maximum = tool_output_limit(sandbox)?;
        let mut command = tokio::process::Command::new(&self.runtime);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn()?;
        let output = bounded_command_output(child, Duration::from_millis(limit), maximum).await?;
        Ok(tool_execution_result(output, started_at, limit))
    }

    async fn destroy(&self, sandbox: &MicroSandbox) -> Result<()> {
        let id = self
            .containers
            .lock()
            .map_err(|_| Error::Intervention("Micro-sandbox registry lock poisoned".into()))?
            .remove(&sandbox.id);
        if let Some(id) = id {
            let output = self.command(&["rm".into(), "--force".into(), id]).await?;
            if !output.status.success() {
                return Err(Error::Intervention(format!(
                    "Micro-sandbox cleanup failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        Ok(())
    }
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::container()
    }
}

#[derive(Clone, Debug, Default)]
pub struct WasiMicroSandboxProvider;

#[async_trait]
impl MicroSandboxProvider for WasiMicroSandboxProvider {
    async fn create(
        &self,
        _reality: &Reality,
        _tool: &ToolDefinition,
        _capabilities: &EffectiveToolCapabilities,
    ) -> Result<MicroSandbox> {
        Err(Error::Intervention("WASI provider is experimental and unavailable in this build; refusing a silent downgrade".into()))
    }
    async fn execute(
        &self,
        _sandbox: &MicroSandbox,
        _invocation: &ResolvedToolInvocation,
    ) -> Result<ToolExecutionResult> {
        Err(Error::Intervention("WASI provider is unavailable".into()))
    }
    async fn destroy(&self, _sandbox: &MicroSandbox) -> Result<()> {
        Ok(())
    }
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: IsolationLevel::StrongSandbox,
            process: IsolationLevel::StrongSandbox,
            network: IsolationLevel::StrongSandbox,
            credentials: IsolationLevel::StrongSandbox,
            ephemeral: true,
            known_limitations: vec!["WASI runtime is not enabled in this build".into()],
        }
    }
}

pub struct ToolRun {
    pub sandbox: MicroSandbox,
    pub result: ToolExecutionResult,
    pub attestation: ExecutionAttestation,
    pub receipt: ToolExecutionReceipt,
    pub lifecycle: Vec<ToolLifecycleEvent>,
}

pub struct ToolRouter<P> {
    pub registry: ToolRegistry,
    pub provider: P,
    pub policy: Box<dyn CapabilityIntersectionPolicy>,
    pub selector: Box<dyn SandboxSelectionPolicy>,
}

impl<P: MicroSandboxProvider> ToolRouter<P> {
    pub fn new(registry: ToolRegistry, provider: P) -> Self {
        Self {
            registry,
            provider,
            policy: Box::new(DenyByDefaultToolIntersectionPolicy),
            selector: Box::new(DefaultSandboxSelectionPolicy),
        }
    }
    pub fn with_policy(mut self, policy: Box<dyn CapabilityIntersectionPolicy>) -> Self {
        self.policy = policy;
        self
    }
    pub fn with_selector(mut self, selector: Box<dyn SandboxSelectionPolicy>) -> Self {
        self.selector = selector;
        self
    }

    pub fn resolve_capabilities(
        &self,
        reality_manifest: &CapabilityManifest,
        tool: &str,
        grants: &[TemporaryCapabilityGrant],
    ) -> Result<EffectiveToolCapabilities> {
        let definition = self.registry.get(tool)?;
        definition.validate()?;
        let mut effective =
            self.policy
                .resolve(reality_manifest, &definition.capabilities, grants)?;
        effective.tool_manifest_hash = Some(definition.manifest_hash()?);
        Ok(effective)
    }

    pub async fn execute(
        &self,
        reality: &Reality,
        reality_manifest: &CapabilityManifest,
        name_or_id: &str,
        input: Value,
        grants: &[TemporaryCapabilityGrant],
    ) -> Result<ToolRun> {
        self.execute_controlled(reality, reality_manifest, name_or_id, input, grants, None)
            .await
    }

    /// Cancellation is observed inside the lifecycle so destruction and attestation
    /// still occur before the caller releases the enclosing Reality.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_controlled(
        &self,
        reality: &Reality,
        reality_manifest: &CapabilityManifest,
        name_or_id: &str,
        input: Value,
        grants: &[TemporaryCapabilityGrant],
        control: Option<(&crate::cancellation::Cancellation, Duration)>,
    ) -> Result<ToolRun> {
        let tool = self.registry.get(name_or_id)?.clone();
        if tool.disabled || tool.trust == ToolTrust::Blocked {
            return Err(Error::Intervention(format!(
                "Tool {} is disabled",
                tool.name
            )));
        }
        tool.validate()?;
        let mut capabilities = self
            .policy
            .resolve(reality_manifest, &tool.capabilities, grants)?;
        capabilities.tool_manifest_hash = Some(tool.manifest_hash()?);
        let requirements = SandboxRequirements {
            filesystem: reality.execution_boundary.capabilities.filesystem_isolation,
            process: reality.execution_boundary.capabilities.process_isolation,
            network: reality.execution_boundary.capabilities.network_isolation,
            portability_required: matches!(tool.invocation, ToolInvocation::WasiComponent { .. }),
        };
        let selected_runtime = self.selector.select(&tool, &requirements)?;
        if selected_runtime == MicroSandboxRuntime::Wasi {
            // A provider must explicitly implement WASI.  The host and
            // container providers intentionally refuse this invocation.
            if !matches!(
                self.provider.guarantees().filesystem,
                IsolationLevel::StrongSandbox
            ) {
                return Err(Error::Intervention(
                    "Tool requires a WASI sandbox but the selected provider cannot provide one"
                        .into(),
                ));
            }
        }
        let invocation = resolve_invocation(&tool, input)?;
        let invocation_hash = blake3::hash(&serde_json::to_vec(&invocation)?)
            .to_hex()
            .to_string();
        let mut sandbox = self.provider.create(reality, &tool, &capabilities).await?;
        let requested_denial = denied_by_intersection(&tool.capabilities, &sandbox.capabilities);
        let mut result = if let Some(reason) = requested_denial {
            ToolExecutionResult {
                status: ToolExecutionStatus::Denied,
                error: Some(reason),
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                ..Default::default()
            }
        } else {
            let execution = async { self.provider.execute(&sandbox, &invocation).await };
            let result = if let Some((cancel, deadline)) = control {
                tokio::select! {
                    _ = cancel.cancelled() => Err(Error::Intervention("Tool execution cancelled".into())),
                    result = timeout(deadline, execution) => result.map_err(|_| Error::Intervention("Tool execution deadline elapsed".into())).and_then(|r| r),
                }
            } else {
                execution.await
            };
            result.unwrap_or_else(|error| ToolExecutionResult {
                status: ToolExecutionStatus::RuntimeFailure,
                error: Some(error.to_string()),
                ..Default::default()
            })
        };
        if result.status == ToolExecutionStatus::Success
            && !tool
                .outputs
                .schema
                .as_object()
                .is_some_and(|object| object.is_empty())
            && validate_tool_output(&tool.outputs.schema, &result.stdout).is_err()
        {
            result.status = ToolExecutionStatus::InvalidOutput;
            result.error = Some("tool output did not conform to the declared schema".into());
        }
        self.provider.destroy(&sandbox).await?;
        sandbox.destroyed_at = Some(Utc::now());
        let started_at = result.started_at.unwrap_or(sandbox.created_at);
        let completed_at = result.completed_at.unwrap_or_else(Utc::now);
        let output_hashes = [result.stdout.as_bytes(), result.stderr.as_bytes()]
            .iter()
            .map(|bytes| blake3::hash(bytes).to_hex().to_string())
            .collect();
        let isolation = if sandbox.runtime == MicroSandboxRuntime::EffectBoundary {
            SandboxGuarantees {
                filesystem: IsolationLevel::None,
                process: IsolationLevel::None,
                network: IsolationLevel::None,
                credentials: IsolationLevel::None,
                ephemeral: true,
                known_limitations: vec![
                    "Effect adapter invocation emits a structured host request; it does not execute adapter credentials inside a sandbox".into(),
                ],
            }
        } else {
            self.provider.guarantees()
        };
        let assurance = if matches!(
            sandbox.runtime,
            MicroSandboxRuntime::Host | MicroSandboxRuntime::EffectBoundary
        ) {
            AttestationAssurance::Observed
        } else {
            AttestationAssurance::IsolatedObserved
        };
        let runtime_info = RuntimeAttestationInfo {
            provider: format!("micro-sandbox/{:?}", sandbox.runtime),
            version: env!("CARGO_PKG_VERSION").into(),
            image_or_runtime_digest: None,
            isolation,
            assurance,
        };
        let attestation = ExecutionAttestation {
            id: crate::core::ExecutionAttestationId::new(),
            tool: tool.identity(),
            reality_id: reality.id.clone(),
            sandbox_id: sandbox.id.clone(),
            invocation_hash,
            tool_artifact_hash: tool.integrity.artifact_hash.clone(),
            tool_manifest_hash: tool.manifest_hash()?,
            reality_manifest_hash: reality_manifest.hash()?,
            effective_capability_hash: sandbox.capabilities.hash()?,
            input_hashes: vec![
                blake3::hash(&serde_json::to_vec(&invocation.input)?)
                    .to_hex()
                    .to_string(),
            ],
            output_hashes,
            effect_refs: result.effects.clone(),
            result: result.status,
            started_at,
            completed_at,
            runtime: runtime_info,
            input_artifacts: vec![],
            output_artifacts: result.outputs.clone(),
            assurance,
            recorded_hash: None,
        };
        let receipt = ToolExecutionReceipt {
            attestation_id: attestation.id.clone(),
            result: result.status,
            outputs: result.outputs.clone(),
            effects: result.effects.clone(),
        };
        let mut lifecycle = vec![
            make_lifecycle(ToolLifecycleEventKind::ToolRequested, &tool, &sandbox, None),
            make_lifecycle(ToolLifecycleEventKind::ToolResolved, &tool, &sandbox, None),
            make_lifecycle(
                ToolLifecycleEventKind::CapabilitiesComputed,
                &tool,
                &sandbox,
                None,
            ),
            make_lifecycle(
                ToolLifecycleEventKind::SandboxCreated,
                &tool,
                &sandbox,
                None,
            ),
            make_lifecycle(ToolLifecycleEventKind::ToolStarted, &tool, &sandbox, None),
            make_lifecycle(
                ToolLifecycleEventKind::ToolCompleted,
                &tool,
                &sandbox,
                result.error.clone(),
            ),
            make_lifecycle(ToolLifecycleEventKind::Attested, &tool, &sandbox, None),
            make_lifecycle(
                ToolLifecycleEventKind::SandboxDestroyed,
                &tool,
                &sandbox,
                None,
            ),
        ];
        if result.status != ToolExecutionStatus::Success {
            lifecycle.push(make_lifecycle(
                ToolLifecycleEventKind::Failed,
                &tool,
                &sandbox,
                result.error.clone(),
            ));
        }
        Ok(ToolRun {
            sandbox,
            result,
            attestation,
            receipt,
            lifecycle,
        })
    }
}

fn denied_by_intersection(
    requested: &ToolCapabilityManifest,
    effective: &EffectiveToolCapabilities,
) -> Option<String> {
    if requested.network.mode != NetworkMode::None && effective.network.mode == NetworkMode::None {
        return Some("tool network capability was denied by the Reality intersection".into());
    }
    if requested.process.allow_exec && !effective.process.allow_exec {
        return Some("tool process capability was denied by the Reality intersection".into());
    }
    if !requested.filesystem.read.is_empty() && effective.filesystem.read.is_empty() {
        return Some(
            "tool filesystem read capability was denied by the Reality intersection".into(),
        );
    }
    if !requested.filesystem.write.is_empty() && effective.filesystem.write.is_empty() {
        return Some(
            "tool filesystem write capability was denied by the Reality intersection".into(),
        );
    }
    if requested.effects.propose && !effective.effects.propose
        || requested.effects.prepare && !effective.effects.prepare
    {
        return Some("tool effect capability was denied by the Reality intersection".into());
    }
    if !requested.credentials.is_empty() && effective.credentials.is_empty() {
        return Some("tool credential capability was denied by the Reality intersection".into());
    }
    None
}

fn make_lifecycle(
    kind: ToolLifecycleEventKind,
    tool: &ToolDefinition,
    sandbox: &MicroSandbox,
    reason: Option<String>,
) -> ToolLifecycleEvent {
    ToolLifecycleEvent {
        id: uuid::Uuid::new_v4().to_string(),
        kind,
        tool_id: Some(tool.id.clone()),
        sandbox_id: Some(sandbox.id.clone()),
        reality_id: Some(sandbox.reality_id.clone()),
        created_at: Utc::now(),
        reason,
    }
}

impl<P: MicroSandboxProvider> ToolRouter<P> {
    pub fn persist_run(&self, store: &Store, run: &ToolRun) -> Result<()> {
        if store
            .tool_definitions(true)?
            .iter()
            .all(|tool| tool.id != run.attestation.tool.id)
        {
            return Err(Error::NotFound(format!(
                "Tool {} is not registered in the persistent store",
                run.attestation.tool.id
            )));
        }
        store.insert_micro_sandbox(&run.sandbox)?;
        for event in &run.lifecycle {
            store.insert_tool_lifecycle_event(event)?;
        }
        store.insert_execution_attestation(&run.attestation)
    }
}

impl Default for ToolExecutionResult {
    fn default() -> Self {
        Self {
            status: ToolExecutionStatus::RuntimeFailure,
            stdout: String::new(),
            stderr: String::new(),
            outputs: vec![],
            effects: vec![],
            truncated: false,
            started_at: None,
            completed_at: None,
            error: None,
        }
    }
}

fn effect_boundary_capabilities(
    capabilities: &EffectiveToolCapabilities,
) -> EffectiveToolCapabilities {
    let mut enforced = capabilities.clone();
    enforced.filesystem.read.clear();
    enforced.filesystem.write.clear();
    enforced.process.allow_exec = false;
    enforced.process.allowed_executables.clear();
    enforced.network.mode = NetworkMode::None;
    enforced.network.allow.clear();
    enforced.environment.readable.clear();
    enforced.environment.values.clear();
    enforced.credentials.clear();
    enforced
}
fn network_mode(network: &ToolNetworkCapabilities) -> &'static str {
    match network.mode {
        NetworkMode::None => "none",
        NetworkMode::LoopbackOnly => "none",
        NetworkMode::AllowList => "none",
        NetworkMode::Unrestricted => "bridge",
    }
}
fn short_id(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability::builtin_profile,
        core::RealityId,
        tool::{builtin_tools, resolve_effective_capabilities},
    };
    use std::{
        env, fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    const OUTPUT_LIMIT: usize = 4096;

    fn sandbox(runtime: MicroSandboxRuntime, timeout_ms: u64) -> (ToolDefinition, MicroSandbox) {
        let tool = builtin_tools()
            .into_iter()
            .find(|tool| tool.name == "shell-generic")
            .expect("shell tool");
        let manifest = builtin_profile("coding-networked").expect("profile");
        let mut capabilities = resolve_effective_capabilities(&manifest, &tool.capabilities, &[])
            .expect("capabilities");
        capabilities.resources.output_bytes = Some(OUTPUT_LIMIT as u64);
        capabilities.resources.timeout_ms = Some(timeout_ms);
        capabilities.duration.max_ms = Some(timeout_ms);
        let mut sandbox = new_micro_sandbox(RealityId::new(), &tool, capabilities, runtime);
        sandbox.expires_at = Utc::now() + chrono::Duration::minutes(1);
        (tool, sandbox)
    }

    fn invocation(tool: &ToolDefinition, script: String) -> ResolvedToolInvocation {
        ResolvedToolInvocation {
            tool: tool.identity(),
            executable: Some("/bin/sh".into()),
            args: vec!["-c".into(), script],
            input: Value::Null,
            effect_adapter: None,
        }
    }

    fn shell_path(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
    }

    fn pid(path: &Path) -> Pid {
        let raw = fs::read_to_string(path).expect("pid file");
        Pid::from_raw(raw.parse().expect("numeric pid"))
    }

    async fn wait_for_pid(path: &Path) -> Pid {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(raw) = fs::read_to_string(path)
                && let Ok(value) = raw.parse()
            {
                return Pid::from_raw(value);
            }
            assert!(
                Instant::now() < deadline,
                "{} did not contain a complete process id",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn assert_reaped(pid: Pid) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match nix::sys::signal::kill(pid, None) {
                Err(Errno::ESRCH) => return,
                Ok(()) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                result => panic!("process {pid} was not reaped: {result:?}"),
            }
        }
    }

    async fn assert_descendant_stopped(release: &Path, sentinel: &Path) {
        fs::write(release, "release").expect("release descendant");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !sentinel.exists(),
            "a process-group descendant survived cleanup"
        );
    }

    fn fake_container_runtime(directory: &Path) -> PathBuf {
        let runtime = directory.join("fake-container-runtime");
        fs::write(
            &runtime,
            "#!/bin/sh\nset -eu\ntest \"$1\" = exec\nshift\nshift\nexec \"$@\"\n",
        )
        .expect("fake runtime");
        let mut permissions = fs::metadata(&runtime)
            .expect("runtime metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&runtime, permissions).expect("runtime permissions");
        runtime
    }

    fn fake_container_runtime_checking_stdin(directory: &Path, observation: &Path) -> PathBuf {
        let runtime = directory.join("fake-container-runtime-stdin");
        fs::write(
            &runtime,
            format!(
                "#!/bin/sh\n\
                 set -eu\n\
                 if IFS= read -r input; then\n\
                   printf '%s' inherited > {observation}\n\
                   exit 97\n\
                 fi\n\
                 printf '%s' eof > {observation}\n\
                 test \"$1\" = exec\n\
                 shift\n\
                 shift\n\
                 exec \"$@\"\n",
                observation = shell_path(observation),
            ),
        )
        .expect("fake runtime");
        let mut permissions = fs::metadata(&runtime)
            .expect("runtime metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&runtime, permissions).expect("runtime permissions");
        runtime
    }

    #[tokio::test]
    async fn trusted_host_stdout_capture_is_bounded_and_stops_the_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let leader = temp.path().join("leader.pid");
        let release = temp.path().join("release");
        let sentinel = temp.path().join("survived");
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Host, 5_000);
        let provider = HostMicroSandboxProvider::trusted_development();
        provider.workspaces.lock().expect("workspaces").insert(
            sandbox.id.clone(),
            temp.path().canonicalize().expect("workspace"),
        );
        let script = format!(
            "printf '%s' \"$$\" > {leader}; \
             (while [ ! -e {release} ]; do sleep 0.01; done; touch {sentinel}) & \
             while :; do printf '0123456789abcdef'; done",
            leader = shell_path(&leader),
            release = shell_path(&release),
            sentinel = shell_path(&sentinel),
        );

        let result = provider
            .execute(&sandbox, &invocation(&tool, script))
            .await
            .expect("host execution");

        assert_eq!(result.status, ToolExecutionStatus::Failed);
        assert!(result.truncated);
        assert_eq!(result.stdout.len(), OUTPUT_LIMIT);
        assert!(result.stderr.is_empty());
        assert_reaped(pid(&leader)).await;
        assert_descendant_stopped(&release, &sentinel).await;
    }

    #[tokio::test]
    async fn container_stderr_capture_uses_the_same_bound_and_group_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let leader = temp.path().join("leader.pid");
        let release = temp.path().join("release");
        let sentinel = temp.path().join("survived");
        let runtime = fake_container_runtime(temp.path());
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Container, 5_000);
        let provider = ContainerMicroSandboxProvider::new(
            runtime.to_string_lossy().into_owned(),
            "fixture:image",
        )
        .expect("provider");
        provider
            .containers
            .lock()
            .expect("containers")
            .insert(sandbox.id.clone(), "fixture-container".into());
        let script = format!(
            "printf '%s' \"$$\" > {leader}; \
             (while [ ! -e {release} ]; do sleep 0.01; done; touch {sentinel}) & \
             while :; do printf 'fedcba9876543210' >&2; done",
            leader = shell_path(&leader),
            release = shell_path(&release),
            sentinel = shell_path(&sentinel),
        );

        let result = provider
            .execute(&sandbox, &invocation(&tool, script))
            .await
            .expect("container execution");

        assert_eq!(result.status, ToolExecutionStatus::Failed);
        assert!(result.truncated);
        assert!(result.stdout.is_empty());
        assert_eq!(result.stderr.len(), OUTPUT_LIMIT);
        assert_reaped(pid(&leader)).await;
        assert_descendant_stopped(&release, &sentinel).await;
    }

    #[tokio::test]
    async fn capture_deadline_preserves_timeout_result_and_stops_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let leader = temp.path().join("leader.pid");
        let release = temp.path().join("release");
        let sentinel = temp.path().join("survived");
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Host, 50);
        let provider = HostMicroSandboxProvider::trusted_development();
        provider.workspaces.lock().expect("workspaces").insert(
            sandbox.id.clone(),
            temp.path().canonicalize().expect("workspace"),
        );
        let script = format!(
            "printf '%s' \"$$\" > {leader}; \
             (while [ ! -e {release} ]; do sleep 0.01; done; touch {sentinel}) & \
             while :; do sleep 1; done",
            leader = shell_path(&leader),
            release = shell_path(&release),
            sentinel = shell_path(&sentinel),
        );

        let result = provider
            .execute(&sandbox, &invocation(&tool, script))
            .await
            .expect("timed execution");

        assert_eq!(result.status, ToolExecutionStatus::TimedOut);
        assert_eq!(result.error.as_deref(), Some("execution exceeded 50ms"));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(!result.truncated);
        assert_reaped(pid(&leader)).await;
        assert_descendant_stopped(&release, &sentinel).await;
    }

    #[tokio::test]
    async fn successful_leader_exit_stops_redirected_background_descendant() {
        let temp = tempfile::tempdir().expect("tempdir");
        let descendant = temp.path().join("descendant.pid");
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Host, 5_000);
        let provider = HostMicroSandboxProvider::trusted_development();
        provider.workspaces.lock().expect("workspaces").insert(
            sandbox.id.clone(),
            temp.path().canonicalize().expect("workspace"),
        );
        let script = format!(
            "sleep 300 </dev/null >/dev/null 2>&1 & \
             printf '%s' \"$!\" > {descendant}; \
             exit 0",
            descendant = shell_path(&descendant),
        );

        let result = provider
            .execute(&sandbox, &invocation(&tool, script))
            .await
            .expect("host execution");

        assert_eq!(result.status, ToolExecutionStatus::Success);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(!result.truncated);
        assert_reaped(pid(&descendant)).await;
    }

    #[tokio::test]
    async fn container_runtime_stdin_is_null() {
        let temp = tempfile::tempdir().expect("tempdir");
        let observation = temp.path().join("stdin-observation");
        let runtime = fake_container_runtime_checking_stdin(temp.path(), &observation);
        let mut command = std::process::Command::new(env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "tool_runtime::tests::container_runtime_stdin_helper",
                "--nocapture",
            ])
            .env("HARDKNOCK_TEST_CONTAINER_RUNTIME", &runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn helper test");
        child
            .stdin
            .take()
            .expect("helper stdin")
            .write_all(b"caller input must not reach the runtime\n")
            .expect("write helper stdin");
        let output = child.wait_with_output().expect("wait for helper test");

        assert!(
            output.status.success(),
            "helper failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            fs::read_to_string(observation).expect("stdin observation"),
            "eof"
        );
    }

    #[tokio::test]
    async fn container_runtime_stdin_helper() {
        let Some(runtime) = env::var_os("HARDKNOCK_TEST_CONTAINER_RUNTIME") else {
            return;
        };
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Container, 5_000);
        let provider =
            ContainerMicroSandboxProvider::new(runtime.to_string_lossy().into_owned(), "fixture")
                .expect("provider");
        provider
            .containers
            .lock()
            .expect("containers")
            .insert(sandbox.id.clone(), "fixture-container".into());
        let result = provider
            .execute(&sandbox, &invocation(&tool, "printf 'ok'".into()))
            .await
            .expect("container execution");

        assert_eq!(result.status, ToolExecutionStatus::Success);
        assert_eq!(result.stdout, "ok");
    }

    #[tokio::test]
    async fn dropping_execution_for_cancellation_stops_the_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let leader = temp.path().join("leader.pid");
        let release = temp.path().join("release");
        let sentinel = temp.path().join("survived");
        let (tool, sandbox) = sandbox(MicroSandboxRuntime::Host, 5_000);
        let provider = HostMicroSandboxProvider::trusted_development();
        provider.workspaces.lock().expect("workspaces").insert(
            sandbox.id.clone(),
            temp.path().canonicalize().expect("workspace"),
        );
        let script = format!(
            "printf '%s' \"$$\" > {leader}; \
             (while [ ! -e {release} ]; do sleep 0.01; done; touch {sentinel}) & \
             while :; do sleep 1; done",
            leader = shell_path(&leader),
            release = shell_path(&release),
            sentinel = shell_path(&sentinel),
        );
        let invocation = invocation(&tool, script);
        let execution = tokio::spawn({
            let provider = provider.clone();
            let sandbox = sandbox.clone();
            async move { provider.execute(&sandbox, &invocation).await }
        });

        let leader_pid = wait_for_pid(&leader).await;
        execution.abort();
        assert!(execution.await.expect_err("cancelled task").is_cancelled());
        assert_reaped(leader_pid).await;
        assert_descendant_stopped(&release, &sentinel).await;
    }
}
