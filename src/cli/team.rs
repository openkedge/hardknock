// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, store::Store, team::*};
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
    GovernanceImport {
        file: PathBuf,
    },
    GovernanceShow {
        id: AgentTeamId,
        revision: u64,
    },
    Epistemic {
        id: AgentTeamId,
    },
    Formation {
        id: AgentTeamId,
        roles: PathBuf,
        #[arg(long, default_value = "moderate")]
        minimum: String,
    },
    Challenge {
        #[command(subcommand)]
        command: ChallengeCommand,
    },
    Responsibility {
        file: PathBuf,
    },
    Reassign {
        file: PathBuf,
    },
    RecoveryHandoff {
        file: PathBuf,
        #[arg(long)]
        context: PathBuf,
    },
    Assurance {
        #[arg(long)]
        context: PathBuf,
        #[arg(long, default_value = "basic")]
        profile: String,
    },
    Why {
        id: AgentTeamId,
    },
    Benchmark,
}
#[derive(Debug, Subcommand)]
pub enum ChallengeCommand {
    Assign {
        file: PathBuf,
    },
    Complete {
        id: ChallengeAssignmentId,
        contribution: AgentContributionId,
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        tokens: u64,
        #[arg(long)]
        latency_ms: u64,
    },
    List {
        team: AgentTeamId,
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
        TeamCommand::GovernanceImport { file } => {
            let governance: TeamGovernance = serde_json::from_slice(&fs::read(file)?)?;
            store.save_team_governance(&governance)?;
            json!(governance)
        }
        TeamCommand::GovernanceShow { id, revision } => {
            json!(store.team_governance(id, *revision)?)
        }
        TeamCommand::Epistemic { id } => json!(store.team_epistemic_profile(id)?),
        TeamCommand::Formation { id, roles, minimum } => {
            let roles = serde_json::from_slice(&fs::read(roles)?)?;
            let minimum = match minimum.as_str() {
                "unknown" => crate::epistemic::DiversityClass::Unknown,
                "low" => crate::epistemic::DiversityClass::Low,
                "moderate" => crate::epistemic::DiversityClass::Moderate,
                "high" => crate::epistemic::DiversityClass::High,
                _ => {
                    return Err(crate::Error::InvalidInput(
                        "minimum must be unknown|low|moderate|high".into(),
                    ));
                }
            };
            json!(store.assess_team_formation(id, &roles, minimum)?)
        }
        TeamCommand::Challenge { command } => challenge(command, store)?,
        TeamCommand::Responsibility { file } => {
            let value: ResponsibilityAssignment = serde_json::from_slice(&fs::read(file)?)?;
            store.assign_responsibility(&value)?;
            json!(value)
        }
        TeamCommand::Reassign { file } => {
            json!(store.reassign_team_role(&serde_json::from_slice(&fs::read(file)?)?)?)
        }
        TeamCommand::RecoveryHandoff { file, context } => {
            let value: TeamRecoveryHandoff = serde_json::from_slice(&fs::read(file)?)?;
            store.record_team_recovery_handoff(
                &value,
                &serde_json::from_slice(&fs::read(context)?)?,
            )?;
            json!(value)
        }
        TeamCommand::Assurance { context, profile } => {
            let profile = match profile.as_str() {
                "basic" => TeamAssuranceProfile::TeamAssuranceBasicV1,
                "diversity" => TeamAssuranceProfile::TeamEpistemicDiversityV1,
                _ => {
                    return Err(crate::Error::InvalidInput(
                        "profile must be basic|diversity".into(),
                    ));
                }
            };
            json!(
                store.assess_team_assurance(
                    profile,
                    &serde_json::from_slice(&fs::read(context)?)?
                )?
            )
        }
        TeamCommand::Why { id } => {
            json!({"team":store.agent_team(id)?,"epistemic":store.team_epistemic_profile(id)?,"responsibilities":store.team_records::<ResponsibilityAssignment>(id,"responsibility_assigned")?,"challenges":store.team_records::<ChallengeCompletion>(id,"challenge_completed")?,"violations":store.team_records::<RoleViolation>(id,"role_violation_attempted")?,"guard_recommendations":store.team_guard_recommendations(id)?,"history":store.team_history(id)?})
        }
        TeamCommand::Benchmark => json!(run_flagship_team_benchmark()),
    })
}
fn challenge(command: &ChallengeCommand, store: &Store) -> Result<Value> {
    Ok(match command {
        ChallengeCommand::Assign { file } => {
            json!(store.assign_team_challenge(&serde_json::from_slice(&fs::read(file)?)?)?)
        }
        ChallengeCommand::Complete {
            id,
            contribution,
            context,
            tokens,
            latency_ms,
        } => json!(store.complete_team_challenge(
            id,
            contribution,
            &serde_json::from_slice(&fs::read(context)?)?,
            *tokens,
            *latency_ms
        )?),
        ChallengeCommand::List { team } => {
            json!({"assigned":store.team_records::<ChallengeAssignment>(team,"challenge_assigned")?,"completed":store.team_records::<ChallengeCompletion>(team,"challenge_completed")?})
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
