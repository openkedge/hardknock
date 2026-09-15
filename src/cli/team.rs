// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, store::Store, team::AgentTeam};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    List,
    Import { file: PathBuf },
    Show { id: AgentTeamId },
    Inspect { id: AgentTeamId },
    Roles { id: AgentTeamId },
    Authority { id: AgentTeamId },
    History { id: AgentTeamId },
}
#[derive(Debug, Subcommand)]
pub enum DelegationCommand {
    List {
        team: AgentTeamId,
    },
    Import {
        file: PathBuf,
    },
    Show {
        team: AgentTeamId,
        id: DelegationId,
    },
    Validate {
        team: AgentTeamId,
        id: DelegationId,
    },
    Revoke {
        id: DelegationId,
        #[arg(long)]
        reason: String,
    },
}
pub fn execute(command: &TeamCommand, store: &Store) -> Result<Value> {
    Ok(match command {
        TeamCommand::List => json!(store.agent_teams()?),
        TeamCommand::Import { file } => {
            let team: AgentTeam = serde_json::from_slice(&fs::read(file)?)?;
            store.save_agent_team(&team)?;
            json!(team)
        }
        TeamCommand::Show { id } | TeamCommand::Inspect { id } => json!(store.agent_team(id)?),
        TeamCommand::Roles { id } => {
            let t = store.agent_team(id)?;
            json!({"roles":t.roles,"assignments":t.role_assignments})
        }
        TeamCommand::Authority { id } => {
            let t = store.agent_team(id)?;
            json!({"team_ceiling":t.authority,"roles":t.roles,"delegation_depth":t.max_delegation_depth,"external_authority_required":true})
        }
        TeamCommand::History { id } => json!(store.team_history(id)?),
    })
}
pub fn delegation(command: &DelegationCommand, store: &Store) -> Result<Value> {
    Ok(match command {
        DelegationCommand::List { team } => json!(store.team_delegations(team)?),
        DelegationCommand::Import { file } => {
            let d = serde_json::from_slice(&fs::read(file)?)?;
            store.record_delegation(&d)?;
            json!(d)
        }
        DelegationCommand::Show { team, id } => json!(
            store
                .team_delegations(team)?
                .get(id)
                .ok_or_else(|| crate::Error::InvalidInput("Unknown delegation".into()))?
        ),
        DelegationCommand::Validate { team, id } => {
            json!({"authority":store.validate_delegation(team,id,chrono::Utc::now())?,"external_authority_required":true})
        }
        DelegationCommand::Revoke { id, reason } => {
            store.revoke_delegation(id, reason)?;
            json!({"revoked":id})
        }
    })
}
