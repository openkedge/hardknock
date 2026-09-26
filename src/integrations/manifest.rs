// SPDX-License-Identifier: Apache-2.0
//! Versioned, bounded installation metadata for generic agent hosts.

use crate::{
    Error, Result,
    mcp::{PORTABLE_TOOLS, PortableToolEffect},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub use crate::mcp::MCP_PROTOCOL_VERSION;

pub const INTEGRATION_MANIFEST_SCHEMA: &str =
    "https://openkedge.github.io/hardknock/schemas/integration-manifest-v1.schema.json";
pub const INTEGRATION_MANIFEST_VERSION: &str = "1.0.0";
pub const MAX_INTEGRATION_MANIFEST_BYTES: usize = 32 * 1024;

const FORBIDDEN_CAPABILITIES: &[&str] = &[
    "agent_execution",
    "approval_grant",
    "experiment_creation",
    "external_effect_commit",
    "unrestricted_command_execution",
    "unrestricted_filesystem_access",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    pub schema: String,
    pub manifest_version: String,
    pub package: PackageMetadata,
    pub stability: Stability,
    pub runtime: RuntimeMetadata,
    pub transport: TransportMetadata,
    pub tools: Vec<ToolMetadata>,
    pub capabilities: Vec<CapabilityMetadata>,
    pub security: SecurityMetadata,
    pub support: PlatformSupport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    Experimental,
    Preview,
    Stable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMetadata {
    pub command: String,
    pub args: Vec<String>,
    pub required_environment: Vec<EnvironmentRequirement>,
    pub healthcheck: CommandSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequirement {
    pub name: String,
    pub description: String,
    pub secret: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_seconds: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportMetadata {
    pub kind: TransportKind,
    pub protocol: String,
    pub protocol_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Stdio,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub capability: String,
    pub effect: ToolEffect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    RecordsEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityMetadata {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityMetadata {
    pub local_bridge_only: bool,
    pub exclusions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformSupport {
    pub operating_systems: Vec<String>,
    pub architectures: Vec<String>,
}

pub fn manifest() -> IntegrationManifest {
    IntegrationManifest {
        schema: INTEGRATION_MANIFEST_SCHEMA.into(),
        manifest_version: INTEGRATION_MANIFEST_VERSION.into(),
        package: PackageMetadata {
            name: env!("CARGO_PKG_NAME").into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        stability: Stability::Preview,
        runtime: RuntimeMetadata {
            command: "hardknock".into(),
            args: vec!["mcp".into(), "serve".into(), "--stdio".into()],
            required_environment: Vec::new(),
            healthcheck: CommandSpec {
                command: "hardknock".into(),
                args: vec!["doctor".into(), "--strict".into(), "--json".into()],
                timeout_seconds: 10,
            },
        },
        transport: TransportMetadata {
            kind: TransportKind::Stdio,
            protocol: "mcp".into(),
            protocol_version: MCP_PROTOCOL_VERSION.into(),
        },
        tools: PORTABLE_TOOLS
            .iter()
            .map(|tool| ToolMetadata {
                name: tool.name.into(),
                description: tool.description.into(),
                capability: tool.capability.into(),
                effect: match tool.effect {
                    PortableToolEffect::ReadOnly => ToolEffect::ReadOnly,
                    PortableToolEffect::RecordsEvidence => ToolEffect::RecordsEvidence,
                },
            })
            .collect(),
        capabilities: PORTABLE_TOOLS
            .iter()
            .map(|tool| CapabilityMetadata {
                id: tool.capability.into(),
                description: tool.capability_description.into(),
            })
            .collect(),
        security: SecurityMetadata {
            local_bridge_only: true,
            exclusions: FORBIDDEN_CAPABILITIES
                .iter()
                .map(|value| (*value).into())
                .collect(),
        },
        support: PlatformSupport {
            operating_systems: vec!["linux".into(), "macos".into()],
            architectures: vec!["aarch64".into(), "x86_64".into()],
        },
    }
}

pub fn to_bounded_json(value: &IntegrationManifest) -> Result<Vec<u8>> {
    validate(value)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_INTEGRATION_MANIFEST_BYTES {
        return Err(Error::InvalidInput(format!(
            "Integration manifest exceeds {MAX_INTEGRATION_MANIFEST_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

pub fn validate(value: &IntegrationManifest) -> Result<()> {
    if value.schema != INTEGRATION_MANIFEST_SCHEMA {
        return invalid("integration manifest schema identifier is unsupported");
    }
    if value.manifest_version != INTEGRATION_MANIFEST_VERSION {
        return invalid("integration manifest version is unsupported");
    }
    if value.package.name != env!("CARGO_PKG_NAME")
        || value.package.version != env!("CARGO_PKG_VERSION")
    {
        return invalid("integration manifest package metadata does not match this build");
    }
    if value.runtime.command != "hardknock"
        || value.runtime.args != ["mcp", "serve", "--stdio"]
        || value.runtime.healthcheck.command != "hardknock"
        || value.runtime.healthcheck.timeout_seconds == 0
        || value.runtime.healthcheck.timeout_seconds > 30
    {
        return invalid("integration manifest runtime command is not bounded");
    }
    if value
        .runtime
        .required_environment
        .iter()
        .any(|item| item.name.is_empty() || item.secret)
    {
        return invalid("integration manifest cannot require secret environment variables");
    }
    if value.transport.kind != TransportKind::Stdio
        || value.transport.protocol != "mcp"
        || value.transport.protocol_version != MCP_PROTOCOL_VERSION
    {
        return invalid("integration manifest transport is unsupported");
    }
    if value.tools.is_empty() || value.tools.len() > 16 || value.capabilities.len() > 16 {
        return invalid("integration manifest capability surface is not bounded");
    }

    let capabilities = value
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect::<BTreeSet<_>>();
    if capabilities.len() != value.capabilities.len()
        || value
            .tools
            .iter()
            .any(|tool| !capabilities.contains(tool.capability.as_str()))
    {
        return invalid("integration manifest tool capabilities are missing or duplicated");
    }
    if capabilities
        .iter()
        .any(|capability| FORBIDDEN_CAPABILITIES.contains(capability))
        || value
            .tools
            .iter()
            .any(|tool| FORBIDDEN_CAPABILITIES.contains(&tool.name.as_str()))
    {
        return invalid("integration manifest exposes a forbidden capability");
    }

    let exclusions = value
        .security
        .exclusions
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !value.security.local_bridge_only
        || FORBIDDEN_CAPABILITIES
            .iter()
            .any(|capability| !exclusions.contains(capability))
    {
        return invalid("integration manifest security exclusions are incomplete");
    }
    if value.support.operating_systems.is_empty()
        || value.support.architectures.is_empty()
        || value.support.operating_systems.len() > 8
        || value.support.architectures.len() > 8
    {
        return invalid("integration manifest platform support is not bounded");
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(Error::InvalidInput(message.into()))
}
