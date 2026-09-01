// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, OpenOptions},
    future::Future,
    os::unix::process::ExitStatusExt,
    path::Path,
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

use crate::{
    Error, Result,
    core::{ActionRecord, ArtifactKind, CommandSpec, EnvironmentMode, ProcessStatus},
    experience::controlled_environment,
    store::artifact,
};

pub struct ProcessRunner;

struct ProcessGroup(Option<Pid>);

impl ProcessGroup {
    fn kill(&self) -> Result<()> {
        let Some(pid) = self.0 else {
            return Ok(());
        };
        match killpg(pid, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(Error::Io(std::io::Error::from_raw_os_error(error as i32))),
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
        let stdout_path = artifacts.join("stdout.log");
        let stderr_path = artifacts.join("stderr.log");
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout_path)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
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
            .stdout(stdout)
            .stderr(stderr)
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
        let mut group = ProcessGroup(Some(Pid::from_raw(pid as i32)));
        tracing::debug!(
            pid,
            "Started agent process (arguments and environment omitted)"
        );
        let (status, exit, group_already_killed) = tokio::select! {
            biased;
            _ = cancel => {
                group.kill()?;
                (ProcessStatus::Interrupted, child.wait().await?, true)
            }
            _ = tokio::time::sleep(timeout) => {
                group.kill()?;
                (ProcessStatus::TimedOut, child.wait().await?, true)
            }
            exit = child.wait() => {
                let exit = exit?;
                (if exit.success() { ProcessStatus::Succeeded } else { ProcessStatus::Failed }, exit, false)
            }
        };
        // Do not let ordinary background descendants outlive a disposable run. A
        // cancelled or timed-out group was already killed before the leader was
        // reaped; signalling that now-empty group again can spuriously return
        // EPERM on macOS and must not turn an interrupted evaluation into an error.
        if !group_already_killed {
            group.kill()?;
        }
        group.0 = None;
        drop(group);
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
