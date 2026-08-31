// SPDX-License-Identifier: Apache-2.0

//! Portable, least-authority tool definitions and per-invocation capability
//! resolution.  A [`ToolDefinition`] is deliberately separate from an
//! arbitrary shell command: the definition is validated, hashed, and carried
//! into the execution attestation before any process is started.

use crate::{
    Error, Result,
    capability::*,
    core::{ArtifactRef, EffectId, MicroSandboxId, RealityId, ToolId},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub const TOOL_SCHEMA: &str = "hardknock.tool.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub id: ToolId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub invocation: ToolInvocation,
    pub capabilities: ToolCapabilityManifest,
    pub inputs: ToolInputSchema,
    pub outputs: ToolOutputSchema,
    pub integrity: ToolIntegrity,
    pub provenance: ToolProvenance,
    #[serde(default)]
    pub trust: ToolTrust,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolInvocation {
    NativeBinary {
        executable: String,
        #[serde(default)]
        args_template: Vec<String>,
    },
    Script {
        interpreter: String,
        path: String,
    },
    WasiComponent {
        artifact: ArtifactRef,
        entrypoint: Option<String>,
    },
    EffectAdapter {
        adapter: String,
        operation: String,
    },
    Custom {
        kind: String,
        payload: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFilesystemCapabilities {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolNetworkCapabilities {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub allow: Vec<NetworkEndpointPattern>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEnvironmentCapabilities {
    #[serde(default)]
    pub readable: Vec<String>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationCapability {
    pub max_ms: Option<u64>,
}

impl Default for DurationCapability {
    fn default() -> Self {
        Self {
            max_ms: Some(300_000),
        }
    }
}

impl Default for ProcessCapabilities {
    fn default() -> Self {
        Self {
            allow_exec: false,
            allowed_executables: Vec::new(),
            denied_executables: Vec::new(),
            max_processes: Some(1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapabilityManifest {
    pub filesystem: ToolFilesystemCapabilities,
    #[serde(default)]
    pub process: ProcessCapabilities,
    pub network: ToolNetworkCapabilities,
    pub environment: ToolEnvironmentCapabilities,
    #[serde(default)]
    pub credentials: Vec<CredentialCapability>,
    pub effects: EffectCapabilities,
    #[serde(default)]
    pub resources: ResourceLimits,
    #[serde(default)]
    pub duration: DurationCapability,
}

impl Default for ToolCapabilityManifest {
    fn default() -> Self {
        Self {
            filesystem: ToolFilesystemCapabilities {
                read: vec![],
                write: vec![],
            },
            process: ProcessCapabilities {
                allow_exec: false,
                allowed_executables: vec![],
                denied_executables: vec![],
                max_processes: Some(1),
            },
            network: ToolNetworkCapabilities {
                mode: NetworkMode::None,
                allow: vec![],
            },
            environment: ToolEnvironmentCapabilities {
                readable: vec![],
                values: BTreeMap::new(),
            },
            credentials: vec![],
            effects: EffectCapabilities {
                propose: false,
                prepare: false,
                commit: false,
                scope: EffectCapabilityScope {
                    kinds: vec![],
                    target_patterns: vec![],
                    operations: vec![],
                },
            },
            resources: ResourceLimits::default(),
            duration: DurationCapability::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveToolCapabilities {
    pub filesystem: ToolFilesystemCapabilities,
    #[serde(default)]
    pub process: ProcessCapabilities,
    pub network: ToolNetworkCapabilities,
    pub environment: ToolEnvironmentCapabilities,
    #[serde(default)]
    pub credentials: Vec<CredentialCapability>,
    pub effects: EffectCapabilities,
    pub resources: ResourceLimits,
    pub duration: DurationCapability,
    pub reality_manifest_hash: Option<String>,
    pub tool_manifest_hash: Option<String>,
    pub policy_grant_hash: Option<String>,
}

impl EffectiveToolCapabilities {
    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporaryCapabilityGrant {
    pub id: String,
    pub capability: ExecutionCapability,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub approved_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapabilityRequest {
    pub sandbox_id: MicroSandboxId,
    pub capability: ExecutionCapability,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDeterminism {
    Deterministic,
    ExpectedVariable,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMaturity {
    #[default]
    Registered,
    Tested,
    Hardened,
    Deprecated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySurface {
    pub writable_scopes: usize,
    pub network_endpoints: usize,
    pub credential_scopes: usize,
    pub effect_permissions: usize,
    pub exposure_duration_ms: u64,
}

impl EffectiveToolCapabilities {
    pub fn surface(&self) -> CapabilitySurface {
        let effect_permissions = self.effects.scope.kinds.len()
            * self.effects.scope.operations.len()
            * self.effects.scope.target_patterns.len();
        CapabilitySurface {
            writable_scopes: self.filesystem.write.len(),
            network_endpoints: self.network.allow.len(),
            credential_scopes: self.credentials.len(),
            effect_permissions,
            exposure_duration_ms: self.duration.max_ms.unwrap_or(0),
        }
    }
}

impl TemporaryCapabilityGrant {
    pub fn active(&self) -> bool {
        self.expires_at > Utc::now()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInputSchema {
    pub schema: Value,
}

impl Default for ToolInputSchema {
    fn default() -> Self {
        Self {
            schema: serde_json::json!({"type":"object"}),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputSchema {
    pub schema: Value,
}

impl Default for ToolOutputSchema {
    fn default() -> Self {
        Self {
            schema: serde_json::json!({}),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIntegrity {
    pub artifact_hash: Option<String>,
    pub manifest_hash: String,
    pub signature: Option<ToolSignature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSignature {
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    #[default]
    Local,
    BuiltIn,
    Imported,
    Federated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProvenance {
    pub source: ToolSource,
    pub registered_at: DateTime<Utc>,
    pub publisher: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTrust {
    #[default]
    Unsigned,
    LocalTrusted,
    SignedTrusted,
    SignedUnknown,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIdentity {
    pub id: ToolId,
    pub name: String,
    pub version: String,
}

impl From<&ToolDefinition> for ToolIdentity {
    fn from(tool: &ToolDefinition) -> Self {
        Self {
            id: tool.id.clone(),
            name: tool.name.clone(),
            version: tool.version.clone(),
        }
    }
}

impl ToolDefinition {
    pub fn identity(&self) -> ToolIdentity {
        self.into()
    }

    pub fn manifest_hash(&self) -> Result<String> {
        let payload = serde_json::json!({
            "name": self.name,
            "version": self.version,
            "description": self.description,
            "invocation": self.invocation,
            "capabilities": self.capabilities,
            "inputs": self.inputs,
            "outputs": self.outputs,
        });
        Ok(blake3::hash(&serde_json::to_vec(&payload)?)
            .to_hex()
            .to_string())
    }

    pub fn artifact_hash(&self) -> Result<Option<String>> {
        let path = match &self.invocation {
            ToolInvocation::NativeBinary { executable, .. } => find_executable(executable),
            ToolInvocation::Script { path, .. } => Some(PathBuf::from(path)),
            ToolInvocation::WasiComponent { artifact, .. } => {
                return Ok(Some(artifact.blake3.clone()));
            }
            ToolInvocation::EffectAdapter { .. } | ToolInvocation::Custom { .. } => None,
        };
        let Some(path) = path else { return Ok(None) };
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(blake3::hash(&fs::read(path)?).to_hex().to_string()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.version.trim().is_empty()
            || self.version.len() > 128
        {
            return Err(Error::InvalidInput(
                "Tool name and version must be bounded and nonempty".into(),
            ));
        }
        if self.description.len() > 16 * 1024 || self.id.to_string().len() > 128 {
            return Err(Error::InvalidInput(
                "Tool description or identifier is too large".into(),
            ));
        }
        self.capabilities.validate()?;
        validate_schema(&self.inputs.schema, "input")?;
        validate_schema(&self.outputs.schema, "output")?;
        match &self.invocation {
            ToolInvocation::NativeBinary {
                executable,
                args_template,
            } => {
                validate_executable(executable)?;
                if args_template.len() > 128
                    || args_template
                        .iter()
                        .any(|arg| arg.len() > 4096 || arg.contains('\0'))
                {
                    return Err(Error::InvalidInput(
                        "Tool argument template is invalid".into(),
                    ));
                }
            }
            ToolInvocation::Script { interpreter, path } => {
                validate_executable(interpreter)?;
                validate_tool_path(path)?;
            }
            ToolInvocation::WasiComponent {
                artifact,
                entrypoint,
            } => {
                if artifact.blake3.trim().is_empty() || artifact.bytes == 0 {
                    return Err(Error::InvalidInput(
                        "WASI artifact must include a nonempty hash and size".into(),
                    ));
                }
                if entrypoint
                    .as_deref()
                    .is_some_and(|value| value.len() > 256 || value.contains(['\0', '\n', '\r']))
                {
                    return Err(Error::InvalidInput("WASI entrypoint is invalid".into()));
                }
            }
            ToolInvocation::EffectAdapter { adapter, operation } => {
                if adapter.trim().is_empty()
                    || operation.trim().is_empty()
                    || adapter.len() > 128
                    || operation.len() > 128
                {
                    return Err(Error::InvalidInput(
                        "Effect adapter invocation is invalid".into(),
                    ));
                }
            }
            ToolInvocation::Custom { kind, payload } => {
                if kind.trim().is_empty()
                    || kind.len() > 128
                    || serde_json::to_vec(payload)?.len() > 64 * 1024
                {
                    return Err(Error::InvalidInput(
                        "Custom tool invocation is invalid".into(),
                    ));
                }
            }
        }
        if !self.integrity.manifest_hash.is_empty()
            && self.integrity.manifest_hash != self.manifest_hash()?
        {
            return Err(Error::Intervention(
                "Tool manifest hash does not match its content".into(),
            ));
        }
        if self.integrity.artifact_hash.is_some()
            && self.integrity.artifact_hash != self.artifact_hash()?
        {
            return Err(Error::Intervention(
                "Tool artifact hash does not match the executable".into(),
            ));
        }
        Ok(())
    }

    pub fn portable_toml(&self) -> Result<String> {
        let invocation = match &self.invocation {
            ToolInvocation::NativeBinary {
                executable,
                args_template,
            } => serde_json::json!({"type":"native","executable":executable,"args":args_template}),
            ToolInvocation::Script { interpreter, path } => {
                serde_json::json!({"type":"script","interpreter":interpreter,"path":path})
            }
            ToolInvocation::EffectAdapter { adapter, operation } => {
                serde_json::json!({"type":"effect_adapter","adapter":adapter,"operation":operation})
            }
            ToolInvocation::WasiComponent { .. } => {
                return Err(Error::Intervention(
                    "Portable TOML encoding for WASI artifacts requires an external artifact path"
                        .into(),
                ));
            }
            ToolInvocation::Custom { .. } => return Err(Error::Intervention(
                "Custom invocations require a runtime-specific registration and cannot be encoded as portable TOML".into(),
            )),
        };
        let operation = |pattern: &EffectOperationPattern| match &pattern.0 {
            crate::effects::EffectOperation::Custom(value) => format!("custom:{value}"),
            value => format!("{value:?}").to_ascii_lowercase(),
        };
        let value = strip_nulls(serde_json::json!({
            "schema": TOOL_SCHEMA, "name": self.name, "version": self.version,
            "description": self.description, "invocation": invocation,
            "inputs":self.inputs.schema,"outputs":self.outputs.schema,
            "capabilities": {
                "filesystem":{"read":self.capabilities.filesystem.read,"write":self.capabilities.filesystem.write},
                "process":{"allow_exec":self.capabilities.process.allow_exec,"allowed_executables":self.capabilities.process.allowed_executables,"denied_executables":self.capabilities.process.denied_executables,"max_processes":self.capabilities.process.max_processes},
                "network":{"mode":self.capabilities.network.mode,"allow":self.capabilities.network.allow},
                "effects":{"propose":self.capabilities.effects.propose,"prepare":self.capabilities.effects.prepare,"commit":false,"allowed_kinds":self.capabilities.effects.scope.kinds,"target_patterns":self.capabilities.effects.scope.target_patterns,"operations":self.capabilities.effects.scope.operations.iter().map(operation).collect::<Vec<_>>()},
                "environment":{"readable":self.capabilities.environment.readable,"values":self.capabilities.environment.values},
                "credentials":self.capabilities.credentials,
                "duration":{"max_ms":self.capabilities.duration.max_ms}
            },
            "resources":{"cpu":self.capabilities.resources.cpu,"memory_mb":self.capabilities.resources.memory_mb,"pids":self.capabilities.resources.pids,"timeout_seconds":self.capabilities.resources.timeout_ms.map(|ms| ms/1000),"output_bytes":self.capabilities.resources.output_bytes}
        }));
        toml::to_string_pretty(&value)
            .map_err(|e| Error::InvalidInput(format!("Cannot encode tool manifest: {e}")))
    }

    pub fn from_toml(input: &str) -> Result<Self> {
        let raw: PortableToolManifest = toml::from_str(input)
            .map_err(|e| Error::InvalidInput(format!("Invalid hardknock-tool.toml: {e}")))?;
        raw.into_definition()
    }

    pub fn from_toml_file(path: &Path) -> Result<Self> {
        Self::from_toml(&fs::read_to_string(path)?)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct PortableToolManifest {
    schema: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    invocation: PortableInvocation,
    #[serde(default)]
    capabilities: PortableCapabilities,
    #[serde(default)]
    inputs: Option<Value>,
    #[serde(default)]
    outputs: Option<Value>,
    #[serde(default)]
    resources: Option<PortableResources>,
}

#[derive(Clone, Debug, Deserialize)]
struct PortableInvocation {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    interpreter: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    operation: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PortableCapabilities {
    #[serde(default)]
    filesystem: PortableFilesystem,
    #[serde(default)]
    process: Option<PortableProcess>,
    #[serde(default)]
    network: PortableNetwork,
    #[serde(default)]
    environment: PortableEnvironment,
    #[serde(default)]
    credentials: Vec<CredentialCapability>,
    #[serde(default)]
    effects: PortableEffects,
    #[serde(default)]
    resources: PortableResources,
    #[serde(default)]
    duration: PortableDuration,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PortableFilesystem {
    #[serde(default, alias = "readable")]
    read: Vec<String>,
    #[serde(default, alias = "writable")]
    write: Vec<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableProcess {
    #[serde(default)]
    allow_exec: bool,
    #[serde(default)]
    allowed_executables: Vec<ExecutablePattern>,
    #[serde(default)]
    denied_executables: Vec<ExecutablePattern>,
    #[serde(default)]
    max_processes: Option<u32>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableNetwork {
    #[serde(default)]
    mode: NetworkMode,
    #[serde(default)]
    allow: Vec<NetworkEndpointPattern>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableEnvironment {
    #[serde(default)]
    readable: Vec<String>,
    #[serde(default)]
    values: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableEffects {
    #[serde(default)]
    propose: bool,
    #[serde(default)]
    prepare: bool,
    #[serde(default)]
    commit: bool,
    #[serde(default, alias = "allowed_kinds")]
    kinds: Vec<crate::effects::EffectKind>,
    #[serde(default, alias = "target_patterns")]
    targets: Vec<String>,
    #[serde(default)]
    operations: Vec<String>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableResources {
    cpu: Option<String>,
    memory_mb: Option<u64>,
    pids: Option<u32>,
    timeout_ms: Option<u64>,
    timeout_seconds: Option<u64>,
    output_bytes: Option<u64>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct PortableDuration {
    max_ms: Option<u64>,
    #[serde(alias = "timeout_seconds")]
    timeout_seconds: Option<u64>,
}

impl PortableToolManifest {
    fn into_definition(self) -> Result<ToolDefinition> {
        if self.schema != TOOL_SCHEMA {
            return Err(Error::InvalidInput(format!(
                "Unsupported tool schema {}; expected {TOOL_SCHEMA}",
                self.schema
            )));
        }
        let invocation =
            match self.invocation.kind.as_str() {
                "native" | "native_binary" => ToolInvocation::NativeBinary {
                    executable: self.invocation.executable.ok_or_else(|| {
                        Error::InvalidInput("Native tool requires executable".into())
                    })?,
                    args_template: self.invocation.args,
                },
                "script" => ToolInvocation::Script {
                    interpreter: self.invocation.interpreter.ok_or_else(|| {
                        Error::InvalidInput("Script tool requires interpreter".into())
                    })?,
                    path: self
                        .invocation
                        .path
                        .ok_or_else(|| Error::InvalidInput("Script tool requires path".into()))?,
                },
                "effect_adapter" => ToolInvocation::EffectAdapter {
                    adapter: self.invocation.adapter.ok_or_else(|| {
                        Error::InvalidInput("Effect adapter requires adapter".into())
                    })?,
                    operation: self.invocation.operation.ok_or_else(|| {
                        Error::InvalidInput("Effect adapter requires operation".into())
                    })?,
                },
                other => {
                    return Err(Error::InvalidInput(format!(
                        "Unsupported portable invocation type {other}"
                    )));
                }
            };
        let portable_resources = self
            .resources
            .unwrap_or_else(|| self.capabilities.resources.clone());
        let resources = ResourceLimits {
            cpu: portable_resources.cpu.clone(),
            memory_mb: self
                .capabilities
                .resources
                .memory_mb
                .or(portable_resources.memory_mb)
                .or(ResourceLimits::default().memory_mb),
            pids: self
                .capabilities
                .resources
                .pids
                .or(portable_resources.pids)
                .or(ResourceLimits::default().pids),
            timeout_ms: self
                .capabilities
                .resources
                .timeout_ms
                .or(self
                    .capabilities
                    .resources
                    .timeout_seconds
                    .map(|seconds| seconds.saturating_mul(1000)))
                .or(portable_resources.timeout_ms)
                .or(portable_resources
                    .timeout_seconds
                    .map(|seconds| seconds.saturating_mul(1000)))
                .or(ResourceLimits::default().timeout_ms),
            output_bytes: self
                .capabilities
                .resources
                .output_bytes
                .or(portable_resources.output_bytes)
                .or(ResourceLimits::default().output_bytes),
        };
        let duration = DurationCapability {
            max_ms: self
                .capabilities
                .duration
                .max_ms
                .or(self
                    .capabilities
                    .duration
                    .timeout_seconds
                    .map(|seconds| seconds.saturating_mul(1000)))
                .or(resources.timeout_ms),
        };
        let process = self
            .capabilities
            .process
            .map(|p| ProcessCapabilities {
                allow_exec: p.allow_exec,
                allowed_executables: p.allowed_executables,
                denied_executables: p.denied_executables,
                max_processes: p.max_processes,
            })
            .unwrap_or_default();
        let capabilities = ToolCapabilityManifest {
            filesystem: ToolFilesystemCapabilities {
                read: self.capabilities.filesystem.read,
                write: self.capabilities.filesystem.write,
            },
            process,
            network: ToolNetworkCapabilities {
                mode: self.capabilities.network.mode,
                allow: self.capabilities.network.allow,
            },
            environment: ToolEnvironmentCapabilities {
                readable: self.capabilities.environment.readable,
                values: self.capabilities.environment.values,
            },
            credentials: self.capabilities.credentials,
            effects: EffectCapabilities {
                propose: self.capabilities.effects.propose,
                prepare: self.capabilities.effects.prepare,
                commit: self.capabilities.effects.commit,
                scope: EffectCapabilityScope {
                    kinds: self.capabilities.effects.kinds,
                    target_patterns: self.capabilities.effects.targets,
                    operations: self
                        .capabilities
                        .effects
                        .operations
                        .into_iter()
                        .map(parse_effect_operation)
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .map(EffectOperationPattern)
                        .collect(),
                },
            },
            resources,
            duration,
        };
        let mut definition = ToolDefinition {
            id: ToolId::new(),
            name: self.name,
            version: self.version,
            description: self.description,
            invocation,
            capabilities,
            inputs: ToolInputSchema {
                schema: self
                    .inputs
                    .unwrap_or_else(|| serde_json::json!({"type":"object"})),
            },
            outputs: ToolOutputSchema {
                schema: self.outputs.unwrap_or_else(|| serde_json::json!({})),
            },
            integrity: ToolIntegrity {
                artifact_hash: None,
                manifest_hash: String::new(),
                signature: None,
            },
            provenance: ToolProvenance {
                source: ToolSource::Local,
                registered_at: Utc::now(),
                publisher: None,
            },
            trust: ToolTrust::Unsigned,
            disabled: false,
        };
        definition.integrity.artifact_hash = definition.artifact_hash()?;
        definition.integrity.manifest_hash = definition.manifest_hash()?;
        definition.validate()?;
        Ok(definition)
    }
}

impl ToolCapabilityManifest {
    pub fn validate(&self) -> Result<()> {
        for pattern in self.filesystem.read.iter().chain(&self.filesystem.write) {
            validate_tool_path(pattern)?;
        }
        for endpoint in &self.network.allow {
            endpoint.validate()?;
        }
        for executable in self
            .process
            .allowed_executables
            .iter()
            .chain(&self.process.denied_executables)
        {
            if executable.0.trim().is_empty()
                || executable.0.len() > 512
                || executable.0.contains(['\0', '\n', '\r'])
            {
                return Err(Error::InvalidInput(
                    "Tool executable capability patterns must be bounded".into(),
                ));
            }
        }
        if self.network.mode != NetworkMode::AllowList && !self.network.allow.is_empty() {
            return Err(Error::InvalidInput(
                "Tool network endpoints require allow-list mode".into(),
            ));
        }
        for name in self
            .environment
            .readable
            .iter()
            .chain(self.environment.values.keys())
        {
            if !valid_env_name(name) {
                return Err(Error::InvalidInput(format!(
                    "Invalid tool environment name {name}"
                )));
            }
        }
        for value in self.environment.values.values() {
            if value.len() > 4096 || value.contains(['\0', '\n', '\r']) {
                return Err(Error::InvalidInput(
                    "Tool environment values must be bounded and single-line".into(),
                ));
            }
        }
        if self.process.max_processes == Some(0)
            || self.resources.memory_mb == Some(0)
            || self.resources.pids == Some(0)
            || self.resources.output_bytes == Some(0)
            || self.duration.max_ms == Some(0)
        {
            return Err(Error::InvalidInput(
                "Tool resource limits must be positive".into(),
            ));
        }
        for credential in &self.credentials {
            if credential.provider.is_empty()
                || credential.name.is_empty()
                || credential.scope.resource.trim().is_empty()
                || credential.permissions.is_empty()
                || credential.permissions.iter().any(|permission| {
                    permission.trim().is_empty()
                        || permission.len() > 128
                        || permission.contains(['\0', '\n', '\r'])
                })
            {
                return Err(Error::InvalidInput(
                    "Tool credentials require explicit provider, name, and permissions".into(),
                ));
            }
        }
        for target in &self.effects.scope.target_patterns {
            if target.trim().is_empty()
                || target.len() > 2048
                || target.contains(['\0', '\n', '\r'])
                || target.strip_suffix('*').unwrap_or(target).contains('*')
            {
                return Err(Error::InvalidInput(
                    "Tool Effect targets allow only an optional trailing wildcard".into(),
                ));
            }
        }
        let enabled = self.effects.propose || self.effects.prepare || self.effects.commit;
        if enabled
            && (self.effects.scope.kinds.is_empty()
                || self.effects.scope.target_patterns.is_empty()
                || self.effects.scope.operations.is_empty())
        {
            return Err(Error::InvalidInput(
                "Enabled tool effects require explicit kinds, targets, and operations".into(),
            ));
        }
        if self.effects.commit {
            return Err(Error::Intervention(
                "Tool definitions cannot self-grant Effect commit authority".into(),
            ));
        }
        if serde_json::to_vec(self)?.len() > MAX_MANIFEST_BYTES {
            return Err(Error::InvalidInput(
                "Tool capability manifest is too large".into(),
            ));
        }
        Ok(())
    }
}

pub trait CapabilityIntersectionPolicy: Send + Sync {
    fn resolve(
        &self,
        reality: &CapabilityManifest,
        tool: &ToolCapabilityManifest,
        grants: &[TemporaryCapabilityGrant],
    ) -> Result<EffectiveToolCapabilities>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DenyByDefaultToolIntersectionPolicy;

impl CapabilityIntersectionPolicy for DenyByDefaultToolIntersectionPolicy {
    fn resolve(
        &self,
        reality: &CapabilityManifest,
        tool: &ToolCapabilityManifest,
        grants: &[TemporaryCapabilityGrant],
    ) -> Result<EffectiveToolCapabilities> {
        reality.validate()?;
        tool.validate()?;
        let grants: Vec<_> = grants.iter().filter(|grant| grant.active()).collect();
        let grant_hash = if grants.is_empty() {
            None
        } else {
            Some(
                blake3::hash(&serde_json::to_vec(&grants)?)
                    .to_hex()
                    .to_string(),
            )
        };
        let effective = EffectiveToolCapabilities {
            filesystem: ToolFilesystemCapabilities {
                read: intersect_paths(&reality.filesystem.readable, &tool.filesystem.read),
                write: intersect_paths(&reality.filesystem.writable, &tool.filesystem.write),
            },
            process: intersect_process(&reality.process, &tool.process, &grants),
            network: intersect_network(&reality.network, &tool.network),
            environment: intersect_environment(&reality.environment, &tool.environment),
            credentials: intersect_credentials(&reality.credentials, &tool.credentials),
            effects: intersect_effects(&reality.effects, &tool.effects),
            resources: intersect_resources(&reality.resources, &tool.resources),
            duration: DurationCapability {
                max_ms: min_opt(reality.resources.timeout_ms, tool.duration.max_ms),
            },
            reality_manifest_hash: Some(reality.hash()?),
            tool_manifest_hash: None,
            policy_grant_hash: grant_hash,
        };
        let effective = apply_grant_restrictions(effective, &grants);
        Ok(effective)
    }
}

pub fn resolve_effective_capabilities(
    reality: &CapabilityManifest,
    tool: &ToolCapabilityManifest,
    grants: &[TemporaryCapabilityGrant],
) -> Result<EffectiveToolCapabilities> {
    DenyByDefaultToolIntersectionPolicy.resolve(reality, tool, grants)
}

fn intersect_paths(reality: &[FilesystemScope], requested: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for pattern in requested {
        let Some((tool_root, recursive)) = tool_path_root(pattern) else {
            continue;
        };
        for scope in reality {
            let reality_root = Path::new(&scope.root);
            let requested_root = Path::new(&tool_root);
            if requested_root == reality_root || requested_root.starts_with(reality_root) {
                out.insert(if recursive && scope.recursive {
                    format!("{tool_root}/**")
                } else {
                    tool_root.clone()
                });
            } else if reality_root.starts_with(requested_root) {
                out.insert(if scope.recursive {
                    format!("{}/**", scope.root)
                } else {
                    scope.root.clone()
                });
            }
        }
    }
    out.into_iter().collect()
}

fn tool_path_root(value: &str) -> Option<(String, bool)> {
    let mapped = value
        .replace("$WORKSPACE", "/workspace")
        .replace("$TMP", "/tmp");
    if !mapped.starts_with('/')
        || mapped.contains(['\0', '\n', '\r'])
        || mapped.split('/').any(|part| part == ".." || part == ".")
    {
        return None;
    }
    let recursive = mapped.ends_with("/**") || mapped.ends_with("/*");
    let root = if recursive {
        mapped
            .trim_end_matches("/**")
            .trim_end_matches("/*")
            .to_owned()
    } else {
        mapped
    };
    Some((if root.is_empty() { "/".into() } else { root }, recursive))
}

fn intersect_process(
    reality: &ProcessCapabilities,
    tool: &ProcessCapabilities,
    grants: &[&TemporaryCapabilityGrant],
) -> ProcessCapabilities {
    let both_constrained =
        !reality.allowed_executables.is_empty() && !tool.allowed_executables.is_empty();
    let mut allowed = if reality.allowed_executables.is_empty() {
        tool.allowed_executables.clone()
    } else if tool.allowed_executables.is_empty() {
        reality.allowed_executables.clone()
    } else {
        reality
            .allowed_executables
            .iter()
            .filter(|left| {
                tool.allowed_executables
                    .iter()
                    .any(|right| pattern_overlap(&left.0, &right.0))
            })
            .cloned()
            .collect()
    };
    allowed.retain(|pattern| {
        !reality
            .denied_executables
            .iter()
            .any(|denied| pattern_overlap(&denied.0, &pattern.0))
            && !tool
                .denied_executables
                .iter()
                .any(|denied| pattern_overlap(&denied.0, &pattern.0))
    });
    let mut result = ProcessCapabilities {
        // An empty allow-list normally means unrestricted. When two explicit
        // lists do not overlap, therefore, execution must be disabled rather
        // than represented by an accidentally unrestricted empty list.
        allow_exec: reality.allow_exec
            && tool.allow_exec
            && (!both_constrained || !allowed.is_empty()),
        allowed_executables: allowed,
        denied_executables: reality
            .denied_executables
            .iter()
            .chain(&tool.denied_executables)
            .cloned()
            .collect(),
        max_processes: min_opt(reality.max_processes, tool.max_processes),
    };
    if !grants.is_empty()
        && !grants
            .iter()
            .any(|grant| matches!(&grant.capability, ExecutionCapability::ProcessExecute(_)))
    {
        result.allow_exec = false;
    }
    result
}

fn intersect_network(
    reality: &NetworkCapabilities,
    tool: &ToolNetworkCapabilities,
) -> ToolNetworkCapabilities {
    let mode = match (reality.mode, tool.mode) {
        (NetworkMode::None, _) | (_, NetworkMode::None) => NetworkMode::None,
        (NetworkMode::LoopbackOnly, NetworkMode::LoopbackOnly) => NetworkMode::LoopbackOnly,
        (NetworkMode::LoopbackOnly, NetworkMode::Unrestricted)
        | (NetworkMode::Unrestricted, NetworkMode::LoopbackOnly) => NetworkMode::LoopbackOnly,
        (NetworkMode::AllowList, NetworkMode::Unrestricted) => NetworkMode::AllowList,
        (NetworkMode::Unrestricted, NetworkMode::AllowList) => NetworkMode::AllowList,
        (NetworkMode::AllowList, NetworkMode::AllowList) => NetworkMode::AllowList,
        (NetworkMode::Unrestricted, NetworkMode::Unrestricted) => NetworkMode::Unrestricted,
        _ => NetworkMode::None,
    };
    let allow = match (reality.mode, tool.mode) {
        (NetworkMode::AllowList, NetworkMode::AllowList) => reality
            .allow
            .iter()
            .filter(|endpoint| tool.allow.contains(endpoint))
            .cloned()
            .collect(),
        (NetworkMode::AllowList, NetworkMode::Unrestricted) => reality.allow.clone(),
        (NetworkMode::Unrestricted, NetworkMode::AllowList) => tool.allow.clone(),
        _ => vec![],
    };
    if mode == NetworkMode::AllowList && allow.is_empty() {
        return ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        };
    }
    ToolNetworkCapabilities { mode, allow }
}

fn intersect_environment(
    reality: &EnvironmentCapabilities,
    tool: &ToolEnvironmentCapabilities,
) -> ToolEnvironmentCapabilities {
    let readable = tool
        .readable
        .iter()
        .filter(|name| reality.readable.contains(name))
        .cloned()
        .collect();
    let values = tool
        .values
        .iter()
        .filter(|(name, value)| {
            reality
                .values
                .get(*name)
                .is_some_and(|actual| actual == *value)
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    ToolEnvironmentCapabilities { readable, values }
}

fn intersect_credentials(
    reality: &[CredentialCapability],
    tool: &[CredentialCapability],
) -> Vec<CredentialCapability> {
    tool.iter()
        .filter_map(|requested| {
            reality.iter().find_map(|available| {
                if available.provider != requested.provider
                    || available.name != requested.name
                    || !pattern_overlap(&available.scope.resource, &requested.scope.resource)
                {
                    return None;
                }
                let permissions = requested
                    .permissions
                    .iter()
                    .filter(|permission| available.permissions.contains(permission))
                    .cloned()
                    .collect::<Vec<_>>();
                if permissions.is_empty() {
                    return None;
                }
                Some(CredentialCapability {
                    provider: requested.provider.clone(),
                    name: requested.name.clone(),
                    scope: CredentialScope {
                        resource: narrow_pattern(
                            &available.scope.resource,
                            &requested.scope.resource,
                        ),
                    },
                    permissions,
                    expires_at: min_time(available.expires_at, requested.expires_at),
                })
            })
        })
        .collect()
}

fn intersect_effects(
    reality: &EffectCapabilities,
    tool: &EffectCapabilities,
) -> EffectCapabilities {
    let scope = EffectCapabilityScope {
        kinds: reality
            .scope
            .kinds
            .iter()
            .filter(|kind| tool.scope.kinds.contains(kind))
            .cloned()
            .collect(),
        target_patterns: intersect_patterns(
            &reality.scope.target_patterns,
            &tool.scope.target_patterns,
        ),
        operations: reality
            .scope
            .operations
            .iter()
            .filter(|op| tool.scope.operations.contains(op))
            .cloned()
            .collect(),
    };
    let scope_is_usable = !scope.kinds.is_empty()
        && !scope.target_patterns.is_empty()
        && !scope.operations.is_empty();
    EffectCapabilities {
        propose: scope_is_usable && reality.propose && tool.propose,
        prepare: scope_is_usable && reality.prepare && tool.prepare,
        commit: false,
        scope,
    }
}

fn intersect_patterns(left: &[String], right: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for a in left {
        for b in right {
            if pattern_overlap(a, b) {
                out.insert(narrow_pattern(a, b));
            }
        }
    }
    out.into_iter().collect()
}
fn pattern_overlap(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('*');
    let b = b.trim_end_matches('*');
    a.starts_with(b) || b.starts_with(a)
}
fn narrow_pattern(a: &str, b: &str) -> String {
    if a.trim_end_matches('*').len() >= b.trim_end_matches('*').len() {
        a.into()
    } else {
        b.into()
    }
}
fn intersect_resources(reality: &ResourceLimits, tool: &ResourceLimits) -> ResourceLimits {
    ResourceLimits {
        cpu: reality
            .cpu
            .clone()
            .zip(tool.cpu.clone())
            .map(|(a, b)| if a <= b { a } else { b })
            .or_else(|| reality.cpu.clone())
            .or_else(|| tool.cpu.clone()),
        memory_mb: min_opt(reality.memory_mb, tool.memory_mb),
        pids: min_opt(reality.pids, tool.pids),
        timeout_ms: min_opt(reality.timeout_ms, tool.timeout_ms),
        output_bytes: min_opt(reality.output_bytes, tool.output_bytes),
    }
}
fn min_opt<T: Ord + Copy>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) | (None, a) => a,
    }
}
fn min_time(left: Option<DateTime<Utc>>, right: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) | (None, a) => a,
    }
}
fn apply_grant_restrictions(
    mut effective: EffectiveToolCapabilities,
    grants: &[&TemporaryCapabilityGrant],
) -> EffectiveToolCapabilities {
    if grants.is_empty() {
        return effective;
    }
    let permits = |capability: &ExecutionCapability| {
        grants
            .iter()
            .any(|grant| capability_matches(capability, &grant.capability))
    };
    if grants
        .iter()
        .any(|grant| matches!(grant.capability, ExecutionCapability::FilesystemRead(_)))
    {
        effective.filesystem.read.retain(|path| {
            permits(&ExecutionCapability::FilesystemRead(FilesystemScope {
                root: path.trim_end_matches("/**").to_owned(),
                recursive: path.ends_with("/**"),
            }))
        });
    }
    if grants
        .iter()
        .any(|grant| matches!(grant.capability, ExecutionCapability::FilesystemWrite(_)))
    {
        effective.filesystem.write.retain(|path| {
            permits(&ExecutionCapability::FilesystemWrite(FilesystemScope {
                root: path.trim_end_matches("/**").to_owned(),
                recursive: path.ends_with("/**"),
            }))
        });
    }
    if grants
        .iter()
        .any(|grant| matches!(&grant.capability, ExecutionCapability::NetworkConnect(_)))
    {
        if effective.network.mode == NetworkMode::AllowList {
            effective.network.allow.retain(|endpoint| permits(&ExecutionCapability::NetworkConnect(endpoint.clone())));
            if effective.network.allow.is_empty() { effective.network.mode = NetworkMode::None; }
        } else if effective.network.mode != NetworkMode::None && !grants.iter().any(|grant| matches!(&grant.capability, ExecutionCapability::NetworkConnect(endpoint) if endpoint.host == "localhost" || endpoint.host == "127.0.0.1" || endpoint.host == "::1")) {
            effective.network = ToolNetworkCapabilities { mode: NetworkMode::None, allow: vec![] };
        }
    }
    effective
}
fn capability_matches(requested: &ExecutionCapability, granted: &ExecutionCapability) -> bool {
    match (requested, granted) {
        (ExecutionCapability::FilesystemRead(a), ExecutionCapability::FilesystemRead(b))
        | (ExecutionCapability::FilesystemWrite(a), ExecutionCapability::FilesystemWrite(b)) => {
            Path::new(&a.root).starts_with(&b.root) || Path::new(&b.root).starts_with(&a.root)
        }
        (ExecutionCapability::ProcessExecute(a), ExecutionCapability::ProcessExecute(b)) => {
            pattern_overlap(&a.0, &b.0)
        }
        (ExecutionCapability::NetworkConnect(a), ExecutionCapability::NetworkConnect(b)) => a == b,
        (ExecutionCapability::EnvironmentRead(a), ExecutionCapability::EnvironmentRead(b)) => {
            a == b
        }
        (ExecutionCapability::CredentialUse(a), ExecutionCapability::CredentialUse(b)) => {
            a.provider == b.provider && a.name == b.name
        }
        (ExecutionCapability::EffectPropose(a), ExecutionCapability::EffectPropose(b))
        | (ExecutionCapability::EffectPrepare(a), ExecutionCapability::EffectPrepare(b))
        | (ExecutionCapability::EffectCommit(a), ExecutionCapability::EffectCommit(b)) => a == b,
        _ => false,
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<ToolId, ToolDefinition>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, mut tool: ToolDefinition) -> Result<ToolId> {
        tool.validate()?;
        if matches!(
            tool.provenance.source,
            ToolSource::Imported | ToolSource::Federated
        ) {
            tool.disabled = true;
            if tool.trust == ToolTrust::Unsigned {
                tool.trust = if tool.integrity.signature.is_some() {
                    ToolTrust::SignedUnknown
                } else {
                    ToolTrust::Unsigned
                };
            }
        }
        if tool.integrity.manifest_hash.is_empty() {
            tool.integrity.manifest_hash = tool.manifest_hash()?;
        }
        if self
            .tools
            .values()
            .any(|existing| existing.name == tool.name && existing.version == tool.version)
        {
            return Err(Error::Intervention(format!(
                "Tool {}@{} is already registered",
                tool.name, tool.version
            )));
        }
        let id = tool.id.clone();
        self.tools.insert(id.clone(), tool);
        Ok(id)
    }
    pub fn get(&self, name_or_id: &str) -> Result<&ToolDefinition> {
        self.tools
            .values()
            .find(|tool| tool.id.to_string() == name_or_id || tool.name == name_or_id)
            .ok_or_else(|| Error::NotFound(format!("Tool {name_or_id} not found")))
    }
    pub fn get_mut(&mut self, name_or_id: &str) -> Result<&mut ToolDefinition> {
        self.tools
            .values_mut()
            .find(|tool| tool.id.to_string() == name_or_id || tool.name == name_or_id)
            .ok_or_else(|| Error::NotFound(format!("Tool {name_or_id} not found")))
    }
    pub fn list(&self) -> Vec<ToolDefinition> {
        self.tools.values().cloned().collect()
    }
    pub fn verify(&self, name_or_id: &str) -> Result<ToolVerification> {
        let tool = self.get(name_or_id)?;
        let manifest_hash = tool.manifest_hash()?;
        let artifact_hash = tool.artifact_hash()?;
        Ok(ToolVerification {
            tool: tool.identity(),
            manifest_matches: tool.integrity.manifest_hash == manifest_hash,
            artifact_matches: tool
                .integrity
                .artifact_hash
                .as_ref()
                .is_none_or(|expected| Some(expected) == artifact_hash.as_ref()),
            trust: tool.trust,
            disabled: tool.disabled,
        })
    }
    pub fn disable(&mut self, name_or_id: &str, reason: impl Into<String>) -> Result<()> {
        let tool = self.get_mut(name_or_id)?;
        tool.disabled = true;
        tool.trust = ToolTrust::Blocked;
        let _ = reason.into();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVerification {
    pub tool: ToolIdentity,
    pub manifest_matches: bool,
    pub artifact_matches: bool,
    pub trust: ToolTrust,
    pub disabled: bool,
}

fn validate_schema(schema: &Value, label: &str) -> Result<()> {
    if !schema.is_object() {
        return Err(Error::InvalidInput(format!(
            "Tool {label} schema must be a JSON object"
        )));
    }
    if serde_json::to_vec(schema)?.len() > 64 * 1024 {
        return Err(Error::InvalidInput(format!(
            "Tool {label} schema is too large"
        )));
    }
    if let Some(kind) = schema.get("type") {
        let valid = kind.as_str().is_some_and(|kind| {
            matches!(
                kind,
                "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
            )
        });
        if !valid {
            return Err(Error::InvalidInput(format!(
                "Tool {label} schema has an unsupported type"
            )));
        }
    }
    if schema
        .get("properties")
        .is_some_and(|value| !value.is_object())
        || schema.get("required").is_some_and(|value| {
            !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        })
    {
        return Err(Error::InvalidInput(format!(
            "Tool {label} schema has malformed properties or required fields"
        )));
    }
    Ok(())
}

fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            object.retain(|_, value| !value.is_null());
            for value in object.values_mut() {
                *value = strip_nulls(value.take());
            }
            Value::Object(object)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

fn parse_effect_operation(value: String) -> Result<crate::effects::EffectOperation> {
    use crate::effects::EffectOperation;
    Ok(match value.as_str() {
        "read" => EffectOperation::Read,
        "create" => EffectOperation::Create,
        "update" => EffectOperation::Update,
        "delete" => EffectOperation::Delete,
        "post" => EffectOperation::Post,
        "dispatch" => EffectOperation::Dispatch,
        "promote" => EffectOperation::Promote,
        value
            if value
                .strip_prefix("custom:")
                .is_some_and(|name| !name.is_empty()) =>
        {
            EffectOperation::Custom(value[7..].into())
        }
        other => {
            return Err(Error::InvalidInput(format!(
                "Unknown effect operation {other}"
            )));
        }
    })
}
fn validate_executable(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 512
        || value.contains(['\0', '\n', '\r'])
        || value.contains('/') && value.split('/').any(|part| part == "..")
    {
        return Err(Error::InvalidInput(format!(
            "Invalid tool executable {value}"
        )));
    }
    Ok(())
}
fn validate_tool_path(value: &str) -> Result<()> {
    let mapped = value
        .replace("$WORKSPACE", "/workspace")
        .replace("$TMP", "/tmp");
    if !mapped.starts_with('/')
        || mapped.contains(['\0', '\n', '\r'])
        || mapped.split('/').any(|part| part == ".." || part == ".")
    {
        return Err(Error::InvalidInput(format!(
            "Tool path must be an absolute controlled path: {value}"
        )));
    }
    Ok(())
}
fn valid_env_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}
fn find_executable(executable: &str) -> Option<PathBuf> {
    if executable.contains('/') {
        return Some(PathBuf::from(executable));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(executable))
            .find(|path| path.is_file())
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicroSandbox {
    pub id: MicroSandboxId,
    pub reality_id: RealityId,
    pub tool_id: ToolId,
    pub capabilities: EffectiveToolCapabilities,
    pub runtime: MicroSandboxRuntime,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub destroyed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicroSandboxRuntime {
    Container,
    Wasi,
    EffectBoundary,
    Host,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRuntimePolicyKind {
    Container,
    Wasi,
    Host,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRuntimePolicy {
    pub default_runtime: ToolRuntimePolicyKind,
    pub allow_host_fallback: bool,
}

impl Default for ToolRuntimePolicy {
    fn default() -> Self {
        Self {
            default_runtime: ToolRuntimePolicyKind::Container,
            allow_host_fallback: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolRegistration {
    pub name: String,
    pub version: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

pub trait AgentToolAdapter: Send + Sync {
    fn expose(&self, tools: &[ToolDefinition]) -> Result<Vec<AgentToolRegistration>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeToolAdapter;

impl AgentToolAdapter for NativeToolAdapter {
    fn expose(&self, tools: &[ToolDefinition]) -> Result<Vec<AgentToolRegistration>> {
        tools
            .iter()
            .map(|tool| {
                Ok(AgentToolRegistration {
                    name: tool.name.clone(),
                    version: tool.version.clone(),
                    input_schema: tool.inputs.schema.clone(),
                    output_schema: tool.outputs.schema.clone(),
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxGuarantees {
    pub filesystem: IsolationLevel,
    pub process: IsolationLevel,
    pub network: IsolationLevel,
    pub credentials: IsolationLevel,
    pub ephemeral: bool,
    #[serde(default)]
    pub known_limitations: Vec<String>,
}

impl SandboxGuarantees {
    pub fn observed() -> Self {
        Self {
            filesystem: IsolationLevel::None,
            process: IsolationLevel::None,
            network: IsolationLevel::None,
            credentials: IsolationLevel::None,
            ephemeral: false,
            known_limitations: vec![
                "Host execution is explicitly trusted development mode; no isolation is claimed"
                    .into(),
            ],
        }
    }
    pub fn container() -> Self {
        Self {
            filesystem: IsolationLevel::Container,
            process: IsolationLevel::Container,
            network: IsolationLevel::Container,
            credentials: IsolationLevel::Container,
            ephemeral: true,
            known_limitations: vec![
                "Shares the host kernel; not a hardened multi-tenant boundary".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRequirements {
    pub filesystem: IsolationLevel,
    pub process: IsolationLevel,
    pub network: IsolationLevel,
    pub portability_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedToolInvocation {
    pub tool: ToolIdentity,
    pub executable: Option<String>,
    pub args: Vec<String>,
    pub input: Value,
    #[serde(default)]
    pub effect_adapter: Option<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Success,
    Failed,
    Denied,
    TimedOut,
    ResourceExceeded,
    InvalidInput,
    InvalidOutput,
    RuntimeFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub status: ToolExecutionStatus,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub outputs: Vec<ArtifactRef>,
    #[serde(default)]
    pub effects: Vec<EffectId>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: Option<String>,
}

pub type ArtifactHash = String;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationAssurance {
    #[default]
    Observed,
    IsolatedObserved,
    RuntimeVerified,
    HardwareBacked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAttestationInfo {
    pub provider: String,
    pub version: String,
    pub image_or_runtime_digest: Option<String>,
    pub isolation: SandboxGuarantees,
    #[serde(default)]
    pub assurance: AttestationAssurance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAttestation {
    pub id: crate::core::ExecutionAttestationId,
    pub tool: ToolIdentity,
    pub reality_id: RealityId,
    pub sandbox_id: MicroSandboxId,
    pub invocation_hash: String,
    pub tool_artifact_hash: Option<String>,
    pub tool_manifest_hash: String,
    pub reality_manifest_hash: String,
    pub effective_capability_hash: String,
    pub input_hashes: Vec<ArtifactHash>,
    pub output_hashes: Vec<ArtifactHash>,
    pub effect_refs: Vec<EffectId>,
    pub result: ToolExecutionStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub runtime: RuntimeAttestationInfo,
    #[serde(default)]
    pub input_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub output_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub assurance: AttestationAssurance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationVerification {
    pub attestation_id: crate::core::ExecutionAttestationId,
    pub valid: bool,
    pub attestation_hash_matches: bool,
    pub manifest_matches: bool,
    pub artifacts_match: bool,
    pub provenance_present: bool,
    pub reasons: Vec<String>,
}

impl ExecutionAttestation {
    pub fn attestation_hash(&self) -> Result<String> {
        let artifact_reference = |artifact: &ArtifactRef| {
            serde_json::json!({
                "path":artifact.path,
                "blake3":artifact.blake3,
                "bytes":artifact.bytes,
                "kind":artifact.kind
            })
        };
        let payload = serde_json::json!({
            "tool": &self.tool, "reality_id": &self.reality_id, "sandbox_id": &self.sandbox_id,
            "invocation_hash": &self.invocation_hash, "tool_artifact_hash": &self.tool_artifact_hash,
            "tool_manifest_hash": &self.tool_manifest_hash, "reality_manifest_hash": &self.reality_manifest_hash,
            "effective_capability_hash": &self.effective_capability_hash, "input_hashes": &self.input_hashes,
            "output_hashes": &self.output_hashes, "effect_refs": &self.effect_refs, "result": &self.result,
            "started_at": &self.started_at, "completed_at": &self.completed_at, "runtime": &self.runtime,
            "input_artifacts":self.input_artifacts.iter().map(artifact_reference).collect::<Vec<_>>(),
            "output_artifacts":self.output_artifacts.iter().map(artifact_reference).collect::<Vec<_>>(),
            "assurance": &self.assurance,
        });
        Ok(blake3::hash(&serde_json::to_vec(&payload)?)
            .to_hex()
            .to_string())
    }

    pub fn verify(
        &self,
        tool: Option<&ToolDefinition>,
        reality_manifest_hash: Option<&str>,
    ) -> Result<AttestationVerification> {
        let mut reasons = Vec::new();
        let computed_hash = self.attestation_hash()?;
        let attestation_hash_matches = self
            .recorded_hash
            .as_ref()
            .is_none_or(|recorded| recorded == &computed_hash);
        if !attestation_hash_matches {
            reasons.push("attestation record hash differs from its canonical contents".into());
        }
        let manifest_matches = tool.is_none_or(|definition| {
            definition
                .manifest_hash()
                .is_ok_and(|hash| hash == self.tool_manifest_hash)
        });
        if !manifest_matches {
            reasons.push("tool manifest hash differs from the recorded hash".into());
        }
        let reality_matches =
            reality_manifest_hash.is_none_or(|hash| hash == self.reality_manifest_hash);
        if !reality_matches {
            reasons.push("Reality manifest hash differs from the recorded hash".into());
        }
        let tool_artifact_matches = match (tool, self.tool_artifact_hash.as_ref()) {
            (_, None) => true,
            (Some(definition), Some(expected)) => definition
                .artifact_hash()
                .is_ok_and(|observed| observed.as_ref() == Some(expected)),
            (None, Some(_)) => true,
        };
        let mut artifacts_match = tool_artifact_matches;
        if !tool_artifact_matches {
            reasons.push("tool artifact hash differs from the recorded hash".into());
        }
        for artifact in self.input_artifacts.iter().chain(&self.output_artifacts) {
            let observed = artifact
                .path
                .is_file()
                .then(|| fs::read(&artifact.path).ok())
                .flatten()
                .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
            if observed.as_deref() != Some(artifact.blake3.as_str()) {
                artifacts_match = false;
                reasons.push(format!(
                    "artifact hash mismatch: {}",
                    artifact.path.display()
                ));
            }
        }
        let provenance_present = !self.tool.name.is_empty()
            && !self.tool.version.is_empty()
            && !self.tool_manifest_hash.is_empty()
            && !self.reality_manifest_hash.is_empty();
        if !provenance_present {
            reasons.push("required tool or Reality provenance is missing".into());
        }
        let valid = attestation_hash_matches
            && manifest_matches
            && reality_matches
            && artifacts_match
            && provenance_present;
        Ok(AttestationVerification {
            attestation_id: self.id.clone(),
            valid,
            attestation_hash_matches,
            manifest_matches: manifest_matches && reality_matches,
            artifacts_match,
            provenance_present,
            reasons,
        })
    }

    pub fn compare_replay(&self, replay: &ExecutionAttestation) -> Result<ReplayOutcome> {
        if self.tool != replay.tool
            || self.tool_artifact_hash != replay.tool_artifact_hash
            || self.tool_manifest_hash != replay.tool_manifest_hash
            || self.reality_manifest_hash != replay.reality_manifest_hash
            || self.effective_capability_hash != replay.effective_capability_hash
            || self.input_hashes != replay.input_hashes
        {
            return Err(Error::InvalidInput(
                "Replay comparison requires the same tool, manifests, capabilities, and inputs"
                    .into(),
            ));
        }
        Ok(
            if self.output_hashes == replay.output_hashes && self.result == replay.result {
                ReplayOutcome::ReplayMatch
            } else {
                ReplayOutcome::ReplayDivergence
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycleEventKind {
    ToolRequested,
    ToolResolved,
    CapabilitiesComputed,
    SandboxCreated,
    ToolStarted,
    ToolCompleted,
    Attested,
    SandboxDestroyed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLifecycleEvent {
    pub id: String,
    pub kind: ToolLifecycleEventKind,
    pub tool_id: Option<ToolId>,
    pub sandbox_id: Option<MicroSandboxId>,
    pub reality_id: Option<RealityId>,
    pub created_at: DateTime<Utc>,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOutcome {
    ReplayMatch,
    ReplayDivergence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionReceipt {
    pub attestation_id: crate::core::ExecutionAttestationId,
    pub result: ToolExecutionStatus,
    pub outputs: Vec<ArtifactRef>,
    pub effects: Vec<EffectId>,
}

impl ToolExecutionResult {
    /// Effectful tools return a structured request without receiving a
    /// database/network credential.  The host can pass this value to the
    /// existing EffectManager after applying its own policy and Reality scope.
    pub fn effect_request(&self) -> Option<Value> {
        serde_json::from_str::<Value>(&self.stdout)
            .ok()
            .and_then(|value| value.get("effect_request").cloned())
    }
}

pub trait SandboxSelectionPolicy: Send + Sync {
    fn select(
        &self,
        tool: &ToolDefinition,
        requirements: &SandboxRequirements,
    ) -> Result<MicroSandboxRuntime>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultSandboxSelectionPolicy;
impl SandboxSelectionPolicy for DefaultSandboxSelectionPolicy {
    fn select(
        &self,
        tool: &ToolDefinition,
        requirements: &SandboxRequirements,
    ) -> Result<MicroSandboxRuntime> {
        match &tool.invocation {
            ToolInvocation::WasiComponent { .. } => Ok(MicroSandboxRuntime::Wasi),
            ToolInvocation::EffectAdapter { .. } => Ok(MicroSandboxRuntime::EffectBoundary),
            ToolInvocation::NativeBinary { .. }
            | ToolInvocation::Script { .. }
            | ToolInvocation::Custom { .. } => {
                if requirements.portability_required
                    && requirements.filesystem >= IsolationLevel::StrongSandbox
                {
                    return Err(Error::Intervention(
                        "Requested sandbox runtime is unavailable; refusing a silent downgrade"
                            .into(),
                    ));
                }
                Ok(MicroSandboxRuntime::Container)
            }
        }
    }
}

fn resource_duration_ms(capabilities: &EffectiveToolCapabilities) -> u64 {
    capabilities
        .duration
        .max_ms
        .or(capabilities.resources.timeout_ms)
        .unwrap_or(300_000)
}

pub fn new_micro_sandbox(
    reality_id: RealityId,
    tool: &ToolDefinition,
    capabilities: EffectiveToolCapabilities,
    runtime: MicroSandboxRuntime,
) -> MicroSandbox {
    let created_at = Utc::now();
    MicroSandbox {
        id: MicroSandboxId::new(),
        reality_id,
        tool_id: tool.id.clone(),
        expires_at: created_at + Duration::milliseconds(resource_duration_ms(&capabilities) as i64),
        capabilities,
        runtime,
        created_at,
        destroyed_at: None,
    }
}

pub fn resolve_invocation(tool: &ToolDefinition, input: Value) -> Result<ResolvedToolInvocation> {
    validate_input(&tool.inputs.schema, &input)?;
    let (executable, args, effect_adapter) = match &tool.invocation {
        ToolInvocation::NativeBinary {
            executable,
            args_template,
        } => (
            Some(executable.clone()),
            resolve_args(args_template, &input)?,
            None,
        ),
        ToolInvocation::Script { interpreter, path } => {
            (Some(interpreter.clone()), vec![path.clone()], None)
        }
        ToolInvocation::WasiComponent { .. } | ToolInvocation::Custom { .. } => {
            (None, vec![], None)
        }
        ToolInvocation::EffectAdapter { adapter, operation } => {
            (None, vec![], Some((adapter.clone(), operation.clone())))
        }
    };
    Ok(ResolvedToolInvocation {
        tool: tool.identity(),
        executable,
        args,
        input,
        effect_adapter,
    })
}

fn resolve_args(template: &[String], input: &Value) -> Result<Vec<String>> {
    let encoded = serde_json::to_string(input)?;
    template
        .iter()
        .map(|arg| {
            let fields = input.as_object();
            for placeholder in input_placeholders(arg) {
                if placeholder != "input"
                    && !fields.is_some_and(|fields| fields.contains_key(placeholder))
                {
                    return Err(Error::InvalidInput(format!(
                        "Tool argument contains an unresolved input placeholder: {{{placeholder}}}"
                    )));
                }
            }
            let mut resolved = arg.replace("{input}", &encoded);
            if let Some(fields) = fields {
                for (name, value) in fields {
                    let placeholder = format!("{{{name}}}");
                    let replacement = value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    resolved = resolved.replace(&placeholder, &replacement);
                }
            }
            Ok(resolved)
        })
        .collect()
}

fn input_placeholders(template: &str) -> Vec<&str> {
    let mut placeholders = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let candidate = &rest[start + 1..];
        let Some(end) = candidate.find('}') else {
            break;
        };
        let name = &candidate[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            placeholders.push(name);
        }
        rest = &candidate[end + 1..];
    }
    placeholders
}
fn validate_input(schema: &Value, input: &Value) -> Result<()> {
    validate_value_against_schema(schema, input, "input")
}

pub fn validate_tool_output(schema: &Value, output: &str) -> Result<()> {
    if schema.get("type").is_none() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(output)
        .map_err(|error| Error::InvalidInput(format!("Tool output is not valid JSON: {error}")))?;
    validate_value_against_schema(schema, &value, "output")
}

fn validate_value_against_schema(schema: &Value, value: &Value, label: &str) -> Result<()> {
    let type_matches = match schema.get("type").and_then(Value::as_str) {
        None => true,
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some(_) => false,
    };
    if !type_matches {
        return Err(Error::InvalidInput(format!(
            "Tool {label} does not match its declared type"
        )));
    }
    if let (Some(object), Some(required)) = (
        value.as_object(),
        schema.get("required").and_then(Value::as_array),
    ) && let Some(missing) = required
        .iter()
        .filter_map(Value::as_str)
        .find(|name| !object.contains_key(*name))
    {
        return Err(Error::InvalidInput(format!(
            "Tool {label} is missing required field {missing}"
        )));
    }
    if let (Some(object), Some(properties)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        for (name, property_schema) in properties {
            if let Some(property) = object.get(name) {
                validate_value_against_schema(property_schema, property, label)?;
            }
        }
    }
    Ok(())
}

/// The small built-in catalog is intentionally finite.  These definitions are
/// useful for adapter integrations and as fixtures for capability-isolation
/// tests; they are not an implicit shell standard library.
pub fn builtin_tools() -> Vec<ToolDefinition> {
    let now = Utc::now();
    let mut definitions = Vec::new();
    let base = |name: &str,
                description: &str,
                invocation: ToolInvocation,
                filesystem: ToolFilesystemCapabilities,
                network: ToolNetworkCapabilities,
                process: ProcessCapabilities| ToolDefinition {
        id: ToolId::new(),
        name: name.into(),
        version: "1.0.0".into(),
        description: description.into(),
        invocation,
        capabilities: ToolCapabilityManifest {
            filesystem,
            process,
            network,
            environment: ToolEnvironmentCapabilities {
                readable: vec![],
                values: BTreeMap::new(),
            },
            credentials: vec![],
            effects: EffectCapabilities {
                propose: false,
                prepare: false,
                commit: false,
                scope: EffectCapabilityScope {
                    kinds: vec![],
                    target_patterns: vec![],
                    operations: vec![],
                },
            },
            resources: ResourceLimits::default(),
            duration: DurationCapability::default(),
        },
        inputs: ToolInputSchema::default(),
        outputs: ToolOutputSchema::default(),
        integrity: ToolIntegrity {
            artifact_hash: None,
            manifest_hash: String::new(),
            signature: None,
        },
        provenance: ToolProvenance {
            source: ToolSource::BuiltIn,
            registered_at: now,
            publisher: Some("Hardknock".into()),
        },
        trust: ToolTrust::LocalTrusted,
        disabled: false,
    };
    let read_process = ProcessCapabilities {
        allow_exec: true,
        allowed_executables: vec![ExecutablePattern("/bin/cat".into())],
        denied_executables: vec![],
        max_processes: Some(1),
    };
    let mut read_file = base(
        "read-file",
        "Read a selected workspace file",
        ToolInvocation::NativeBinary {
            executable: "/bin/cat".into(),
            args_template: vec!["{path}".into()],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/**".into()],
            write: vec![],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        read_process,
    );
    read_file.inputs = ToolInputSchema {
        schema: serde_json::json!({
            "type":"object",
            "required":["path"],
            "properties":{"path":{"type":"string"}}
        }),
    };
    definitions.push(read_file);
    let write_process = ProcessCapabilities {
        allow_exec: true,
        allowed_executables: vec![ExecutablePattern("/bin/sh".into())],
        denied_executables: vec![],
        max_processes: Some(1),
    };
    let mut write_file = base(
        "write-file",
        "Write a selected workspace file through an explicit tool",
        ToolInvocation::NativeBinary {
            executable: "/bin/sh".into(),
            args_template: vec![
                "-c".into(),
                "printf '%s' \"$2\" > \"$1\"".into(),
                "hardknock-write".into(),
                "{path}".into(),
                "{content}".into(),
            ],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/**".into()],
            write: vec!["$WORKSPACE/**".into()],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        write_process,
    );
    write_file.inputs = ToolInputSchema {
        schema: serde_json::json!({
            "type":"object",
            "required":["path","content"],
            "properties":{"path":{"type":"string"},"content":{"type":"string"}}
        }),
    };
    definitions.push(write_file);
    let test_process = ProcessCapabilities {
        allow_exec: true,
        allowed_executables: vec![
            ExecutablePattern("/usr/bin/env".into()),
            ExecutablePattern("/bin/sh".into()),
        ],
        denied_executables: vec![],
        max_processes: Some(64),
    };
    let mut run_tests = base(
        "run-tests",
        "Run repository tests without network or credentials",
        ToolInvocation::NativeBinary {
            executable: "/bin/sh".into(),
            args_template: vec!["-c".into(), "{command}".into()],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/**".into()],
            write: vec!["$WORKSPACE/.cache/**".into(), "$TMP/**".into()],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        test_process,
    );
    run_tests.inputs = ToolInputSchema {
        schema: serde_json::json!({
            "type":"object",
            "required":["command"],
            "properties":{"command":{"type":"string"}}
        }),
    };
    definitions.push(run_tests);
    let git_process = ProcessCapabilities {
        allow_exec: true,
        allowed_executables: vec![
            ExecutablePattern("/usr/bin/git".into()),
            ExecutablePattern("/bin/git".into()),
        ],
        denied_executables: vec![],
        max_processes: Some(4),
    };
    definitions.push(base(
        "git-diff",
        "Inspect the current Reality diff",
        ToolInvocation::NativeBinary {
            executable: "git".into(),
            args_template: vec!["diff".into()],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/**".into()],
            write: vec![],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        git_process,
    ));
    let mut metadata = base(
        "package-metadata",
        "Fetch package metadata from the configured registry",
        ToolInvocation::NativeBinary {
            executable: "/usr/bin/curl".into(),
            args_template: vec![
                "--fail".into(),
                "https://registry.npmjs.org/{package}".into(),
            ],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/package.json".into()],
            write: vec!["$TMP/**".into()],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::AllowList,
            allow: vec![NetworkEndpointPattern {
                host: "registry.npmjs.org".into(),
                port: 443,
            }],
        },
        ProcessCapabilities {
            allow_exec: true,
            allowed_executables: vec![ExecutablePattern("/usr/bin/curl".into())],
            denied_executables: vec![],
            max_processes: Some(4),
        },
    );
    metadata.inputs = ToolInputSchema {
        schema: serde_json::json!({
            "type":"object",
            "required":["package"],
            "properties":{"package":{"type":"string"}}
        }),
    };
    metadata.outputs = ToolOutputSchema {
        schema: serde_json::json!({"type":"object"}),
    };
    definitions.push(metadata);
    let mut effect = base(
        "effect-request",
        "Emit a structured request for host-side effect preparation",
        ToolInvocation::EffectAdapter {
            adapter: "hardknock".into(),
            operation: "propose".into(),
        },
        ToolFilesystemCapabilities {
            read: vec![],
            write: vec![],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        ProcessCapabilities::default(),
    );
    effect.capabilities.effects = EffectCapabilities {
        propose: true,
        prepare: true,
        commit: false,
        scope: EffectCapabilityScope {
            kinds: vec![crate::effects::EffectKind::Database],
            target_patterns: vec!["postgres://inventory_test/*".into()],
            operations: vec![
                EffectOperationPattern(crate::effects::EffectOperation::Create),
                EffectOperationPattern(crate::effects::EffectOperation::Update),
            ],
        },
    };
    effect.outputs = ToolOutputSchema {
        schema: serde_json::json!({"type":"object"}),
    };
    definitions.push(effect);
    let mut shell = base(
        "shell-generic",
        "Explicit high-capability shell for trusted development workflows",
        ToolInvocation::NativeBinary {
            executable: "/bin/sh".into(),
            args_template: vec!["-c".into(), "{command}".into()],
        },
        ToolFilesystemCapabilities {
            read: vec!["$WORKSPACE/**".into(), "$TMP/**".into()],
            write: vec!["$WORKSPACE/**".into(), "$TMP/**".into()],
        },
        ToolNetworkCapabilities {
            mode: NetworkMode::Unrestricted,
            allow: vec![],
        },
        ProcessCapabilities {
            allow_exec: true,
            allowed_executables: vec![],
            denied_executables: vec![],
            max_processes: Some(256),
        },
    );
    shell.inputs = ToolInputSchema {
        schema: serde_json::json!({
            "type":"object",
            "required":["command"],
            "properties":{"command":{"type":"string"}}
        }),
    };
    definitions.push(shell);
    for definition in &mut definitions {
        if let Ok(hash) = definition.artifact_hash() {
            definition.integrity.artifact_hash = hash;
        }
        if let Ok(hash) = definition.manifest_hash() {
            definition.integrity.manifest_hash = hash;
        }
    }
    definitions
}
