// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    Error, Result,
    core::{Reality, StateRef},
    dojo::{GitRealityProvider, RealityProvider},
    store::{CapabilityStore, Store},
};
use chrono::Utc;
use nix::unistd::{getegid, geteuid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub const DEFAULT_CONTAINER_IMAGE: &str = "debian:bookworm-slim";

pub trait IsolatedRealityProvider: RealityProvider {
    fn capabilities(&self, manifest: &CapabilityManifest) -> RealityProviderCapabilities;
    fn create_with_capabilities(
        &self,
        state: &StateRef,
        manifest: &CapabilityManifest,
    ) -> Result<Reality>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRuntimeMetadata {
    pub runtime: String,
    pub container_id: String,
    pub container_name: String,
    pub image: String,
    pub image_digest: String,
    pub network_name: Option<String>,
    #[serde(default)]
    pub attached_fixture_containers: Vec<String>,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ContainerRuntime {
    executable: String,
}

impl ContainerRuntime {
    pub fn detect() -> Result<Self> {
        for executable in ["docker", "podman"] {
            let available = Command::new(executable)
                .args(["version", "--format", "{{.Client.Version}}"])
                .output()
                .is_ok_and(|output| output.status.success());
            if available {
                return Ok(Self {
                    executable: executable.into(),
                });
            }
        }
        Err(Error::Intervention(
            "Container Reality requires a running Docker-compatible runtime (docker or podman)"
                .into(),
        ))
    }

    pub fn named(executable: impl Into<String>) -> Result<Self> {
        let executable = executable.into();
        if executable.is_empty()
            || executable.len() > 256
            || executable.contains(['\0', '\n', '\r'])
        {
            return Err(Error::InvalidInput(
                "Container runtime executable is invalid".into(),
            ));
        }
        Ok(Self { executable })
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    fn output(&self, args: &[String]) -> Result<String> {
        let output = Command::new(&self.executable).args(args).output()?;
        if !output.status.success() {
            return Err(Error::Intervention(format!(
                "Container runtime operation failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn status(&self, args: &[String]) -> Result<()> {
        self.output(args).map(|_| ())
    }
}

pub struct ContainerRealityProvider<'a> {
    store: &'a Store,
    runtime: ContainerRuntime,
    image: String,
}

impl<'a> ContainerRealityProvider<'a> {
    pub fn new(store: &'a Store) -> Result<Self> {
        Ok(Self {
            store,
            runtime: ContainerRuntime::detect()?,
            image: DEFAULT_CONTAINER_IMAGE.into(),
        })
    }

    pub fn with_runtime(
        store: &'a Store,
        runtime: ContainerRuntime,
        image: impl Into<String>,
    ) -> Result<Self> {
        let image = image.into();
        validate_image(&image)?;
        Ok(Self {
            store,
            runtime,
            image,
        })
    }

    pub fn create_arguments(
        &self,
        reality: &Reality,
        manifest: &CapabilityManifest,
        network_name: Option<&str>,
    ) -> Result<Vec<String>> {
        manifest.validate()?;
        let root = reality.root.canonicalize()?;
        let control = self
            .store
            .home
            .join("run")
            .join("realities")
            .join(reality.id.to_string());
        let name = container_name(&reality.id.to_string());
        let user = sandbox_user();
        let network = match manifest.network.mode {
            NetworkMode::None | NetworkMode::LoopbackOnly => "none",
            NetworkMode::AllowList => network_name.ok_or_else(|| {
                Error::InvalidInput("Allow-list mode requires a dedicated internal network".into())
            })?,
            NetworkMode::Unrestricted => "bridge",
        };
        let mut arguments = vec![
            "create".into(),
            "--name".into(),
            name,
            "--label".into(),
            format!("io.openkedge.hardknock.reality={}", reality.id),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--user".into(),
            user,
            "--network".into(),
            network.into(),
            "--workdir".into(),
            "/workspace".into(),
            "--mount".into(),
            format!("type=bind,src={},dst=/workspace,rw", root.display()),
            "--mount".into(),
            format!("type=bind,src={},dst=/run/hardknock,ro", control.display()),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,nodev,noexec,size=256m".into(),
        ];
        if let Some(cpu) = &manifest.resources.cpu {
            arguments.extend(["--cpus".into(), cpu.clone()]);
        }
        if let Some(memory) = manifest.resources.memory_mb {
            arguments.extend(["--memory".into(), format!("{memory}m")]);
        }
        let pids = match (manifest.resources.pids, manifest.process.max_processes) {
            (Some(resource), Some(process)) => Some(resource.min(process)),
            (resource, process) => resource.or(process),
        };
        if let Some(pids) = pids {
            arguments.extend(["--pids-limit".into(), pids.to_string()]);
        }
        for (name, value) in &manifest.environment.values {
            arguments.extend(["--env".into(), format!("{name}={value}")]);
        }
        arguments.extend([
            self.image.clone(),
            "/bin/sh".into(),
            "-c".into(),
            "mkdir -p \"$HOME\" && while :; do sleep 3600; done".into(),
        ]);
        Ok(arguments)
    }

    fn setup_network(
        &self,
        reality: &Reality,
        manifest: &CapabilityManifest,
    ) -> Result<(Option<String>, Vec<String>)> {
        if manifest.network.mode != NetworkMode::AllowList {
            return Ok((None, vec![]));
        }
        let name = format!("hk-net-{}", short_id(&reality.id.to_string()));
        self.runtime.status(&[
            "network".into(),
            "create".into(),
            "--internal".into(),
            "--label".into(),
            format!("io.openkedge.hardknock.reality={}", reality.id),
            name.clone(),
        ])?;
        let mut attached = Vec::new();
        let unique: BTreeSet<_> = manifest
            .network
            .allow
            .iter()
            .map(|endpoint| endpoint.host.clone())
            .collect();
        for fixture in unique {
            // V0.9 allow-list mode connects only explicitly named local fixture
            // containers to an internal per-Reality network. It provides no internet
            // egress and does not mount host networking.
            if let Err(primary) = self.runtime.status(&[
                "inspect".into(),
                "--type".into(),
                "container".into(),
                fixture.clone(),
            ]) {
                self.cleanup_network(&name, &attached);
                return Err(primary);
            }
            if let Err(primary) = self.runtime.status(&[
                "network".into(),
                "connect".into(),
                "--alias".into(),
                fixture.clone(),
                name.clone(),
                fixture.clone(),
            ]) {
                self.cleanup_network(&name, &attached);
                return Err(primary);
            }
            attached.push(fixture);
        }
        Ok((Some(name), attached))
    }

    fn cleanup_network(&self, network: &str, attached: &[String]) {
        for fixture in attached {
            let _ = self.runtime.output(&[
                "network".into(),
                "disconnect".into(),
                "--force".into(),
                network.into(),
                fixture.clone(),
            ]);
        }
        let _ = self
            .runtime
            .output(&["network".into(), "rm".into(), network.into()]);
    }

    fn cleanup_runtime(&self, metadata: &ContainerRuntimeMetadata) -> Result<()> {
        let mut errors = Vec::new();
        let remove =
            self.runtime
                .output(&["rm".into(), "--force".into(), metadata.container_id.clone()]);
        if let Err(error) = remove {
            errors.push(error.to_string());
        }
        if let Some(network) = &metadata.network_name {
            for fixture in &metadata.attached_fixture_containers {
                let result = self.runtime.output(&[
                    "network".into(),
                    "disconnect".into(),
                    "--force".into(),
                    network.clone(),
                    fixture.clone(),
                ]);
                if let Err(error) = result {
                    errors.push(error.to_string());
                }
            }
            if let Err(error) =
                self.runtime
                    .output(&["network".into(), "rm".into(), network.clone()])
            {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Intervention(format!(
                "Container cleanup incomplete: {}",
                errors.join("; ")
            )))
        }
    }

    pub fn freeze(&self, reality: &mut Reality) -> Result<()> {
        let metadata: ContainerRuntimeMetadata = self.store.provider_runtime(&reality.id)?;
        self.runtime
            .status(&["pause".into(), metadata.container_id])?;
        reality.execution_boundary.frozen = true;
        self.store.update_reality(reality)?;
        let mut errors = Vec::new();
        if let Err(error) = self.store.revoke_capability_tokens(&reality.id) {
            errors.push(error.to_string());
        }
        if let Err(error) = StaticTestCredentialBroker::new(self.store)
            .and_then(|broker| broker.revoke_reality(&reality.id))
        {
            errors.push(error.to_string());
        }
        let _ = fs::remove_dir_all(
            self.store
                .home
                .join("run")
                .join("realities")
                .join(reality.id.to_string())
                .join("credentials"),
        );
        let _ = fs::remove_file(
            self.store
                .home
                .join("run")
                .join("realities")
                .join(reality.id.to_string())
                .join("capability-token.json"),
        );
        if let Some(manifest_id) = reality.execution_boundary.manifest_id.clone()
            && let Err(error) = self.store.append_capability_event(&CapabilityEvent {
                id: crate::core::CapabilityEventId::new(),
                reality_id: reality.id.clone(),
                manifest_id,
                kind: CapabilityEventKind::Frozen,
                request: None,
                reason: "Reality frozen by local user; execution and issued authority revoked"
                    .into(),
                created_at: Utc::now(),
            })
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Intervention(format!(
                "Reality is frozen but authority cleanup was incomplete: {}",
                errors.join("; ")
            )))
        }
    }

    pub fn runtime_metadata(&self, reality: &Reality) -> Result<ContainerRuntimeMetadata> {
        self.store.provider_runtime(&reality.id)
    }

    pub fn processes(&self, reality: &Reality) -> Result<String> {
        let metadata: ContainerRuntimeMetadata = self.store.provider_runtime(&reality.id)?;
        self.runtime.output(&[
            "top".into(),
            metadata.container_id,
            "-eo".into(),
            "pid,user,comm,args".into(),
        ])
    }
}

impl IsolatedRealityProvider for ContainerRealityProvider<'_> {
    fn capabilities(&self, manifest: &CapabilityManifest) -> RealityProviderCapabilities {
        RealityProviderCapabilities::container(manifest.network.mode)
    }

    fn create_with_capabilities(
        &self,
        state: &StateRef,
        manifest: &CapabilityManifest,
    ) -> Result<Reality> {
        manifest.validate()?;
        let git = GitRealityProvider::new(self.store);
        let mut reality = git.create(state)?;
        let control = self
            .store
            .home
            .join("run")
            .join("realities")
            .join(reality.id.to_string());
        if let Err(primary) = fs::create_dir_all(&control)
            .and_then(|()| fs::set_permissions(&control, fs::Permissions::from_mode(0o755)))
        {
            let _ = fs::remove_dir_all(&control);
            let _ = git.discard(&mut reality);
            return Err(primary.into());
        }
        let (network_name, attached) = match self.setup_network(&reality, manifest) {
            Ok(value) => value,
            Err(primary) => {
                let _ = fs::remove_dir_all(&control);
                let _ = git.discard(&mut reality);
                return Err(primary);
            }
        };
        let arguments = match self.create_arguments(&reality, manifest, network_name.as_deref()) {
            Ok(arguments) => arguments,
            Err(primary) => {
                if let Some(network) = &network_name {
                    self.cleanup_network(network, &attached);
                }
                let _ = fs::remove_dir_all(&control);
                let _ = git.discard(&mut reality);
                return Err(primary);
            }
        };
        let container_id = match self.runtime.output(&arguments) {
            Ok(id) => id,
            Err(primary) => {
                if let Some(network) = &network_name {
                    self.cleanup_network(network, &attached);
                }
                let _ = fs::remove_dir_all(&control);
                let _ = git.discard(&mut reality);
                return Err(primary);
            }
        };
        let digest = match self.runtime.output(&[
            "inspect".into(),
            "--format".into(),
            "{{.Image}}".into(),
            container_id.clone(),
        ]) {
            Ok(digest) => digest,
            Err(primary) => {
                let metadata = ContainerRuntimeMetadata {
                    runtime: self.runtime.executable.clone(),
                    container_id,
                    container_name: container_name(&reality.id.to_string()),
                    image: self.image.clone(),
                    image_digest: "unresolved".into(),
                    network_name,
                    attached_fixture_containers: attached,
                    created_at: Utc::now(),
                };
                let _ = self.cleanup_runtime(&metadata);
                let _ = fs::remove_dir_all(&control);
                let _ = git.discard(&mut reality);
                return Err(primary);
            }
        };
        let metadata = ContainerRuntimeMetadata {
            runtime: self.runtime.executable.clone(),
            container_id: container_id.clone(),
            container_name: container_name(&reality.id.to_string()),
            image: self.image.clone(),
            image_digest: digest.clone(),
            network_name,
            attached_fixture_containers: attached,
            created_at: Utc::now(),
        };
        if let Err(primary) = self.runtime.status(&["start".into(), container_id]) {
            let _ = self.cleanup_runtime(&metadata);
            let _ = fs::remove_dir_all(&control);
            let _ = git.discard(&mut reality);
            return Err(primary);
        }
        let persist = (|| -> Result<()> {
            reality.execution_boundary = ExecutionBoundary {
                provider: "container".into(),
                capabilities: self.capabilities(manifest),
                manifest_id: Some(manifest.id.clone()),
                manifest_hash: Some(manifest.hash()?),
                manifest_revision: manifest.revision,
                image_digest: Some(digest),
                frozen: false,
            };
            self.store.update_reality(&reality)?;
            self.store
                .insert_capability_manifest(&reality.id, manifest)?;
            self.store
                .put_provider_runtime(&reality.id, "container", &metadata)?;
            Ok(())
        })();
        if let Err(primary) = persist {
            let _ = self.cleanup_runtime(&metadata);
            let _ = fs::remove_dir_all(&control);
            let _ = git.discard(&mut reality);
            return Err(primary);
        }
        Ok(reality)
    }
}

impl RealityProvider for ContainerRealityProvider<'_> {
    fn create(&self, state: &StateRef) -> Result<Reality> {
        self.create_with_capabilities(state, &builtin_profile("coding-offline")?)
    }

    fn fork(&self, reality: &Reality) -> Result<Reality> {
        let manifest = self.store.effective_capability_manifest(&reality.id)?;
        self.create_with_capabilities(&reality.starting_state, &manifest)
    }

    fn diff(&self, reality: &Reality) -> Result<Vec<u8>> {
        GitRealityProvider::new(self.store).diff(reality)
    }

    fn discard(&self, reality: &mut Reality) -> Result<()> {
        let metadata: ContainerRuntimeMetadata = self.store.provider_runtime(&reality.id)?;
        let mut errors = Vec::new();
        let runtime_removed = match self.cleanup_runtime(&metadata) {
            Ok(()) => true,
            Err(error) => {
                errors.push(error.to_string());
                false
            }
        };
        if let Err(error) = StaticTestCredentialBroker::new(self.store)
            .and_then(|broker| broker.revoke_reality(&reality.id))
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.store.revoke_capability_tokens(&reality.id) {
            errors.push(error.to_string());
        }
        let control = self
            .store
            .home
            .join("run")
            .join("realities")
            .join(reality.id.to_string());
        if let Err(error) = fs::remove_dir_all(&control)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(error.to_string());
        }
        if runtime_removed && let Err(error) = GitRealityProvider::new(self.store).discard(reality)
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Intervention(format!(
                "Container Reality cleanup incomplete: {}",
                errors.join("; ")
            )))
        }
    }
}

fn validate_image(image: &str) -> Result<()> {
    if image.trim().is_empty()
        || image.len() > 512
        || image.contains(['\0', '\n', '\r', ' '])
        || image.starts_with('-')
    {
        return Err(Error::InvalidInput(
            "Container image reference is invalid".into(),
        ));
    }
    Ok(())
}

fn sandbox_user() -> String {
    let uid = geteuid().as_raw();
    let gid = getegid().as_raw();
    if uid == 0 {
        "65532:65532".into()
    } else {
        format!("{uid}:{gid}")
    }
}

fn container_name(id: &str) -> String {
    format!("hk-{}", short_id(id))
}

fn short_id(id: &str) -> &str {
    id.rsplit('-').next().unwrap_or(id)
}

pub fn dangerous_mount(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value == "/"
        || value == "/home"
        || value == "/var/run/docker.sock"
        || value.ends_with("/.ssh")
        || value.ends_with("/.aws")
        || value.ends_with("/.kube")
}
