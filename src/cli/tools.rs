// SPDX-License-Identifier: Apache-2.0

use super::{Cli, Commands};
use crate::{
    Error, Result,
    capability::CapabilityManifest,
    core::RealityId,
    store::{CapabilityStore, Store, ToolStore},
    tool::{ToolDefinition, ToolRegistry, builtin_tools},
    tool_runtime::{ContainerMicroSandboxProvider, HostMicroSandboxProvider, ToolRouter},
};
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ToolRuntimeArg {
    Container,
    Host,
}

#[derive(Debug, Subcommand)]
pub enum ToolCommand {
    List {
        #[arg(long)]
        include_disabled: bool,
    },
    Show {
        tool: String,
    },
    Verify {
        tool: String,
    },
    Validate {
        file: PathBuf,
    },
    Register {
        file: PathBuf,
    },
    Disable {
        tool: String,
    },
    Audit {
        #[arg(long)]
        sandbox: Option<crate::core::MicroSandboxId>,
    },
    Benchmark {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Run {
        tool: String,
        #[arg(long, default_value = "{}")]
        input: String,
        #[arg(long)]
        reality: Option<RealityId>,
        #[arg(long, value_enum, default_value = "container")]
        runtime: ToolRuntimeArg,
        #[arg(
            long,
            help = "Required with --runtime host; records Observed, not isolated, execution"
        )]
        allow_host_fallback: bool,
        #[arg(
            long,
            help = "Resolve and display capabilities without executing the tool"
        )]
        explain_capabilities: bool,
    },
}

pub fn handles(command: &Commands) -> bool {
    matches!(command, Commands::Tool { .. })
}

fn ensure_builtins(store: &Store) -> Result<()> {
    let existing = store.tool_definitions(true)?;
    for builtin in builtin_tools() {
        if existing
            .iter()
            .any(|tool| tool.name == builtin.name && tool.version == builtin.version)
        {
            continue;
        }
        store.insert_tool_definition(&builtin)?;
    }
    Ok(())
}

fn registry(store: &Store) -> Result<ToolRegistry> {
    ensure_builtins(store)?;
    let mut registry = ToolRegistry::new();
    for tool in store.tool_definitions(false)? {
        registry.register(tool)?;
    }
    Ok(registry)
}

pub async fn execute(
    cli: &Cli,
    store: &Store,
    _cancel: &crate::cancellation::Cancellation,
) -> Result<Value> {
    let Commands::Tool { command } = &cli.command else {
        return Err(Error::InvalidInput("Tool dispatch failed".into()));
    };
    match command {
        ToolCommand::List { include_disabled } => {
            ensure_builtins(store)?;
            Ok(json!({"tools":store.tool_definitions(*include_disabled)?}))
        }
        ToolCommand::Show { tool } => {
            ensure_builtins(store)?;
            Ok(serde_json::to_value(
                store.tool_definition_by_name(tool).or_else(|_| {
                    tool.parse()
                        .ok()
                        .map(|id| store.tool_definition(&id))
                        .unwrap_or_else(|| Err(Error::NotFound(format!("Tool {tool} not found"))))
                })?,
            )?)
        }
        ToolCommand::Verify { tool } => {
            ensure_builtins(store)?;
            let definition = store.tool_definition_by_name(tool).or_else(|_| {
                tool.parse()
                    .ok()
                    .map(|id| store.tool_definition(&id))
                    .unwrap_or_else(|| Err(Error::NotFound(format!("Tool {tool} not found"))))
            })?;
            let mut registry = ToolRegistry::new();
            registry.register(definition)?;
            Ok(serde_json::to_value(registry.verify(tool)?)?)
        }
        ToolCommand::Validate { file } => {
            let definition = ToolDefinition::from_toml_file(file)?;
            definition.validate()?;
            Ok(json!({"valid":true,"tool":definition,"manifest_hash":definition.manifest_hash()?}))
        }
        ToolCommand::Register { file } => {
            let definition = ToolDefinition::from_toml_file(file)?;
            definition.validate()?;
            ensure_builtins(store)?;
            if store
                .tool_definitions(true)?
                .iter()
                .any(|tool| tool.name == definition.name && tool.version == definition.version)
            {
                return Err(Error::Intervention(format!(
                    "Tool {}@{} is already registered",
                    definition.name, definition.version
                )));
            }
            store.insert_tool_definition(&definition)?;
            Ok(json!({"registered":true,"tool":definition}))
        }
        ToolCommand::Disable { tool } => {
            ensure_builtins(store)?;
            let definition = store.tool_definition_by_name(tool).or_else(|_| {
                tool.parse()
                    .ok()
                    .map(|id| store.tool_definition(&id))
                    .unwrap_or_else(|| Err(Error::NotFound(format!("Tool {tool} not found"))))
            })?;
            store.disable_tool_definition(&definition.id)?;
            Ok(json!({"disabled":true,"tool":definition.id}))
        }
        ToolCommand::Audit { sandbox } => {
            let events = store.tool_lifecycle_events(sandbox.as_ref())?;
            let attestations = store.execution_attestations(None)?;
            let sandboxes = if let Some(id) = sandbox {
                vec![store.micro_sandbox(id)?]
            } else {
                store.micro_sandboxes()?
            };
            Ok(
                json!({"events":events,"summary":{"tools_executed":attestations.len(),"capability_denials":events.iter().filter(|event| event.kind == crate::tool::ToolLifecycleEventKind::Failed).count(),"network_grants":sandboxes.iter().map(|sandbox| sandbox.capabilities.network.allow.len()).sum::<usize>(),"credential_grants":sandboxes.iter().map(|sandbox| sandbox.capabilities.credentials.len()).sum::<usize>(),"effect_proposals":attestations.iter().map(|attestation| attestation.effect_refs.len()).sum::<usize>(),"runtime_failures":attestations.iter().filter(|attestation| attestation.result == crate::tool::ToolExecutionStatus::RuntimeFailure).count()},"sandboxes":sandboxes,"attestations":attestations}),
            )
        }
        ToolCommand::Benchmark { output } => {
            let report = crate::tool_benchmark::builtin_exposure_benchmark()?;
            if let Some(path) = output {
                std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
            }
            Ok(serde_json::to_value(report)?)
        }
        ToolCommand::Run {
            tool,
            input,
            reality,
            runtime,
            allow_host_fallback,
            explain_capabilities,
        } => {
            ensure_builtins(store)?;
            let reality_id = reality
                .clone()
                .or_else(|| {
                    store.realities().ok().and_then(|items| {
                        items
                            .into_iter()
                            .rev()
                            .find(|item| {
                                matches!(
                                    item.status,
                                    crate::core::RealityStatus::Created
                                        | crate::core::RealityStatus::Running
                                )
                            })
                            .map(|item| item.id)
                    })
                })
                .ok_or_else(|| {
                    Error::NotFound("No active Reality available; pass --reality".into())
                })?;
            let reality_record = store.reality(&reality_id)?;
            let manifest: CapabilityManifest = store.effective_capability_manifest(&reality_id)?;
            let input: Value = serde_json::from_str(input).map_err(|error| {
                Error::InvalidInput(format!("Tool input must be valid JSON: {error}"))
            })?;
            let registry = registry(store)?;
            if *explain_capabilities {
                let router = ToolRouter::new(registry, HostMicroSandboxProvider::new(true));
                let effective = router.resolve_capabilities(&manifest, tool, &[])?;
                return Ok(
                    json!({"reality":reality_id,"tool":tool,"reality_permits":manifest,"effective":effective,"surface":effective.surface()}),
                );
            }
            let run = match runtime {
                ToolRuntimeArg::Host => {
                    if !allow_host_fallback {
                        return Err(Error::Intervention(
                            "Host runtime requires --allow-host-fallback".into(),
                        ));
                    }
                    ToolRouter::new(registry, HostMicroSandboxProvider::trusted_development())
                        .execute(&reality_record, &manifest, tool, input, &[])
                        .await?
                }
                ToolRuntimeArg::Container => {
                    let image = reality_record
                        .execution_boundary
                        .image_digest
                        .as_deref()
                        .unwrap_or(crate::capability::DEFAULT_CONTAINER_IMAGE);
                    let provider = ContainerMicroSandboxProvider::new("docker", image)?;
                    ToolRouter::new(registry, provider)
                        .execute(&reality_record, &manifest, tool, input, &[])
                        .await?
                }
            };
            store.insert_micro_sandbox(&run.sandbox)?;
            for event in &run.lifecycle {
                store.insert_tool_lifecycle_event(event)?;
            }
            store.insert_execution_attestation(&run.attestation)?;
            Ok(
                json!({"sandbox":run.sandbox,"result":run.result,"receipt":run.receipt,"attestation":run.attestation}),
            )
        }
    }
}
