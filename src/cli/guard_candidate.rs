// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, knowledge_runtime::*, store::Store};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum GuardCandidateCommand {
    List,
    Show {
        id: GuardRevisionCandidateId,
    },
    Export {
        id: GuardRevisionCandidateId,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Verify {
        file: PathBuf,
    },
    Generate {
        hierarchy: KnowledgeHierarchyId,
        node: KnowledgeNodeId,
        #[arg(long, requires = "guard_revision")]
        guard: Option<String>,
        #[arg(long, requires = "guard")]
        guard_revision: Option<String>,
    },
}
pub fn execute(command: &GuardCandidateCommand, store: &Store) -> Result<Value> {
    match command {
        GuardCandidateCommand::List => {
            Ok(json!({"candidates":store.guard_candidates()?,"enforcement_changed":false}))
        }
        GuardCandidateCommand::Show { id } => {
            Ok(json!({"candidate":store.guard_candidate(id)?,"enforcement_changed":false}))
        }
        GuardCandidateCommand::Export { id, output } => {
            let artifact = GuardRevisionArtifact::export(store.guard_candidate(id)?)?;
            artifact.verify()?;
            if let Some(path) = output {
                fs::write(path, serde_json::to_vec_pretty(&artifact)?)?;
            }
            store.knowledge_event("guard_revision_candidate_exported", id)?;
            Ok(serde_json::to_value(artifact)?)
        }
        GuardCandidateCommand::Verify { file } => {
            let artifact: GuardRevisionArtifact = serde_json::from_slice(&fs::read(file)?)?;
            artifact.verify()?;
            Ok(json!({"valid":true,"schema":artifact.schema,"enforcement_changed":false}))
        }
        GuardCandidateCommand::Generate {
            hierarchy,
            node,
            guard,
            guard_revision,
        } => {
            let h = store.knowledge_hierarchy(hierarchy)?;
            let candidate = guard_revision_candidate(
                &h,
                node,
                guard.as_ref().map(|id| GuardRef {
                    id: id.clone(),
                    revision: guard_revision
                        .clone()
                        .unwrap_or_else(|| "unspecified".into()),
                }),
            )?;
            store.save_guard_candidate(&candidate)?;
            Ok(json!({"candidate":candidate,"enforcement_changed":false}))
        }
    }
}
