// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    Error, Result,
    core::{ActionRecord, ArtifactKind, CommandSpec, ProcessStatus, Reality, RealityStatus},
    store::{CapabilityStore, Store, artifact, token_hash},
};
use chrono::Utc;
use std::{
    fs,
    future::Future,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

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
        fs::create_dir(artifacts)?;
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
        let output = match tokio::time::timeout(timeout, command.output()).await {
            Ok(output) => output?,
            Err(_) => {
                // Killing the client does not reliably kill a docker exec process. Freeze the
                // whole disposable Reality so a timed-out descendant cannot keep running.
                let _ = Command::new(&runtime.runtime)
                    .args(["kill", &runtime.container_id])
                    .output()
                    .await;
                return Err(Error::Intervention(
                    "Container action timed out; Reality container was stopped".into(),
                ));
            }
        };
        let maximum = manifest.resources.output_bytes.unwrap_or(8 * 1024 * 1024) as usize;
        let stdout = bounded(&redactor.redact(&output.stdout), maximum);
        let stderr = bounded(&redactor.redact(&output.stderr), maximum);
        let stdout_path = artifacts.join("stdout.log");
        let stderr_path = artifacts.join("stderr.log");
        fs::write(&stdout_path, stdout)?;
        fs::write(&stderr_path, stderr)?;
        let status = if output.status.success() {
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
            exit_code: output.status.code(),
            signal: output.status.signal(),
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

fn bounded(input: &[u8], maximum: usize) -> Vec<u8> {
    if input.len() <= maximum {
        input.to_vec()
    } else {
        let mut value = input[..maximum].to_vec();
        value.extend_from_slice(b"\n[HARDKNOCK OUTPUT TRUNCATED]\n");
        value
    }
}
