// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, store::Store, team::AgentTeam};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    Handoff {
        #[command(subcommand)]
        command: HandoffCommand,
    },
    List,
    Import {
        file: PathBuf,
    },
    Show {
        id: AgentTeamId,
    },
    Inspect {
        id: AgentTeamId,
    },
    Roles {
        id: AgentTeamId,
    },
    Authority {
        id: AgentTeamId,
    },
    History {
        id: AgentTeamId,
    },
    Diversity {
        review: TeamReviewId,
    },
    CommonMode {
        review: TeamReviewId,
    },
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
        TeamCommand::Handoff { command } => handoff(command, store)?,
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
        TeamCommand::Diversity { review } | TeamCommand::CommonMode { review } => {
            json!(store.team_evidence(review)?)
        }
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

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    Target {
        #[arg(long)]
        context: PathBuf,
    },
    Create {
        file: PathBuf,
    },
    Show {
        id: TeamReviewId,
    },
    Findings {
        id: TeamReviewId,
    },
    Contribute {
        file: PathBuf,
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        findings: Option<PathBuf>,
    },
    ResolveFinding {
        file: PathBuf,
    },
    Assess {
        id: TeamReviewId,
        #[arg(long)]
        context: PathBuf,
    },
}
pub fn review(command: &ReviewCommand, store: &Store) -> Result<Value> {
    Ok(match command {
        ReviewCommand::Target { context } => {
            json!({"action_hash":crate::team::review_action_hash(&serde_json::from_slice(&fs::read(context)?)?)?})
        }
        ReviewCommand::Create { file } => {
            json!(store.create_team_review(&serde_json::from_slice(&fs::read(file)?)?)?)
        }
        ReviewCommand::Show { id } => {
            json!({"review":store.team_review(id)?,"contributions":store.team_contributions(id)?,"evidence":store.team_evidence(id)?})
        }
        ReviewCommand::Findings { id } => json!(store.review_findings(id)?),
        ReviewCommand::Contribute {
            file,
            context,
            findings,
        } => {
            let findings = if let Some(path) = findings {
                serde_json::from_slice(&fs::read(path)?)?
            } else {
                Vec::new()
            };
            json!(store.record_team_contribution(
                &serde_json::from_slice(&fs::read(file)?)?,
                &findings,
                &serde_json::from_slice(&fs::read(context)?)?
            )?)
        }
        ReviewCommand::ResolveFinding { file } => {
            json!(store.resolve_review_finding(&serde_json::from_slice(&fs::read(file)?)?)?)
        }
        ReviewCommand::Assess { id, context } => {
            json!(store.assess_team_review(id, &serde_json::from_slice(&fs::read(context)?)?)?)
        }
    })
}

#[derive(Debug, Subcommand)]
pub enum HandoffCommand {
    Create {
        file: PathBuf,
        #[arg(long)]
        context: PathBuf,
    },
    Show {
        id: AgentHandoffId,
    },
    Receive {
        id: AgentHandoffId,
        #[arg(long)]
        context: PathBuf,
    },
}
fn handoff(command: &HandoffCommand, store: &Store) -> Result<Value> {
    Ok(match command {
        HandoffCommand::Create { file, context } => json!(store.create_agent_handoff(
            &serde_json::from_slice(&fs::read(file)?)?,
            &serde_json::from_slice(&fs::read(context)?)?
        )?),
        HandoffCommand::Show { id } => json!(store.agent_handoff(id)?),
        HandoffCommand::Receive { id, context } => {
            json!(store.receive_agent_handoff(id, &serde_json::from_slice(&fs::read(context)?)?)?)
        }
    })
}
