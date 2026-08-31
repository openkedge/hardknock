// SPDX-License-Identifier: Apache-2.0

use super::{Cli, Commands};
use crate::{
    Error, Result,
    core::ExecutionAttestationId,
    store::{CapabilityStore, Store, ToolStore},
};
use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Debug, Subcommand)]
pub enum AttestationCommand {
    List,
    Show { id: ExecutionAttestationId },
    Verify { id: ExecutionAttestationId },
    Replay { id: ExecutionAttestationId },
}

pub fn handles(command: &Commands) -> bool {
    matches!(command, Commands::Attestation { .. })
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    let Commands::Attestation { command } = &cli.command else {
        return Err(Error::InvalidInput("Attestation dispatch failed".into()));
    };
    match command {
        AttestationCommand::List => Ok(json!({"attestations":store.execution_attestations(None)?})),
        AttestationCommand::Show { id } => {
            Ok(serde_json::to_value(store.execution_attestation(id)?)?)
        }
        AttestationCommand::Verify { id } => {
            let attestation = store.execution_attestation(id)?;
            let tool = attestation.tool.id.clone();
            let definition = store.tool_definition(&tool).ok();
            let reality_hash = store
                .effective_capability_manifest(&attestation.reality_id)
                .ok()
                .and_then(|manifest| manifest.hash().ok());
            Ok(serde_json::to_value(
                attestation.verify(definition.as_ref(), reality_hash.as_deref())?,
            )?)
        }
        AttestationCommand::Replay { id } => {
            let attestation = store.execution_attestation(id)?;
            Ok(
                json!({"attestation_id":id,"outcome":"replay_requires_input_artifacts","replayed":false,"reason":"The original invocation intentionally stores input hashes rather than potentially sensitive input values; provide the original input artifact to a future replay runner.","original_result":attestation.result}),
            )
        }
    }
}
