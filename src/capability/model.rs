// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{
        CapabilityEscalationId, CapabilityEventId, CapabilityManifestId, CredentialId,
        ExecutionAttestationId, RealityId,
    },
    effects::{EffectKind, EffectOperation},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, path::Path};

pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    #[default]
    None,
    Cooperative,
    Process,
    Container,
    StrongSandbox,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealityProviderCapabilities {
    pub filesystem_isolation: IsolationLevel,
    pub process_isolation: IsolationLevel,
    pub network_isolation: IsolationLevel,
    pub credential_isolation: IsolationLevel,
    pub external_effect_control: EffectControlLevel,
    #[serde(default)]
    pub known_limitations: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectControlLevel {
    #[default]
    Cooperative,
    Gated,
}

impl RealityProviderCapabilities {
    pub fn git_worktree() -> Self {
        Self {
            filesystem_isolation: IsolationLevel::Cooperative,
            process_isolation: IsolationLevel::None,
            network_isolation: IsolationLevel::None,
            credential_isolation: IsolationLevel::None,
            external_effect_control: EffectControlLevel::Cooperative,
            known_limitations: vec![
                "processes, host network, and ambient credentials are not isolated".into(),
            ],
        }
    }

    pub fn container(network: NetworkMode) -> Self {
        Self {
            filesystem_isolation: IsolationLevel::Container,
            process_isolation: IsolationLevel::Container,
            network_isolation: match network {
                NetworkMode::Unrestricted => IsolationLevel::None,
                _ => IsolationLevel::Container,
            },
            credential_isolation: IsolationLevel::Container,
            external_effect_control: EffectControlLevel::Gated,
            known_limitations: vec![
                "container shares the host kernel and is not a hardened multi-tenant sandbox"
                    .into(),
                "allow-list mode is limited to dedicated internal container networks".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBoundary {
    pub provider: String,
    pub capabilities: RealityProviderCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_id: Option<CapabilityManifestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default)]
    pub manifest_revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub frozen: bool,
}

impl Default for ExecutionBoundary {
    fn default() -> Self {
        Self {
            provider: "git-worktree".into(),
            capabilities: RealityProviderCapabilities::git_worktree(),
            manifest_id: None,
            manifest_hash: None,
            manifest_revision: 0,
            image_digest: None,
            frozen: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemScope {
    pub root: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

fn default_true() -> bool {
    true
}

impl FilesystemScope {
    pub fn new(root: impl Into<String>) -> Result<Self> {
        let scope = Self {
            root: root.into(),
            recursive: true,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<()> {
        let path = Path::new(&self.root);
        if !path.is_absolute()
            || self.root.contains(['\0', '\n', '\r'])
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(Error::InvalidInput(format!(
                "Filesystem scope must be a normalized absolute path: {}",
                self.root
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemCapabilities {
    #[serde(default)]
    pub readable: Vec<FilesystemScope>,
    #[serde(default)]
    pub writable: Vec<FilesystemScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutablePattern(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCapabilities {
    pub allow_exec: bool,
    #[serde(default)]
    pub allowed_executables: Vec<ExecutablePattern>,
    #[serde(default)]
    pub denied_executables: Vec<ExecutablePattern>,
    pub max_processes: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    None,
    LoopbackOnly,
    AllowList,
    Unrestricted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkEndpointPattern {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for NetworkEndpointPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl NetworkEndpointPattern {
    pub fn validate(&self) -> Result<()> {
        if self.host.is_empty()
            || self.host.len() > 253
            || self.host.contains(['/', '\\', '\0', '\n', '\r', ' '])
            || self.port == 0
        {
            return Err(Error::InvalidInput(format!(
                "Invalid network endpoint pattern {self}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCapabilities {
    pub mode: NetworkMode,
    #[serde(default)]
    pub allow: Vec<NetworkEndpointPattern>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCapabilities {
    #[serde(default)]
    pub readable: Vec<String>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialScope {
    pub resource: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialCapability {
    pub provider: String,
    pub name: String,
    pub scope: CredentialScope,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectCapabilityScope {
    #[serde(default)]
    pub kinds: Vec<EffectKind>,
    #[serde(default)]
    pub target_patterns: Vec<String>,
    #[serde(default)]
    pub operations: Vec<EffectOperationPattern>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectOperationPattern(pub EffectOperation);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectCapabilities {
    pub propose: bool,
    pub prepare: bool,
    pub commit: bool,
    pub scope: EffectCapabilityScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu: Option<String>,
    pub memory_mb: Option<u64>,
    pub pids: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub output_bytes: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu: Some("1.0".into()),
            memory_mb: Some(1024),
            pids: Some(256),
            timeout_ms: Some(300_000),
            output_bytes: Some(8 * 1024 * 1024),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityManifest {
    pub id: CapabilityManifestId,
    pub profile: String,
    pub revision: u32,
    pub filesystem: FilesystemCapabilities,
    pub process: ProcessCapabilities,
    pub network: NetworkCapabilities,
    pub environment: EnvironmentCapabilities,
    #[serde(default)]
    pub credentials: Vec<CredentialCapability>,
    pub effects: EffectCapabilities,
    pub resources: ResourceLimits,
    pub created_at: DateTime<Utc>,
}

impl CapabilityManifest {
    pub fn validate(&self) -> Result<()> {
        if self.profile.trim().is_empty() || self.profile.len() > 128 || self.revision == 0 {
            return Err(Error::InvalidInput(
                "Capability manifest profile and revision must be bounded and nonzero".into(),
            ));
        }
        for scope in self
            .filesystem
            .readable
            .iter()
            .chain(&self.filesystem.writable)
        {
            scope.validate()?;
        }
        for endpoint in &self.network.allow {
            endpoint.validate()?;
        }
        if self.network.mode != NetworkMode::AllowList && !self.network.allow.is_empty() {
            return Err(Error::InvalidInput(
                "Network endpoints require allow-list mode".into(),
            ));
        }
        for key in self
            .environment
            .readable
            .iter()
            .chain(self.environment.values.keys())
        {
            if !valid_environment_name(key) {
                return Err(Error::InvalidInput(format!(
                    "Invalid environment capability {key}"
                )));
            }
        }
        for value in self.environment.values.values() {
            if value.len() > 4096 || value.contains(['\0', '\n', '\r']) {
                return Err(Error::InvalidInput(
                    "Environment capability values must be bounded single-line strings".into(),
                ));
            }
        }
        for credential in &self.credentials {
            for value in [
                credential.provider.as_str(),
                credential.name.as_str(),
                credential.scope.resource.as_str(),
            ] {
                if value.trim().is_empty()
                    || value.len() > 512
                    || value.contains(['\0', '\n', '\r'])
                {
                    return Err(Error::InvalidInput(
                        "Credential capabilities require bounded provider, name, and resource scopes"
                            .into(),
                    ));
                }
            }
            if credential.permissions.is_empty()
                || credential.permissions.iter().any(|permission| {
                    permission.trim().is_empty()
                        || permission.len() > 128
                        || permission.contains(['\0', '\n', '\r'])
                })
            {
                return Err(Error::InvalidInput(
                    "Credential capabilities require explicit bounded permissions".into(),
                ));
            }
        }
        let effects_enabled = self.effects.propose || self.effects.prepare || self.effects.commit;
        if effects_enabled
            && (self.effects.scope.kinds.is_empty()
                || self.effects.scope.target_patterns.is_empty()
                || self.effects.scope.operations.is_empty())
        {
            return Err(Error::InvalidInput(
                "Enabled Effect capabilities require explicit kinds, targets, and operations"
                    .into(),
            ));
        }
        for target in &self.effects.scope.target_patterns {
            if target.trim().is_empty()
                || target.len() > 2048
                || target.contains(['\0', '\n', '\r'])
                || target.strip_suffix('*').unwrap_or(target).contains('*')
            {
                return Err(Error::InvalidInput(
                    "Effect target patterns support only an optional trailing wildcard".into(),
                ));
            }
        }
        if self.resources.pids == Some(0)
            || self.resources.memory_mb == Some(0)
            || self.resources.output_bytes == Some(0)
        {
            return Err(Error::InvalidInput(
                "Resource limits must be positive when present".into(),
            ));
        }
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::InvalidInput(
                "Capability manifest exceeded 256 KiB".into(),
            ));
        }
        Ok(())
    }

    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "scope", rename_all = "snake_case")]
pub enum ExecutionCapability {
    FilesystemRead(FilesystemScope),
    FilesystemWrite(FilesystemScope),
    ProcessExecute(ExecutablePattern),
    NetworkConnect(NetworkEndpointPattern),
    EnvironmentRead(String),
    CredentialUse(CredentialCapability),
    EffectPropose(EffectCapabilityScope),
    EffectPrepare(EffectCapabilityScope),
    EffectCommit(EffectCapabilityScope),
    Custom {
        kind: String,
        payload: serde_json::Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request_kind", rename_all = "snake_case")]
pub enum CapabilityRequest {
    Filesystem {
        operation: FilesystemOperation,
        path: String,
    },
    Process {
        executable: String,
    },
    Network {
        endpoint: NetworkEndpointPattern,
        mutation: NetworkMutationClass,
    },
    Environment {
        name: String,
    },
    Credential {
        provider: String,
        name: String,
        resource: String,
        permission: String,
    },
    Effect {
        stage: EffectCapabilityStage,
        kind: EffectKind,
        target: String,
        operation: EffectOperation,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemOperation {
    Read,
    Write,
    Delete,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMutationClass {
    Read,
    Mutation,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectCapabilityStage {
    Propose,
    Prepare,
    Commit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDecision {
    Allow,
    Deny,
    RequireApproval,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEvaluation {
    pub decision: CapabilityDecision,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityViolationObservation {
    pub action: CapabilityRequest,
    pub capability: Option<ExecutionCapability>,
    pub decision: CapabilityDecision,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEventKind {
    Allowed,
    Denied,
    ApprovalRequired,
    CredentialIssued,
    CredentialRevoked,
    ManifestRevised,
    Frozen,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityEvent {
    pub id: CapabilityEventId,
    pub reality_id: RealityId,
    pub manifest_id: CapabilityManifestId,
    pub kind: CapabilityEventKind,
    pub request: Option<CapabilityRequest>,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealityRequirements {
    pub filesystem_isolation: IsolationLevel,
    pub process_isolation: IsolationLevel,
    pub network_isolation: IsolationLevel,
    pub credential_isolation: IsolationLevel,
    pub effect_gating: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEscalationRequest {
    pub id: CapabilityEscalationId,
    pub reality_id: RealityId,
    pub requested: ExecutionCapability,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEscalationDecision {
    GrantTemporary,
    Deny,
    RequireUserApproval,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssuedCredential {
    pub id: CredentialId,
    pub reality_id: RealityId,
    pub provider: String,
    pub name: String,
    pub scope: CredentialScope,
    pub expires_at: Option<DateTime<Utc>>,
    /// Opaque local reference only. Raw secret bytes are never serialized here.
    pub secret_ref: String,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAssurance {
    pub reality_provider: String,
    pub isolation: RealityProviderCapabilities,
    pub capability_manifest_hash: Option<String>,
    pub external_effect_gating: bool,
    #[serde(default)]
    pub origin: ExecutionEvidenceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_id: Option<ExecutionAttestationId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEvidenceOrigin {
    HostProcess,
    ContainerReality,
    MicroSandbox,
    Wasi,
    EffectBoundary,
    #[default]
    Unknown,
}
