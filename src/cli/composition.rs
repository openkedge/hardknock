// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, cancellation::Cancellation, composition::*, core::*, store::Store};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum ComposeCommand {
    List,
    Proof {
        id: String,
        #[arg(long)]
        repo: PathBuf,
    },
    Import {
        file: PathBuf,
    },
    Show {
        id: String,
    },
    Inspect {
        id: String,
        #[arg(long)]
        context: Option<PathBuf>,
    },
    Test {
        id: String,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        trusted_host: bool,
    },
    Validate {
        id: String,
        #[arg(long = "request", required = true)]
        requests: Vec<PathBuf>,
        #[arg(long)]
        trusted_host: bool,
    },
    Why {
        id: String,
    },
    Recovery {
        id: String,
    },
    Gaps {
        id: String,
    },
    History {
        id: String,
    },
    Diff {
        id: String,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
    },
    Pin {
        #[arg(long)]
        skill: Option<SkillId>,
        #[arg(long, conflicts_with = "skill")]
        tool: Option<ToolId>,
        #[arg(long,conflicts_with_all=["tool","skill"])]
        recovery: Option<RecoveryId>,
    },
}
pub async fn execute(
    command: &ComposeCommand,
    store: &Store,
    cancel: &Cancellation,
) -> Result<Value> {
    match command {
        ComposeCommand::Proof { id, repo } => {
            let c = store.composition(id)?;
            Ok(
                json!({"starting_state":composition_starting_proof(&c, &crate::dojo::capture_state(repo)?)?}),
            )
        }
        ComposeCommand::List => Ok(json!({"compositions":store.compositions()?})),
        ComposeCommand::Import { file } => {
            let c: Composition = serde_json::from_slice(&fs::read(file)?)?;
            store.save_composition(&c)?;
            Ok(json!({"composition":c,"imported":true}))
        }
        ComposeCommand::Pin {
            skill,
            tool,
            recovery,
        } => {
            let component = match (skill, tool, recovery) {
                (Some(id), None, None) => ComposableArtifactRef::Skill(id.clone()),
                (None, Some(id), None) => ComposableArtifactRef::Tool(id.clone()),
                (None, None, Some(id)) => ComposableArtifactRef::Recovery(id.clone()),
                _ => {
                    return Err(Error::InvalidInput(
                        "Specify one executable component".into(),
                    ));
                }
            };
            Ok(json!({"component":store.pin_composition_component(&component)?}))
        }
        ComposeCommand::Show { id } => {
            let c = store.composition(id)?;
            Ok(
                json!({"composition":c,"dependency_health":store.composition_dependency_health(&c)?,"empirical_maturity":store.composition_maturity(&c)?}),
            )
        }
        ComposeCommand::Inspect { id, context } => {
            let c = store.composition(id)?;
            let context = if let Some(file) = context {
                crate::hierarchy::KnowledgeContext::from_json(serde_json::from_slice(&fs::read(
                    file,
                )?)?)?
            } else {
                Default::default()
            };
            Ok(json!({"preflight":DefaultCompositionPreflightAnalyzer.analyze(&c,&context)?}))
        }
        ComposeCommand::Why { id } | ComposeCommand::Gaps { id } => {
            let c = store.composition(id)?;
            Ok(
                json!({"assurance":store.composition_assurance(&c)?,"interactions":store.composition_interactions(&c.id)?,"health":store.composition_dependency_health(&c)?}),
            )
        }
        ComposeCommand::Recovery { id } => {
            let c = store.composition(id)?;
            Ok(json!({"recovery_plans":c.recoveries,"evidence":store.composition_evidence(&c.id)?}))
        }
        ComposeCommand::History { id } => {
            let c = store.composition(id)?;
            Ok(json!({"history":store.composition_history(&c.id)?}))
        }
        ComposeCommand::Diff { id, from, to } => {
            let c = store.composition(id)?;
            Ok(
                json!({"from":store.historical_composition(&c.id,*from)?,"to":store.historical_composition(&c.id,*to)?}),
            )
        }
        ComposeCommand::Test {
            id,
            request,
            trusted_host,
        } => run(store, id, request, *trusted_host, cancel).await,
        ComposeCommand::Validate {
            id,
            requests,
            trusted_host,
        } => {
            let mut results = vec![];
            for request in requests {
                results.push(run(store, id, request, *trusted_host, cancel).await?);
            }
            let c = store.composition(id)?;
            let assurance = store.composition_assurance(&c)?;
            let composite = if assurance["satisfied"] == true {
                Some(store.promote_composite_skill(&c.id)?)
            } else {
                None
            };
            Ok(json!({"results":results,"assurance":assurance,"composite_skill":composite}))
        }
    }
}
async fn run(
    store: &Store,
    id: &str,
    file: &PathBuf,
    host: bool,
    cancel: &Cancellation,
) -> Result<Value> {
    let c = store.composition(id)?;
    let request: CompositionExperimentRequest = serde_json::from_slice(&fs::read(file)?)?;
    if request.composition != c.id || request.revision != c.revision {
        return Err(Error::InvalidInput(
            "Request does not bind the selected current composition".into(),
        ));
    }
    let evidence = if host {
        CompositionExperimentEngine::new(
            store,
            crate::tool_runtime::HostMicroSandboxProvider::trusted_development(),
        )?
        .run(&request, cancel)
        .await?
    } else {
        CompositionExperimentEngine::new(
            store,
            crate::tool_runtime::ContainerMicroSandboxProvider::new("docker", "alpine:3.20")?,
        )?
        .run(&request, cancel)
        .await?
    };
    Ok(json!({"evidence":evidence,"production_effects_committed":false}))
}
