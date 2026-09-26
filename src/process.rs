// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, OpenOptions},
    future::Future,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use chrono::Utc;
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use tokio::process::Command;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

use crate::{
    Error, Result,
    core::{ActionRecord, ArtifactKind, CommandSpec, EnvironmentMode, ProcessStatus},
    experience::controlled_environment,
    store::artifact,
};

pub struct ProcessRunner;

struct ProcessGroup(Option<Pid>);

pub const MAX_CAPTURE_BYTES_PER_STREAM: u64 = 8 * 1024 * 1024;
const PROCESS_GROUP_SWEEP_WINDOW: Duration = Duration::from_millis(100);
const PROCESS_GROUP_SWEEP_INTERVAL: Duration = Duration::from_millis(5);
const OUTPUT_LIMIT_MARKER: &[u8] = b"\n[hardknock: output capture limit exceeded]\n";

async fn capture_bounded<R>(
    mut input: R,
    output: std::fs::File,
    path: PathBuf,
    stream: &'static str,
    limit_signal: mpsc::Sender<&'static str>,
) -> Result<bool>
where
    R: AsyncRead + Unpin,
{
    let mut output = tokio::fs::File::from_std(output);
    let data_limit = MAX_CAPTURE_BYTES_PER_STREAM.saturating_sub(OUTPUT_LIMIT_MARKER.len() as u64);
    let mut captured = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).await?;
        if read == 0 {
            output.flush().await?;
            output.sync_all().await?;
            return Ok(false);
        }
        let available = data_limit.saturating_sub(captured) as usize;
        let retained = read.min(available);
        if retained > 0 {
            output.write_all(&buffer[..retained]).await?;
            captured = captured.saturating_add(retained as u64);
        }
        if retained < read {
            output.write_all(OUTPUT_LIMIT_MARKER).await?;
            output.flush().await?;
            output.sync_all().await?;
            let _ = limit_signal.send(stream).await;
            tracing::warn!(
                stream,
                path = %path.display(),
                limit = MAX_CAPTURE_BYTES_PER_STREAM,
                "Process output capture limit exceeded"
            );
            return Ok(true);
        }
    }
}

impl ProcessGroup {
    fn signal(&self, leader_reaped: bool) -> Result<bool> {
        let Some(pid) = self.0 else {
            return Ok(false);
        };
        match killpg(pid, Signal::SIGKILL) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            // Darwin can report EPERM for a group that has no signalable
            // members after its leader is reaped. Ordinary descendants inherit
            // the leader's credentials, so a post-reap EPERM is quiescent for
            // the process tree this runner owns.
            Err(Errno::EPERM) if leader_reaped => Ok(false),
            Err(error) => Err(Error::Io(std::io::Error::from_raw_os_error(error as i32))),
        }
    }

    fn kill(&self) -> Result<()> {
        self.signal(false).map(drop)
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
        // Best effort fallback if the runner future is dropped during cancellation.
        if let Err(error) = self.kill() {
            tracing::error!(%error, "Could not stop process group");
        }
    }
}

impl ProcessRunner {
    pub async fn run<F: Future<Output = ()>>(
        &self,
        spec: &CommandSpec,
        cwd: &Path,
        artifacts: &Path,
        timeout: Duration,
        cancel: F,
    ) -> Result<(ProcessStatus, ActionRecord)> {
        fs::create_dir(artifacts)?;
        fs::set_permissions(artifacts, fs::Permissions::from_mode(0o700))?;
        let stdout_path = artifacts.join("stdout.log");
        let stderr_path = artifacts.join("stderr.log");
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stdout_path)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stderr_path)?;
        let started_at = Utc::now();
        let start = Instant::now();
        let mut command = Command::new(&spec.program);
        if spec.environment == EnvironmentMode::Controlled {
            command.env_clear().envs(controlled_environment(cwd));
        }
        command.envs(&spec.environment_overrides);
        let mut child = command
            .args(&spec.args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| Error::ProcessStart {
                program: spec.program.clone(),
                source,
            })?;
        let pid = child
            .id()
            .ok_or_else(|| Error::InvalidInput("Spawned process has no PID".into()))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::InvalidInput("Spawned process has no stdout pipe".into()))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::InvalidInput("Spawned process has no stderr pipe".into()))?;
        let (limit_tx, mut limit_rx) = mpsc::channel(2);
        let stdout_capture = tokio::spawn(capture_bounded(
            child_stdout,
            stdout,
            stdout_path.clone(),
            "stdout",
            limit_tx.clone(),
        ));
        let stderr_capture = tokio::spawn(capture_bounded(
            child_stderr,
            stderr,
            stderr_path.clone(),
            "stderr",
            limit_tx,
        ));
        let mut group = ProcessGroup(Some(Pid::from_raw(pid as i32)));
        tracing::debug!(
            pid,
            "Started agent process (arguments and environment omitted)"
        );
        let mut exceeded_stream = None;
        let (status, exit) = tokio::select! {
            biased;
            _ = cancel => {
                group.kill()?;
                (ProcessStatus::Interrupted, child.wait().await?)
            }
            _ = tokio::time::sleep(timeout) => {
                group.kill()?;
                (ProcessStatus::TimedOut, child.wait().await?)
            }
            Some(stream) = limit_rx.recv() => {
                exceeded_stream = Some(stream);
                group.kill()?;
                (ProcessStatus::Failed, child.wait().await?)
            }
            exit = child.wait() => {
                let exit = exit?;
                (if exit.success() { ProcessStatus::Succeeded } else { ProcessStatus::Failed }, exit)
            }
        };
        // killpg() signals only the members present during that system call. A
        // shell can be in fork() at the same time and create a group member that
        // misses the first signal. Once the leader is reaped it cannot fork
        // again, so bounded follow-up sweeps close that race for ordinary
        // descendants. Processes that deliberately establish another group or
        // session remain outside this boundary.
        group.terminate_remaining().await?;
        group.0 = None;
        drop(group);
        let stdout_exceeded = stdout_capture
            .await
            .map_err(|_| Error::InvalidInput("stdout capture task failed".into()))??;
        let stderr_exceeded = stderr_capture
            .await
            .map_err(|_| Error::InvalidInput("stderr capture task failed".into()))??;
        exceeded_stream = exceeded_stream.or_else(|| {
            stdout_exceeded
                .then_some("stdout")
                .or_else(|| stderr_exceeded.then_some("stderr"))
        });
        if let Some(stream) = exceeded_stream {
            return Err(Error::Intervention(format!(
                "Process {stream} exceeded the {} byte capture limit; the process tree was stopped and bounded output remains at {}",
                MAX_CAPTURE_BYTES_PER_STREAM,
                artifacts.display()
            )));
        }
        let action = ActionRecord {
            command: spec.clone(),
            cwd: cwd.into(),
            started_at,
            duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            exit_code: exit.code(),
            signal: exit.signal(),
            stdout: artifact(&stdout_path)?.with_kind(ArtifactKind::Stdout),
            stderr: artifact(&stderr_path)?.with_kind(ArtifactKind::Stderr),
        };
        Ok((status, action))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tokio::sync::oneshot;

    use super::{MAX_CAPTURE_BYTES_PER_STREAM, ProcessRunner};
    use crate::core::{CommandSpec, EnvironmentMode, ProcessStatus};

    #[tokio::test]
    async fn normal_capture_shutdown_never_interrupts_a_successful_process() {
        const ITERATIONS: usize = 128;

        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        fs::create_dir(&cwd).unwrap();
        let command = CommandSpec::shell(":", EnvironmentMode::Controlled);

        for iteration in 0..ITERATIONS {
            let artifacts = temp.path().join(format!("artifacts-{iteration}"));
            let (status, action) = ProcessRunner
                .run(
                    &command,
                    &cwd,
                    &artifacts,
                    Duration::from_secs(5),
                    std::future::pending(),
                )
                .await
                .unwrap();
            assert_eq!(status, ProcessStatus::Succeeded);
            assert_eq!(action.exit_code, Some(0));
            assert_eq!(action.signal, None);
        }
    }

    #[tokio::test]
    async fn cancellation_sweeps_process_group_during_bounded_fork_bursts() {
        const ITERATIONS: usize = 100;
        const DESCENDANTS_PER_ITERATION: usize = 8;

        let temp = tempfile::tempdir().unwrap();
        let mut releases_and_sentinels = Vec::with_capacity(ITERATIONS);

        for iteration in 0..ITERATIONS {
            let cwd = temp.path().join(format!("cwd-{iteration}"));
            let artifacts = temp.path().join(format!("artifacts-{iteration}"));
            let anchor_ready = temp.path().join(format!("anchor-ready-{iteration}"));
            let fork_ready = temp.path().join(format!("fork-ready-{iteration}"));
            let release = temp.path().join(format!("release-{iteration}"));
            let sentinel = temp.path().join(format!("sentinel-{iteration}"));
            fs::create_dir(&cwd).unwrap();

            let script = format!(
                "(printf ready > '{anchor}'; while [ ! -e '{release}' ]; do sleep 1; done; touch '{sentinel}') & \
                 while [ ! -e '{anchor}' ]; do :; done; \
                 printf ready > '{ready}'; \
                 i=0; while [ \"$i\" -lt {descendants} ]; do \
                     (while [ ! -e '{release}' ]; do sleep 1; done; touch '{sentinel}') & \
                     i=$((i + 1)); \
                 done; \
                 wait",
                anchor = anchor_ready.display(),
                release = release.display(),
                sentinel = sentinel.display(),
                ready = fork_ready.display(),
                descendants = DESCENDANTS_PER_ITERATION,
            );
            let command = CommandSpec::shell(&script, EnvironmentMode::Inherited);
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let cancel = async move {
                cancel_rx.await.expect("cancellation sender dropped");
            };
            let trigger = async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                while !fork_ready.exists() {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "fork burst {iteration} did not start"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                cancel_tx.send(()).expect("process runner stopped early");
            };

            let (result, ()) = tokio::join!(
                ProcessRunner.run(&command, &cwd, &artifacts, Duration::from_secs(10), cancel,),
                trigger
            );
            let (status, action) = result.unwrap();
            assert_eq!(status, ProcessStatus::Interrupted);
            assert_eq!(action.signal, Some(9));
            releases_and_sentinels.push((release, sentinel));
        }

        for (release, _) in &releases_and_sentinels {
            fs::write(release, "release").unwrap();
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        for (_, sentinel) in releases_and_sentinels {
            assert!(
                !sentinel.exists(),
                "an ordinary descendant survived its process-group cleanup: {}",
                sentinel.display()
            );
        }
    }

    #[tokio::test]
    async fn output_capture_is_bounded_and_stops_the_process_tree() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        let artifacts = temp.path().join("artifacts");
        fs::create_dir(&cwd).unwrap();
        let command = CommandSpec::shell(
            &format!(
                "yes x | head -c {}",
                MAX_CAPTURE_BYTES_PER_STREAM + 1024 * 1024
            ),
            EnvironmentMode::Inherited,
        );

        let error = ProcessRunner
            .run(
                &command,
                &cwd,
                &artifacts,
                Duration::from_secs(10),
                std::future::pending(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("capture limit"), "{error}");
        assert!(
            fs::metadata(artifacts.join("stdout.log")).unwrap().len()
                <= MAX_CAPTURE_BYTES_PER_STREAM
        );
        assert!(
            fs::metadata(artifacts.join("stderr.log")).unwrap().len()
                <= MAX_CAPTURE_BYTES_PER_STREAM
        );
    }
}
