// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Error, Result};
use std::path::{Component, Path, PathBuf};

pub trait CapabilityPolicy: Send + Sync {
    fn evaluate(
        &self,
        request: &CapabilityRequest,
        manifest: &CapabilityManifest,
    ) -> CapabilityEvaluation;
}

pub struct DenyByDefaultCapabilityPolicy;

impl CapabilityPolicy for DenyByDefaultCapabilityPolicy {
    fn evaluate(
        &self,
        request: &CapabilityRequest,
        manifest: &CapabilityManifest,
    ) -> CapabilityEvaluation {
        let allowed = match request {
            CapabilityRequest::Filesystem { operation, path } => {
                let scopes = match operation {
                    FilesystemOperation::Read | FilesystemOperation::List => {
                        &manifest.filesystem.readable
                    }
                    FilesystemOperation::Write | FilesystemOperation::Delete => {
                        &manifest.filesystem.writable
                    }
                };
                normalized_virtual_path(path)
                    .ok()
                    .is_some_and(|path| scopes.iter().any(|scope| scope_allows(scope, &path)))
            }
            CapabilityRequest::Process { executable } => {
                manifest.process.allow_exec
                    && !manifest
                        .process
                        .denied_executables
                        .iter()
                        .any(|pattern| pattern_matches(&pattern.0, executable))
                    && (manifest.process.allowed_executables.is_empty()
                        || manifest
                            .process
                            .allowed_executables
                            .iter()
                            .any(|pattern| pattern_matches(&pattern.0, executable)))
            }
            CapabilityRequest::Network { endpoint, .. } => match manifest.network.mode {
                NetworkMode::None => false,
                NetworkMode::LoopbackOnly => {
                    matches!(endpoint.host.as_str(), "localhost" | "127.0.0.1" | "::1")
                }
                NetworkMode::AllowList => manifest.network.allow.contains(endpoint),
                NetworkMode::Unrestricted => true,
            },
            CapabilityRequest::Environment { name } => manifest
                .environment
                .readable
                .iter()
                .any(|item| item == name),
            CapabilityRequest::Credential {
                provider,
                name,
                resource,
                permission,
            } => manifest.credentials.iter().any(|credential| {
                credential.provider == *provider
                    && credential.name == *name
                    && pattern_matches(&credential.scope.resource, resource)
                    && credential.permissions.iter().any(|item| item == permission)
                    && credential
                        .expires_at
                        .is_none_or(|expiry| expiry > chrono::Utc::now())
            }),
            CapabilityRequest::Effect {
                stage,
                kind,
                target,
                operation,
            } => {
                let stage_allowed = match stage {
                    EffectCapabilityStage::Propose => manifest.effects.propose,
                    EffectCapabilityStage::Prepare => manifest.effects.prepare,
                    EffectCapabilityStage::Commit => manifest.effects.commit,
                };
                stage_allowed
                    && effect_scope_allows(&manifest.effects.scope, *kind, target, operation)
            }
        };
        if allowed {
            CapabilityEvaluation {
                decision: CapabilityDecision::Allow,
                reason: format!("allowed by capability profile {}", manifest.profile),
            }
        } else {
            CapabilityEvaluation {
                decision: CapabilityDecision::Deny,
                reason: format!(
                    "denied by default: capability profile {} does not grant this exact action",
                    manifest.profile
                ),
            }
        }
    }
}

fn effect_scope_allows(
    scope: &EffectCapabilityScope,
    kind: crate::effects::EffectKind,
    target: &str,
    operation: &crate::effects::EffectOperation,
) -> bool {
    scope.kinds.contains(&kind)
        && scope
            .target_patterns
            .iter()
            .any(|pattern| pattern_matches(pattern, target))
        && scope
            .operations
            .iter()
            .any(|pattern| pattern.0 == *operation)
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        pattern == value
    }
}

fn normalized_virtual_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if !path.is_absolute() || value.contains('\0') {
        return Err(Error::InvalidInput(
            "Capability path must be an absolute virtual path".into(),
        ));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(Error::Intervention(
                    "Path traversal is outside the Reality capability scope".into(),
                ));
            }
        }
    }
    Ok(normalized)
}

fn scope_allows(scope: &FilesystemScope, path: &Path) -> bool {
    let root = Path::new(&scope.root);
    path == root || (scope.recursive && path.starts_with(root))
}

/// Resolve a virtual `/workspace` path against a managed host workspace. Existing
/// ancestors are canonicalized so a repository symlink cannot escape the root.
pub fn resolve_workspace_path(workspace: &Path, virtual_path: &str) -> Result<PathBuf> {
    let normalized = normalized_virtual_path(virtual_path)?;
    let relative = normalized
        .strip_prefix("/workspace")
        .map_err(|_| Error::Intervention("File proxy only maps the /workspace namespace".into()))?;
    let canonical_workspace = workspace.canonicalize()?;
    let candidate = canonical_workspace.join(relative);
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| {
            Error::Intervention("File path has no managed workspace ancestor".into())
        })?;
    }
    if !ancestor.canonicalize()?.starts_with(&canonical_workspace) {
        return Err(Error::Intervention(
            "Symlink path escapes the managed workspace".into(),
        ));
    }
    if candidate.exists() && !candidate.canonicalize()?.starts_with(&canonical_workspace) {
        return Err(Error::Intervention(
            "Symlink target escapes the managed workspace".into(),
        ));
    }
    Ok(candidate)
}

pub trait RealitySelectionPolicy {
    fn select(
        &self,
        requirements: &RealityRequirements,
        available: &[(&str, RealityProviderCapabilities)],
    ) -> Result<String>;
}

pub struct MinimumSufficientRealityPolicy;

impl RealitySelectionPolicy for MinimumSufficientRealityPolicy {
    fn select(
        &self,
        requirements: &RealityRequirements,
        available: &[(&str, RealityProviderCapabilities)],
    ) -> Result<String> {
        available
            .iter()
            .find(|(_, capabilities)| provider_satisfies(capabilities, requirements))
            .map(|(id, _)| (*id).to_owned())
            .ok_or_else(|| {
                Error::Intervention(
                    "No available Reality provider satisfies the declared isolation requirements"
                        .into(),
                )
            })
    }
}

pub fn provider_satisfies(
    provider: &RealityProviderCapabilities,
    requirements: &RealityRequirements,
) -> bool {
    provider.filesystem_isolation >= requirements.filesystem_isolation
        && provider.process_isolation >= requirements.process_isolation
        && provider.network_isolation >= requirements.network_isolation
        && provider.credential_isolation >= requirements.credential_isolation
        && (!requirements.effect_gating
            || provider.external_effect_control == EffectControlLevel::Gated)
}
