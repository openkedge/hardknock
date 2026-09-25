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

const PROCESS_GROUP_SWEEP_WINDOW: Duration = Duration::from_millis(100);
const PROCESS_GROUP_SWEEP_INTERVAL: Duration = Duration::from_millis(5);

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

    use super::ProcessRunner;
    use crate::core::{CommandSpec, EnvironmentMode, ProcessStatus};

    #[tokio::test]
    async fn cancellation_sweeps_process_group_during_bounded_fork_bursts() {
        const ITERATIONS: usize = 32;
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
}
