// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    Error, Result,
    core::{ActionRecord, ArtifactKind, CommandSpec, ProcessStatus, Reality, RealityStatus},
    store::{CapabilityStore, Store, artifact, token_hash},
};
use chrono::Utc;
use std::{
    fs::{self, OpenOptions},
    future::Future,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    pin::Pin,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

const DEFAULT_OUTPUT_BYTES_PER_STREAM: u64 = 8 * 1024 * 1024;
const CAPTURE_BUFFER_BYTES: usize = 64 * 1024;
const FORCED_STOP_GRACE: Duration = Duration::from_secs(2);
const CONTAINER_FREEZE_TIMEOUT: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT_MARKER: &[u8] = b"\n[hardknock: output capture limit exceeded]\n";
const TIMEOUT_MARKER: &[u8] = b"\n[hardknock: container action timed out]\n";
const CAPTURE_FAILURE_MARKER: &[u8] = b"\n[hardknock: output capture failed]\n";
const FORCED_STOP_MARKER: &[u8] = b"\n[hardknock: container action stopped]\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturedStream {
    Stdout,
    Stderr,
}

impl CapturedStream {
    fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CaptureEvent {
    Limit(CapturedStream),
    Failed(CapturedStream),
}

#[derive(Default)]
struct CaptureState {
    bytes: Vec<u8>,
    limit_exceeded: bool,
    failure: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerStopReason {
    OutputLimit(CapturedStream),
    TimedOut,
    CaptureFailed(CapturedStream),
}

struct ContainerCommandOutput {
    exit: Option<ExitStatus>,
    stop_reason: Option<ContainerStopReason>,
    stop_error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum NormalizedAction {
    Shell(CommandSpec),
    FileRead { path: String },
    FileWrite { path: String, data: Vec<u8> },
    FileDelete { path: String },
    FileList { path: String },
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ActionResult {
    Process {
        status: ProcessStatus,
        action: ActionRecord,
    },
    FileData(Vec<u8>),
    FileList(Vec<String>),
    FileChanged,
}

pub type ProxyFuture<'a> = Pin<Box<dyn Future<Output = Result<ActionResult>> + 'a>>;

pub trait ToolExecutionProxy {
    fn execute<'a>(
        &'a self,
        reality: &'a Reality,
        token: &'a SignedRealityCapabilityToken,
        action: &'a NormalizedAction,
        artifacts: &'a Path,
    ) -> ProxyFuture<'a>;
}

pub struct CapabilityExecutionProxy<'a> {
    store: &'a Store,
    authority: CapabilityTokenAuthority,
    policy: DenyByDefaultCapabilityPolicy,
    redactor: SecretRedactor,
}

impl<'a> CapabilityExecutionProxy<'a> {
    pub fn new(store: &'a Store, redactor: SecretRedactor) -> Result<Self> {
        Ok(Self {
            store,
            authority: CapabilityTokenAuthority::load_or_create(&store.home)?,
            policy: DenyByDefaultCapabilityPolicy,
            redactor,
        })
    }

    fn authorize(
        &self,
        reality: &Reality,
        token: &SignedRealityCapabilityToken,
        manifest: &CapabilityManifest,
        token_operation: RealityTokenOperation,
        request: CapabilityRequest,
    ) -> Result<()> {
        self.authority
            .verify(token, reality, manifest, token_operation)?;
        if self.store.capability_token_revoked(&token_hash(token)?)? {
            return Err(Error::Intervention(
                "Capability token is revoked or was never issued by this Store".into(),
            ));
        }
        let evaluation = self.policy.evaluate(&request, manifest);
        self.store.append_capability_event(&CapabilityEvent {
            id: crate::core::CapabilityEventId::new(),
            reality_id: reality.id.clone(),
            manifest_id: manifest.id.clone(),
            kind: match evaluation.decision {
                CapabilityDecision::Allow => CapabilityEventKind::Allowed,
                CapabilityDecision::Deny => CapabilityEventKind::Denied,
                CapabilityDecision::RequireApproval => CapabilityEventKind::ApprovalRequired,
            },
            request: Some(request),
            reason: evaluation.reason.clone(),
            created_at: Utc::now(),
        })?;
        if evaluation.decision == CapabilityDecision::Allow {
            Ok(())
        } else {
            Err(Error::Intervention(evaluation.reason))
        }
    }

    fn file_action(
        &self,
        reality: &Reality,
        token: &SignedRealityCapabilityToken,
        manifest: &CapabilityManifest,
        operation: FilesystemOperation,
        path: &str,
    ) -> Result<PathBuf> {
        self.authorize(
            reality,
            token,
            manifest,
            match operation {
                FilesystemOperation::Read | FilesystemOperation::List => {
                    RealityTokenOperation::FileRead
                }
                FilesystemOperation::Write | FilesystemOperation::Delete => {
                    RealityTokenOperation::FileWrite
                }
            },
            CapabilityRequest::Filesystem {
                operation,
                path: path.into(),
            },
        )?;
        resolve_workspace_path(&reality.root, path)
    }

    async fn shell(
        &self,
        reality: &Reality,
        token: &SignedRealityCapabilityToken,
        manifest: &CapabilityManifest,
        spec: &CommandSpec,
        artifacts: &Path,
    ) -> Result<ActionResult> {
        self.authorize(
            reality,
            token,
            manifest,
            RealityTokenOperation::Shell,
            CapabilityRequest::Process {
                executable: spec.program.clone(),
            },
        )?;
        if reality.status == RealityStatus::Discarded || reality.execution_boundary.frozen {
            return Err(Error::Intervention(
                "Reality is discarded or frozen; process execution is disabled".into(),
            ));
        }
        for key in spec.environment_overrides.keys() {
            if manifest.environment.values.get(key) != spec.environment_overrides.get(key) {
                return Err(Error::Intervention(format!(
                    "Environment override {key} does not exactly match the manifest"
                )));
            }
        }
        let runtime: ContainerRuntimeMetadata = self.store.provider_runtime(&reality.id)?;
        if reality.execution_boundary.provider != "container" {
            return Err(Error::Intervention(
                "Capability shell proxy requires a container Reality".into(),
            ));
        }
        let started_at = Utc::now();
        let started = Instant::now();
        let credentials =
            StaticTestCredentialBroker::new(self.store)?.materialize_for_action(reality)?;
        let redactor = self
            .redactor
            .including(credentials.secrets().iter().cloned());
        let mut arguments = vec!["exec".to_owned(), "--workdir".into(), "/workspace".into()];
        for (name, value) in credentials.environment() {
            arguments.extend(["--env".into(), format!("{name}={value}")]);
        }
        for (name, value) in &spec.environment_overrides {
            arguments.extend(["--env".into(), format!("{name}={value}")]);
        }
        arguments.push(runtime.container_id.clone());
        arguments.push(spec.program.clone());
        arguments.extend(spec.args.clone());
        let mut command = Command::new(&runtime.runtime);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let timeout = Duration::from_millis(manifest.resources.timeout_ms.unwrap_or(300_000));
        let maximum = manifest
            .resources
            .output_bytes
            .unwrap_or(DEFAULT_OUTPUT_BYTES_PER_STREAM);
        let reserved_bytes = maximum.checked_mul(2).ok_or_else(|| {
            Error::InvalidInput("Container output reservation exceeds the supported size".into())
        })?;
        let _artifact_capacity = self.store.reserve_artifact_capacity(reserved_bytes, 2)?;
        let output = run_container_command(
            command,
            &runtime.runtime,
            &runtime.container_id,
            artifacts,
            timeout,
            maximum,
            &redactor,
        )
        .await?;
        if let Some(error) = output.stop_error {
            return Err(Error::Intervention(format!(
                "Container action could not be stopped cleanly: {error}; bounded output remains at {}",
                artifacts.display()
            )));
        }
        match output.stop_reason {
            Some(ContainerStopReason::TimedOut) => {
                return Err(Error::Intervention(format!(
                    "Container action timed out; Reality container was stopped and bounded output remains at {}",
                    artifacts.display()
                )));
            }
            Some(ContainerStopReason::CaptureFailed(stream)) => {
                return Err(Error::Intervention(format!(
                    "Container {} capture failed; Reality container was stopped and bounded output remains at {}",
                    stream.name(),
                    artifacts.display()
                )));
            }
            _ => {}
        }
        let exit = output.exit.ok_or_else(|| {
            Error::Intervention(format!(
                "Container action did not report an exit status; bounded output remains at {}",
                artifacts.display()
            ))
        })?;
        let stdout_path = artifacts.join("stdout.log");
        let stderr_path = artifacts.join("stderr.log");
        let status = if matches!(
            output.stop_reason,
            Some(ContainerStopReason::OutputLimit(_))
        ) {
            ProcessStatus::Failed
        } else if exit.success() {
            ProcessStatus::Succeeded
        } else {
            ProcessStatus::Failed
        };
        let action = ActionRecord {
            command: CommandSpec {
                program: spec.program.clone(),
                args: spec.args.clone(),
                environment: crate::core::EnvironmentMode::Controlled,
                // Do not persist possibly sensitive injected values.
                environment_overrides: Default::default(),
            },
            cwd: PathBuf::from("/workspace"),
            started_at,
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            exit_code: exit.code(),
            signal: exit.signal(),
            stdout: artifact(&stdout_path)?.with_kind(ArtifactKind::Stdout),
            stderr: artifact(&stderr_path)?.with_kind(ArtifactKind::Stderr),
        };
        Ok(ActionResult::Process { status, action })
    }
}

impl ToolExecutionProxy for CapabilityExecutionProxy<'_> {
    fn execute<'a>(
        &'a self,
        reality: &'a Reality,
        token: &'a SignedRealityCapabilityToken,
        action: &'a NormalizedAction,
        artifacts: &'a Path,
    ) -> ProxyFuture<'a> {
        Box::pin(async move {
            let manifest = self.store.effective_capability_manifest(&reality.id)?;
            match action {
                NormalizedAction::Shell(spec) => {
                    self.shell(reality, token, &manifest, spec, artifacts).await
                }
                NormalizedAction::FileRead { path } => {
                    let path = self.file_action(
                        reality,
                        token,
                        &manifest,
                        FilesystemOperation::Read,
                        path,
                    )?;
                    Ok(ActionResult::FileData(
                        self.redactor.redact(&fs::read(path)?),
                    ))
                }
                NormalizedAction::FileWrite { path, data } => {
                    if data.len() > 8 * 1024 * 1024 {
                        return Err(Error::InvalidInput(
                            "File proxy write exceeded 8 MiB".into(),
                        ));
                    }
                    let path = self.file_action(
                        reality,
                        token,
                        &manifest,
                        FilesystemOperation::Write,
                        path,
                    )?;
                    fs::write(path, data)?;
                    Ok(ActionResult::FileChanged)
                }
                NormalizedAction::FileDelete { path } => {
                    let path = self.file_action(
                        reality,
                        token,
                        &manifest,
                        FilesystemOperation::Delete,
                        path,
                    )?;
                    fs::remove_file(path)?;
                    Ok(ActionResult::FileChanged)
                }
                NormalizedAction::FileList { path } => {
                    let path = self.file_action(
                        reality,
                        token,
                        &manifest,
                        FilesystemOperation::List,
                        path,
                    )?;
                    let mut entries = fs::read_dir(path)?
                        .map(|entry| {
                            entry.map(|entry| entry.file_name().to_string_lossy().into_owned())
                        })
                        .collect::<std::io::Result<Vec<_>>>()?;
                    entries.sort();
                    Ok(ActionResult::FileList(entries))
                }
            }
        })
    }
}

fn capture_lock(state: &Arc<Mutex<CaptureState>>) -> MutexGuard<'_, CaptureState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn capture_stream<R>(
    mut input: R,
    state: Arc<Mutex<CaptureState>>,
    stream: CapturedStream,
    maximum: usize,
    events: mpsc::UnboundedSender<CaptureEvent>,
) where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    loop {
        let read = match input.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                capture_lock(&state).failure = Some(error.to_string());
                let _ = events.send(CaptureEvent::Failed(stream));
                return;
            }
        };
        if read == 0 {
            return;
        }

        let exceeded = {
            let mut state = capture_lock(&state);
            let available = maximum.saturating_sub(state.bytes.len());
            let retained = available.min(read);
            state.bytes.extend_from_slice(&buffer[..retained]);
            if retained < read {
                state.limit_exceeded = true;
                true
            } else {
                false
            }
        };
        if exceeded {
            let _ = events.send(CaptureEvent::Limit(stream));
            return;
        }
    }
}

fn detected_capture_stop(
    stdout: &Arc<Mutex<CaptureState>>,
    stderr: &Arc<Mutex<CaptureState>>,
) -> Option<ContainerStopReason> {
    let stdout = capture_lock(stdout);
    let stderr = capture_lock(stderr);
    if stdout.limit_exceeded {
        Some(ContainerStopReason::OutputLimit(CapturedStream::Stdout))
    } else if stderr.limit_exceeded {
        Some(ContainerStopReason::OutputLimit(CapturedStream::Stderr))
    } else if stdout.failure.is_some() {
        Some(ContainerStopReason::CaptureFailed(CapturedStream::Stdout))
    } else if stderr.failure.is_some() {
        Some(ContainerStopReason::CaptureFailed(CapturedStream::Stderr))
    } else {
        None
    }
}

fn mark_capture_failure(state: &Arc<Mutex<CaptureState>>, message: impl Into<String>) {
    let mut state = capture_lock(state);
    if state.failure.is_none() {
        state.failure = Some(message.into());
    }
}

async fn settle_capture_tasks(
    stdout_task: &mut JoinHandle<()>,
    stderr_task: &mut JoinHandle<()>,
    stdout: &Arc<Mutex<CaptureState>>,
    stderr: &Arc<Mutex<CaptureState>>,
    timeout: Duration,
) -> bool {
    let joined = tokio::time::timeout(timeout, async {
        tokio::join!(&mut *stdout_task, &mut *stderr_task)
    })
    .await;
    match joined {
        Ok((stdout_join, stderr_join)) => {
            if let Err(error) = stdout_join {
                mark_capture_failure(stdout, format!("stdout capture task failed: {error}"));
            }
            if let Err(error) = stderr_join {
                mark_capture_failure(stderr, format!("stderr capture task failed: {error}"));
            }
            true
        }
        Err(_) => {
            let stdout_pending = !stdout_task.is_finished();
            let stderr_pending = !stderr_task.is_finished();
            stdout_task.abort();
            stderr_task.abort();
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            if stdout_pending {
                mark_capture_failure(stdout, "stdout capture did not reach EOF");
            }
            if stderr_pending {
                mark_capture_failure(stderr, "stderr capture did not reach EOF");
            }
            false
        }
    }
}

async fn freeze_container(runtime: &str, container_id: &str) -> Option<String> {
    let mut command = Command::new(runtime);
    command
        .args(["kill", container_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(CONTAINER_FREEZE_TIMEOUT, command.status()).await {
        Ok(Ok(status)) if status.success() => None,
        Ok(Ok(status)) => Some(format!(
            "container runtime kill exited with status {status}"
        )),
        Ok(Err(error)) => Some(format!("container runtime kill failed: {error}")),
        Err(_) => Some(format!(
            "container runtime kill exceeded {} ms",
            CONTAINER_FREEZE_TIMEOUT.as_millis()
        )),
    }
}

fn append_stop_error(target: &mut Option<String>, error: impl Into<String>) {
    let error = error.into();
    if let Some(target) = target {
        target.push_str("; ");
        target.push_str(&error);
    } else {
        *target = Some(error);
    }
}

async fn stop_runtime_client(child: &mut Child) -> (Option<ExitStatus>, Option<String>) {
    let mut error = None;
    if let Err(source) = child.start_kill() {
        match child.try_wait() {
            Ok(Some(exit)) => return (Some(exit), None),
            Ok(None) => append_stop_error(
                &mut error,
                format!("could not terminate container runtime client: {source}"),
            ),
            Err(wait_error) => append_stop_error(
                &mut error,
                format!(
                    "could not terminate container runtime client: {source}; status check failed: {wait_error}"
                ),
            ),
        }
    }
    match tokio::time::timeout(FORCED_STOP_GRACE, child.wait()).await {
        Ok(Ok(exit)) => (Some(exit), error),
        Ok(Err(source)) => {
            append_stop_error(
                &mut error,
                format!("could not reap container runtime client: {source}"),
            );
            (None, error)
        }
        Err(_) => {
            append_stop_error(
                &mut error,
                format!(
                    "container runtime client did not stop within {} ms",
                    FORCED_STOP_GRACE.as_millis()
                ),
            );
            (None, error)
        }
    }
}

fn bounded_with_marker(input: &[u8], maximum: usize, marker: &[u8]) -> Vec<u8> {
    let retained_marker = marker.len().min(maximum);
    let retained_input = input.len().min(maximum.saturating_sub(retained_marker));
    let mut output = Vec::with_capacity(retained_input + retained_marker);
    output.extend_from_slice(&input[..retained_input]);
    output.extend_from_slice(&marker[..retained_marker]);
    output
}

fn finalized_capture(
    state: &CaptureState,
    redactor: &SecretRedactor,
    maximum: usize,
    stop_reason: Option<ContainerStopReason>,
    stream: CapturedStream,
) -> Vec<u8> {
    let redacted = redactor.redact(&state.bytes);
    let marker = if state.limit_exceeded {
        Some(OUTPUT_LIMIT_MARKER)
    } else if state.failure.is_some() {
        Some(CAPTURE_FAILURE_MARKER)
    } else {
        match stop_reason {
            Some(ContainerStopReason::TimedOut) => Some(TIMEOUT_MARKER),
            Some(ContainerStopReason::OutputLimit(exceeded)) if exceeded != stream => {
                Some(FORCED_STOP_MARKER)
            }
            Some(ContainerStopReason::CaptureFailed(failed)) if failed != stream => {
                Some(FORCED_STOP_MARKER)
            }
            _ => None,
        }
    };
    match marker {
        Some(marker) => bounded_with_marker(&redacted, maximum, marker),
        None if redacted.len() > maximum => {
            bounded_with_marker(&redacted, maximum, OUTPUT_LIMIT_MARKER)
        }
        None => redacted,
    }
}

fn write_private_artifact(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

async fn run_container_command(
    mut command: Command,
    runtime: &str,
    container_id: &str,
    artifacts: &Path,
    timeout: Duration,
    maximum: u64,
    redactor: &SecretRedactor,
) -> Result<ContainerCommandOutput> {
    let maximum = usize::try_from(maximum).map_err(|_| {
        Error::InvalidInput("Container output limit does not fit this platform".into())
    })?;
    if maximum == 0 {
        return Err(Error::InvalidInput(
            "Container output limit must be positive".into(),
        ));
    }

    fs::create_dir(artifacts)?;
    fs::set_permissions(artifacts, fs::Permissions::from_mode(0o700))?;
    let stdout_path = artifacts.join("stdout.log");
    let stderr_path = artifacts.join("stderr.log");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::InvalidInput("Container action has no stdout pipe".into()))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::InvalidInput("Container action has no stderr pipe".into()))?;
    let stdout = Arc::new(Mutex::new(CaptureState::default()));
    let stderr = Arc::new(Mutex::new(CaptureState::default()));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut stdout_task = tokio::spawn(capture_stream(
        child_stdout,
        Arc::clone(&stdout),
        CapturedStream::Stdout,
        maximum,
        events_tx.clone(),
    ));
    let mut stderr_task = tokio::spawn(capture_stream(
        child_stderr,
        Arc::clone(&stderr),
        CapturedStream::Stderr,
        maximum,
        events_tx,
    ));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut events_open = true;
    let mut exit = None;
    let mut stop_reason = loop {
        tokio::select! {
            biased;
            event = events_rx.recv(), if events_open => {
                match event {
                    Some(CaptureEvent::Limit(stream)) => {
                        break Some(ContainerStopReason::OutputLimit(stream));
                    }
                    Some(CaptureEvent::Failed(stream)) => {
                        break Some(ContainerStopReason::CaptureFailed(stream));
                    }
                    None => events_open = false,
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                break Some(ContainerStopReason::TimedOut);
            }
            result = child.wait() => {
                exit = Some(result?);
                break None;
            }
        }
    };
    let mut stop_error = None;

    if stop_reason.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if !settle_capture_tasks(
            &mut stdout_task,
            &mut stderr_task,
            &stdout,
            &stderr,
            remaining,
        )
        .await
        {
            stop_reason = Some(ContainerStopReason::TimedOut);
        } else {
            stop_reason = detected_capture_stop(&stdout, &stderr);
        }
    }

    if stop_reason.is_some() {
        if exit.is_none() {
            let (stopped_exit, error) = stop_runtime_client(&mut child).await;
            exit = stopped_exit;
            if let Some(error) = error {
                append_stop_error(&mut stop_error, error);
            }
        }
        if let Some(error) = freeze_container(runtime, container_id).await {
            append_stop_error(&mut stop_error, error);
        }
        if (!stdout_task.is_finished() || !stderr_task.is_finished())
            && !settle_capture_tasks(
                &mut stdout_task,
                &mut stderr_task,
                &stdout,
                &stderr,
                FORCED_STOP_GRACE,
            )
            .await
        {
            append_stop_error(
                &mut stop_error,
                "output capture did not finish after the container was stopped",
            );
        }
    }

    let stdout_data = finalized_capture(
        &capture_lock(&stdout),
        redactor,
        maximum,
        stop_reason,
        CapturedStream::Stdout,
    );
    let stderr_data = finalized_capture(
        &capture_lock(&stderr),
        redactor,
        maximum,
        stop_reason,
        CapturedStream::Stderr,
    );
    write_private_artifact(&stdout_path, &stdout_data)?;
    write_private_artifact(&stderr_path, &stderr_data)?;

    if let Some(ContainerStopReason::OutputLimit(stream)) = stop_reason {
        tracing::warn!(
            stream = stream.name(),
            limit = maximum,
            artifacts = %artifacts.display(),
            "Container output capture limit exceeded; Reality container was stopped"
        );
    }
    Ok(ContainerCommandOutput {
        exit,
        stop_reason,
        stop_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability::{
            ContainerRuntimeLifecycle, ExecutionBoundary, RealityProviderCapabilities,
            builtin_profile,
        },
        core::{EnvironmentMode, RealityId, StateRef},
    };
    use std::os::unix::fs::PermissionsExt;

    const SECRET: &[u8] = b"TOKEN-12345";

    fn fake_runtime(directory: &Path) -> PathBuf {
        let runtime = directory.join("fake-runtime");
        fs::write(
            &runtime,
            r#"#!/bin/sh
set -eu
base=${0%/*}
operation=$1
shift
case "$operation" in
  exec)
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --workdir|--env)
          shift 2
          ;;
        *)
          break
          ;;
      esac
    done
    test "$#" -ge 2
    container_id=$1
    shift
    mode=$1
    shift
    case "$mode" in
      nonzero)
        printf 'stdout TOKEN-12345 retained\n'
        printf 'stderr TOKEN-12345 retained\n' >&2
        exit 7
        ;;
      dual)
        (
          i=0
          while [ "$i" -lt 4096 ]; do
            printf 'stdout-012345678901234567890123456789012345678901234567890123456789\n'
            i=$((i + 1))
          done
        ) &
        stdout_pid=$!
        (
          i=0
          while [ "$i" -lt 4096 ]; do
            printf 'stderr-012345678901234567890123456789012345678901234567890123456789\n'
            i=$((i + 1))
          done
        ) >&2 &
        stderr_pid=$!
        wait "$stdout_pid"
        wait "$stderr_pid"
        ;;
      overflow)
        while :; do
          printf 'stdout TOKEN-12345 012345678901234567890123456789\n'
          printf 'stderr TOKEN-12345 012345678901234567890123456789\n' >&2
        done
        ;;
      timeout)
        printf 'stdout TOKEN-12345 before timeout\n'
        printf 'stderr TOKEN-12345 before timeout\n' >&2
        while :; do :; done
        ;;
      reservation)
        reservation_directory=$1
        count=0
        for reservation in "$reservation_directory"/*.json; do
          if [ -f "$reservation" ]; then
            count=$((count + 1))
          fi
        done
        printf 'active-reservations=%s TOKEN-12345\n' "$count"
        exit 7
        ;;
      *)
        printf 'unknown fake mode: %s\n' "$mode" >&2
        exit 64
        ;;
    esac
    ;;
  kill)
    test "$#" -eq 1
    : > "$base/killed"
    ;;
  *)
    printf 'unknown fake operation: %s\n' "$operation" >&2
    exit 64
    ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        runtime
    }

    fn fake_command(runtime: &Path, mode: &str) -> Command {
        let mut command = Command::new(runtime);
        command
            .args(["exec", "--workdir", "/workspace", "fake-container", mode])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn run_fake(
        directory: &Path,
        mode: &str,
        artifacts: &Path,
        timeout: Duration,
        maximum: u64,
    ) -> ContainerCommandOutput {
        let runtime = fake_runtime(directory);
        run_container_command(
            fake_command(&runtime, mode),
            runtime.to_str().unwrap(),
            "fake-container",
            artifacts,
            timeout,
            maximum,
            &SecretRedactor::new([SECRET.to_vec()]),
        )
        .await
        .unwrap()
    }

    fn assert_private_bounded_artifacts(artifacts: &Path, maximum: u64) {
        assert_eq!(
            fs::metadata(artifacts).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["stdout.log", "stderr.log"] {
            let metadata = fs::metadata(artifacts.join(name)).unwrap();
            assert!(metadata.len() <= maximum, "{name} exceeded {maximum}");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            let captured = fs::read(artifacts.join(name)).unwrap();
            assert!(
                !captured
                    .windows(SECRET.len())
                    .any(|window| window == SECRET),
                "{name} retained an unredacted secret"
            );
        }
    }

    #[tokio::test]
    async fn drains_stdout_and_stderr_concurrently_without_deadlock() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = temp.path().join("dual-artifacts");
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            run_fake(
                temp.path(),
                "dual",
                &artifacts,
                Duration::from_secs(4),
                512 * 1024,
            ),
        )
        .await
        .expect("concurrent pipe capture deadlocked");

        assert_eq!(output.stop_reason, None);
        assert!(output.stop_error.is_none());
        assert!(output.exit.unwrap().success());
        assert!(fs::metadata(artifacts.join("stdout.log")).unwrap().len() > 64 * 1024);
        assert!(fs::metadata(artifacts.join("stderr.log")).unwrap().len() > 64 * 1024);
        assert_private_bounded_artifacts(&artifacts, 512 * 1024);
    }

    #[tokio::test]
    async fn output_limit_stops_container_and_retains_bounded_private_redacted_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = temp.path().join("overflow-artifacts");
        let output = run_fake(
            temp.path(),
            "overflow",
            &artifacts,
            Duration::from_secs(5),
            1024,
        )
        .await;

        assert!(matches!(
            output.stop_reason,
            Some(ContainerStopReason::OutputLimit(_))
        ));
        assert!(output.stop_error.is_none(), "{:?}", output.stop_error);
        assert!(output.exit.is_some());
        assert!(temp.path().join("killed").is_file());
        assert_private_bounded_artifacts(&artifacts, 1024);
        let stdout = fs::read(artifacts.join("stdout.log")).unwrap();
        let stderr = fs::read(artifacts.join("stderr.log")).unwrap();
        assert!(
            stdout.ends_with(OUTPUT_LIMIT_MARKER) || stderr.ends_with(OUTPUT_LIMIT_MARKER),
            "the exceeded stream did not retain the output-limit marker"
        );
    }

    #[tokio::test]
    async fn timeout_stops_container_and_marks_bounded_private_redacted_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = temp.path().join("timeout-artifacts");
        let output = run_fake(
            temp.path(),
            "timeout",
            &artifacts,
            Duration::from_millis(50),
            512,
        )
        .await;

        assert_eq!(output.stop_reason, Some(ContainerStopReason::TimedOut));
        assert!(output.stop_error.is_none(), "{:?}", output.stop_error);
        assert!(output.exit.is_some());
        assert!(temp.path().join("killed").is_file());
        assert_private_bounded_artifacts(&artifacts, 512);
        for name in ["stdout.log", "stderr.log"] {
            let captured = fs::read(artifacts.join(name)).unwrap();
            assert!(
                captured.ends_with(TIMEOUT_MARKER),
                "{name} did not retain the timeout marker"
            );
        }
    }

    #[tokio::test]
    async fn shell_holds_capacity_reservation_and_preserves_nonzero_action_record() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let runtime = fake_runtime(temp.path());
        let store = Store::open(&home).unwrap();
        let mut manifest = builtin_profile("coding-offline").unwrap();
        manifest.resources.output_bytes = Some(4096);
        manifest.resources.timeout_ms = Some(2_000);
        let manifest_hash = manifest.hash().unwrap();
        let reality = Reality {
            id: RealityId::new(),
            parent: None,
            fork_reason: None,
            experiment_id: None,
            candidate_id: None,
            effect_ledger: None,
            execution_boundary: ExecutionBoundary {
                provider: "container".into(),
                capabilities: RealityProviderCapabilities::container(manifest.network.mode),
                manifest_id: Some(manifest.id.clone()),
                manifest_hash: Some(manifest_hash),
                manifest_revision: manifest.revision,
                image_digest: Some("sha256:test".into()),
                frozen: false,
            },
            root: workspace.clone(),
            starting_state: StateRef {
                repo_path: workspace,
                git_commit: "test-commit".into(),
                tree_hash: "test-tree".into(),
            },
            created_at: Utc::now(),
            status: RealityStatus::Created,
            ephemeral: true,
        };
        store.insert_reality(&reality).unwrap();
        store
            .insert_capability_manifest(&reality.id, &manifest)
            .unwrap();
        store
            .put_provider_runtime(
                &reality.id,
                "container",
                &ContainerRuntimeMetadata {
                    runtime: runtime.to_string_lossy().into_owned(),
                    container_id: "fake-container".into(),
                    container_name: "fake-container".into(),
                    image: "fake-image".into(),
                    image_digest: "sha256:test".into(),
                    network_name: None,
                    attached_fixture_containers: Vec::new(),
                    lifecycle: ContainerRuntimeLifecycle::Ready,
                    created_at: Utc::now(),
                },
            )
            .unwrap();
        let authority = CapabilityTokenAuthority::load_or_create(&store.home).unwrap();
        let token = authority
            .issue(&reality, &manifest, chrono::Duration::minutes(5))
            .unwrap();
        store.audit_capability_token(&token).unwrap();
        let proxy =
            CapabilityExecutionProxy::new(&store, SecretRedactor::new([SECRET.to_vec()])).unwrap();
        let reservation_directory = store.home.join("locks").join("artifact-reservations");
        let spec = CommandSpec {
            program: "reservation".into(),
            args: vec![reservation_directory.to_string_lossy().into_owned()],
            environment: EnvironmentMode::Controlled,
            environment_overrides: Default::default(),
        };
        let artifacts = temp.path().join("proxy-artifacts");

        let result = proxy
            .execute(
                &reality,
                &token,
                &NormalizedAction::Shell(spec.clone()),
                &artifacts,
            )
            .await
            .unwrap();

        let ActionResult::Process { status, action } = result else {
            panic!("expected process action");
        };
        assert_eq!(status, ProcessStatus::Failed);
        assert_eq!(action.command.program, spec.program);
        assert_eq!(action.command.args, spec.args);
        assert_eq!(action.command.environment, EnvironmentMode::Controlled);
        assert!(action.command.environment_overrides.is_empty());
        assert_eq!(action.cwd, Path::new("/workspace"));
        assert_eq!(action.exit_code, Some(7));
        assert_eq!(action.signal, None);
        assert_eq!(action.stdout.path, artifacts.join("stdout.log"));
        assert_eq!(action.stderr.path, artifacts.join("stderr.log"));
        let stdout = fs::read(&action.stdout.path).unwrap();
        assert!(
            String::from_utf8_lossy(&stdout).contains("active-reservations=1"),
            "capacity reservation was not held while the runtime executed: {}",
            String::from_utf8_lossy(&stdout)
        );
        assert!(String::from_utf8_lossy(&stdout).contains("[REDACTED]"));
        assert_private_bounded_artifacts(&artifacts, 4096);
    }
}
