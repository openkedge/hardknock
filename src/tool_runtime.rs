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
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    process::Command,
    time::{Duration, timeout},
};

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
        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let output = match timeout(Duration::from_millis(limit), child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Ok(ToolExecutionResult {
                    status: ToolExecutionStatus::TimedOut,
                    started_at: Some(started_at),
                    completed_at: Some(Utc::now()),
                    error: Some(format!("execution exceeded {limit}ms")),
                    ..Default::default()
                });
            }
        };
        let maximum = sandbox
            .capabilities
            .resources
            .output_bytes
            .unwrap_or(8 * 1024 * 1024) as usize;
        let (stdout, stdout_truncated) = bounded_text(&output.stdout, maximum);
        let (stderr, stderr_truncated) = bounded_text(&output.stderr, maximum);
        Ok(ToolExecutionResult {
            status: if output.status.success() {
                ToolExecutionStatus::Success
            } else {
                ToolExecutionStatus::Failed
            },
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
            started_at: Some(started_at),
            completed_at: Some(Utc::now()),
            ..Default::default()
        })
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

    fn command(&self, args: &[String]) -> Result<std::process::Output> {
        Ok(std::process::Command::new(&self.runtime)
            .args(args)
            .output()?)
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
        let output = self.command(&args)?;
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
        let start = self.command(&["start".into(), container_id])?;
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
        let mut command = tokio::process::Command::new(&self.runtime);
        command.args(args).kill_on_drop(true);
        let output = match timeout(Duration::from_millis(limit), command.output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Ok(ToolExecutionResult {
                    status: ToolExecutionStatus::TimedOut,
                    started_at: Some(started_at),
                    completed_at: Some(Utc::now()),
                    error: Some(format!("execution exceeded {limit}ms")),
                    ..Default::default()
                });
            }
        };
        let maximum = sandbox
            .capabilities
            .resources
            .output_bytes
            .unwrap_or(8 * 1024 * 1024) as usize;
        let (stdout, stdout_truncated) = bounded_text(&output.stdout, maximum);
        let (stderr, stderr_truncated) = bounded_text(&output.stderr, maximum);
        Ok(ToolExecutionResult {
            status: if output.status.success() {
                ToolExecutionStatus::Success
            } else {
                ToolExecutionStatus::Failed
            },
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
            started_at: Some(started_at),
            completed_at: Some(Utc::now()),
            ..Default::default()
        })
    }

    async fn destroy(&self, sandbox: &MicroSandbox) -> Result<()> {
        let id = self
            .containers
            .lock()
            .map_err(|_| Error::Intervention("Micro-sandbox registry lock poisoned".into()))?
            .remove(&sandbox.id);
        if let Some(id) = id {
            let output = self.command(&["rm".into(), "--force".into(), id])?;
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
            self.provider
                .execute(&sandbox, &invocation)
                .await
                .unwrap_or_else(|error| ToolExecutionResult {
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
        let destroy_result = self.provider.destroy(&sandbox).await;
        sandbox.destroyed_at = Some(Utc::now());
        if let Err(error) = destroy_result
            && result.status == ToolExecutionStatus::Success
        {
            return Err(error);
        }
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

fn bounded_text(bytes: &[u8], maximum: usize) -> (String, bool) {
    let truncated = bytes.len() > maximum;
    let bytes = &bytes[..bytes.len().min(maximum)];
    (String::from_utf8_lossy(bytes).into_owned(), truncated)
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
