// SPDX-License-Identifier: Apache-2.0

use crate::{
    Result,
    capability::{CapabilityManifest, builtin_profile},
    tool::{
        EffectiveToolCapabilities, ToolCapabilityManifest, ToolDefinition, ToolRegistry,
        builtin_tools, resolve_effective_capabilities,
    },
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityExposureDimensions {
    pub network_endpoints_exposed: usize,
    pub credential_grants: usize,
    pub writable_scopes: usize,
    pub effect_permissions: usize,
    pub exposure_duration_ms: u64,
    pub network_exposure_duration_ms: u64,
    pub credential_exposure_duration_ms: u64,
    pub write_exposure_duration_ms: u64,
    pub effect_exposure_duration_ms: u64,
}

impl CapabilityExposureDimensions {
    pub fn from_effective(capabilities: &EffectiveToolCapabilities) -> Self {
        let surface = capabilities.surface();
        let duration = surface.exposure_duration_ms;
        Self {
            network_endpoints_exposed: surface.network_endpoints,
            credential_grants: surface.credential_scopes,
            writable_scopes: surface.writable_scopes,
            effect_permissions: surface.effect_permissions,
            exposure_duration_ms: duration,
            network_exposure_duration_ms: if capabilities.network.mode
                == crate::capability::NetworkMode::None
            {
                0
            } else {
                duration
            },
            credential_exposure_duration_ms: if capabilities.credentials.is_empty() {
                0
            } else {
                duration
            },
            write_exposure_duration_ms: if capabilities.filesystem.write.is_empty() {
                0
            } else {
                duration
            },
            effect_exposure_duration_ms: if surface.effect_permissions == 0 {
                0
            } else {
                duration
            },
        }
    }
    pub fn add_assign(&mut self, other: &Self) {
        self.network_endpoints_exposed += other.network_endpoints_exposed;
        self.credential_grants += other.credential_grants;
        self.writable_scopes += other.writable_scopes;
        self.effect_permissions += other.effect_permissions;
        self.exposure_duration_ms = self
            .exposure_duration_ms
            .saturating_add(other.exposure_duration_ms);
        self.network_exposure_duration_ms = self
            .network_exposure_duration_ms
            .saturating_add(other.network_exposure_duration_ms);
        self.credential_exposure_duration_ms = self
            .credential_exposure_duration_ms
            .saturating_add(other.credential_exposure_duration_ms);
        self.write_exposure_duration_ms = self
            .write_exposure_duration_ms
            .saturating_add(other.write_exposure_duration_ms);
        self.effect_exposure_duration_ms = self
            .effect_exposure_duration_ms
            .saturating_add(other.effect_exposure_duration_ms);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExposureBenchmark {
    pub session_container: CapabilityExposureDimensions,
    pub micro_sandboxes: CapabilityExposureDimensions,
    pub tool_count: usize,
    pub capability_resolution_ms: u64,
    pub runtime_startup_ms: Option<u64>,
    pub notes: Vec<String>,
}

pub fn compare_exposure(
    reality: &CapabilityManifest,
    tools: &[ToolDefinition],
) -> Result<ToolExposureBenchmark> {
    let started = Instant::now();
    let mut micro = CapabilityExposureDimensions::default();
    for tool in tools {
        let effective = resolve_effective_capabilities(reality, &tool.capabilities, &[])?;
        micro.add_assign(&CapabilityExposureDimensions::from_effective(&effective));
    }
    let session_window = reality
        .resources
        .timeout_ms
        .unwrap_or(0)
        .saturating_mul(tools.len() as u64);
    let session_effect_permissions = reality.effects.scope.kinds.len()
        * reality.effects.scope.operations.len()
        * reality.effects.scope.target_patterns.len();
    let session = CapabilityExposureDimensions {
        network_endpoints_exposed: reality.network.allow.len(),
        credential_grants: reality.credentials.len(),
        writable_scopes: reality.filesystem.writable.len(),
        effect_permissions: session_effect_permissions,
        exposure_duration_ms: session_window,
        network_exposure_duration_ms: if reality.network.mode
            == crate::capability::NetworkMode::None
        {
            0
        } else {
            session_window
        },
        credential_exposure_duration_ms: if reality.credentials.is_empty() {
            0
        } else {
            session_window
        },
        write_exposure_duration_ms: if reality.filesystem.writable.is_empty() {
            0
        } else {
            session_window
        },
        effect_exposure_duration_ms: if session_effect_permissions == 0 {
            0
        } else {
            session_window
        },
    };
    Ok(ToolExposureBenchmark { session_container: session, micro_sandboxes: micro, tool_count: tools.len(), capability_resolution_ms: started.elapsed().as_millis() as u64, runtime_startup_ms: None, notes: vec!["Dimensions are reported separately; no synthetic security score is calculated".into(), "Exposure durations are configured maximum windows from manifest timeouts, not observed wall-clock runtime".into(), "Runtime startup requires an installed Docker/Podman provider and is therefore optional".into()] })
}

pub fn builtin_exposure_benchmark() -> Result<ToolExposureBenchmark> {
    let reality = builtin_profile("coding-networked")?;
    let mut registry = ToolRegistry::new();
    for tool in builtin_tools()
        .into_iter()
        .filter(|tool| tool.name != "shell-generic")
    {
        registry.register(tool)?;
    }
    compare_exposure(&reality, &registry.list())
}

/// Keep this helper available to callers that need to include a custom tool
/// manifest in the same benchmark without constructing a full Definition.
pub fn effective_dimensions(
    reality: &CapabilityManifest,
    tool: &ToolCapabilityManifest,
) -> Result<CapabilityExposureDimensions> {
    Ok(CapabilityExposureDimensions::from_effective(
        &resolve_effective_capabilities(reality, tool, &[])?,
    ))
}
