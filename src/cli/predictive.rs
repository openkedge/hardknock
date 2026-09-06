// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands};
use crate::{
    Error, Result,
    core::*,
    predictive::*,
    store::{NewTrajectory, NewTrajectoryEvent, Store},
};
use chrono::Utc;
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

#[derive(Debug, Subcommand)]
pub enum TrajectoryCommand {
    List,
    Show {
        id: TrajectoryId,
    },
    Compare {
        left: TrajectoryId,
        right: TrajectoryId,
    },
    /// Replays normalized observations only; it never re-executes an action or effect.
    Replay {
        id: TrajectoryId,
    },
    Start {
        #[arg(long)]
        spec: PathBuf,
    },
    Event {
        id: TrajectoryId,
        #[arg(long)]
        spec: PathBuf,
    },
    Finish {
        id: TrajectoryId,
        #[arg(long)]
        outcome: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ForecastCommand {
    List,
    Active,
    Show {
        id: FailureForecastId,
    },
    Explain {
        id: FailureForecastId,
    },
    Test {
        id: EarlyWarningSignatureId,
    },
    Backtest {
        id: EarlyWarningSignatureId,
    },
    Gaps,
    Audit {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Replay {
        id: FailureForecastId,
    },
    Quality,
    Impact {
        id: EarlyWarningSignatureId,
    },
    Forecastability {
        failure: String,
    },
    Discover {
        failure: String,
    },
    Curriculum,
    Localize {
        id: EarlyWarningSignatureId,
    },
    Register {
        #[arg(long)]
        spec: PathBuf,
    },
    Validate {
        id: EarlyWarningSignatureId,
        #[arg(long, value_delimiter = ',')]
        positive: Vec<TrajectoryId>,
        #[arg(long, value_delimiter = ',')]
        negative: Vec<TrajectoryId>,
    },
    Predict {
        trajectory: TrajectoryId,
    },
    Benchmark {
        #[arg(long)]
        trusted_local: bool,
    },
}

fn read<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn explain_forecast(store: &Store, id: &FailureForecastId) -> Result<Value> {
    let forecast = store.forecast(id)?;
    let signature = store.warning_signature(&forecast.signature)?;
    Ok(
        json!({"forecast":forecast,"why":{"early_warning_signature":signature.id,"failure_trajectory":forecast.matched_trajectory,"matched_signals":forecast.matched_signals,"missing_signals":forecast.missing_signals,"ordered_conditions":signature.ordered_conditions,"historical_matching_trajectories":forecast.historical_matches,"causal_basis":forecast.causal_basis,"evidence_kind":forecast.evidence_kind,"recommended_interventions":forecast.recommended_interventions,"matcher_version":forecast.matcher_version,"policy_version":forecast.policy_version},"caveat":"Strength and lifecycle status are qualitative and scoped; precision is not a probability of this failure."}),
    )
}

fn replay_trajectory(store: &Store, original: &ExecutionTrajectory) -> Result<ExecutionTrajectory> {
    let replay = store.start_trajectory(NewTrajectory {
        session_id: HardknockSessionId::new(),
        subject: Some(original.subject.clone()),
        task_family: original.task_family.clone(),
        context: original.context.clone(),
    })?;
    for event in store.trajectory_events(&original.id)? {
        // Historical replay stops at the prediction boundary. Copying the terminal
        // failure would turn an early warning into a post-hoc explanation.
        if event.kind == TrajectoryEventKind::FailureObserved {
            break;
        }
        store.append_trajectory_event(
            &replay.id,
            NewTrajectoryEvent {
                kind: event.kind,
                observation: event.observation,
                evidence: vec![TrajectoryEvidenceRef::Trajectory(original.id.clone())],
            },
        )?;
    }
    store.trajectory(&replay.id)
}

pub async fn execute(
    cli: &Cli,
    store: &Store,
    cancel: &crate::cancellation::Cancellation,
) -> Result<Value> {
    match &cli.command {
        Commands::Trajectory { command } => match command {
            TrajectoryCommand::List => Ok(json!({"trajectories":store.trajectories()?})),
            TrajectoryCommand::Show { id } => Ok(json!({
                "trajectory":store.trajectory(id)?,
                "events":store.trajectory_events(id)?,
                "forecasts":store.forecasts()?.into_iter().filter(|item|&item.trajectory_id==id).collect::<Vec<_>>()
            })),
            TrajectoryCommand::Compare { left, right } => {
                let a = store.trajectory(left)?;
                let b = store.trajectory(right)?;
                let prefix = a
                    .fingerprint
                    .event_kinds
                    .iter()
                    .zip(&b.fingerprint.event_kinds)
                    .take_while(|(x, y)| x == y)
                    .count();
                Ok(
                    json!({"left":a,"right":b,"shared_ordered_prefix":prefix,"same_structure":a.fingerprint.hash==b.fingerprint.hash,"claim":"Structural comparison only; no global similarity percentage"}),
                )
            }
            TrajectoryCommand::Replay { id } => {
                let original = store.trajectory(id)?;
                let replay = replay_trajectory(store, &original)?;
                Ok(
                    json!({"trajectory":replay,"forecasts":store.forecast_trajectory(&replay.id)?,"notice":"Normalized observations replayed; no command or external effect executed"}),
                )
            }
            TrajectoryCommand::Start { spec } => {
                Ok(json!({"trajectory":store.start_trajectory(read(spec)?)?}))
            }
            TrajectoryCommand::Event { id, spec } => {
                Ok(json!({"event":store.append_trajectory_event(id,read(spec)?)?}))
            }
            TrajectoryCommand::Finish { id, outcome } => {
                Ok(json!({"trajectory":store.finish_trajectory(id,read(outcome)?)?}))
            }
        },
        Commands::Forecast { command } => match command {
            ForecastCommand::List => {
                Ok(json!({"forecasts":store.forecasts()?,"signatures":store.warning_signatures()?}))
            }
            ForecastCommand::Active => Ok(json!({"forecasts":store.active_forecasts()?})),
            ForecastCommand::Show { id } => {
                let forecast = store.forecast(id)?;
                Ok(
                    json!({"forecast":forecast,"signature":store.warning_signature(&forecast.signature)?,"health":store.forecast_health(&forecast.signature)?,"preventive_interventions":store.preventive_interventions()?.into_iter().filter(|item|item.signature==forecast.signature).collect::<Vec<_>>() }),
                )
            }
            ForecastCommand::Explain { id } => explain_forecast(store, id),
            ForecastCommand::Test { id } | ForecastCommand::Backtest { id } => {
                store.backtest_warning(id)
            }
            ForecastCommand::Gaps => store.forecast_gaps(),
            ForecastCommand::Audit { limit } => store.forecast_audit(*limit),
            ForecastCommand::Replay { id } => {
                let old = store.forecast(id)?;
                let original = store.trajectory(&old.trajectory_id)?;
                let replay = replay_trajectory(store, &original)?;
                Ok(
                    json!({"source_forecast":old.id,"trajectory":replay,"forecasts":store.forecast_trajectory(&replay.id)?}),
                )
            }
            ForecastCommand::Quality => Ok(
                json!({"quality":store.forecast_quality()?,"summary":store.predictive_summary()?,"misses":store.forecast_misses()?}),
            ),
            ForecastCommand::Impact { id } => store.predictive_impact(id),
            ForecastCommand::Forecastability { failure } => {
                Ok(json!({"failure":failure,"forecastability":store.forecastability(failure)?}))
            }
            ForecastCommand::Discover { failure } => Ok(json!({
                "failure":failure,
                "candidate_risk_indicators":store.discover_candidate_risk_indicators(failure)?,
                "notice":"Candidates are inactive until an early-warning signature is separately validated"
            })),
            ForecastCommand::Curriculum => {
                Ok(json!({"goals":store.predictive_curriculum_goals()?}))
            }
            ForecastCommand::Localize { id } => Ok(
                json!({"signature":store.localize_federated_signature(id)?,"automatic_intervention":false}),
            ),
            ForecastCommand::Register { spec } => {
                let mut signature: EarlyWarningSignature = read(spec)?;
                signature.created_at = Utc::now();
                signature.updated_at = signature.created_at;
                Ok(json!({"signature":store.register_warning_signature(signature)?}))
            }
            ForecastCommand::Validate {
                id,
                positive,
                negative,
            } => Ok(json!({"signature":store.validate_warning_signature(id,positive,negative)?})),
            ForecastCommand::Predict { trajectory } => {
                Ok(json!({"forecasts":store.forecast_trajectory(trajectory)?}))
            }
            ForecastCommand::Benchmark { trusted_local } => {
                if !trusted_local {
                    return Err(Error::Intervention("Predictive benchmark executes reviewed local fixture commands in Git worktrees; pass --trusted-local".into()));
                }
                let config = crate::bridge::config::Config::load(&store.home)?;
                benchmark::run(store, &config, &cli.repo, cancel).await
            }
        },
        Commands::Provenance { object } if object.starts_with("forecast-") => {
            Ok(store.forecast_provenance(&object.parse()?)?)
        }
        Commands::Provenance { object } if object.starts_with("early-warning-") => {
            Ok(store.predictive_impact(&object.parse()?)?)
        }
        Commands::Why {
            forecast: Some(id), ..
        } => explain_forecast(store, id),
        _ => Err(Error::InvalidInput(
            "Expected trajectory or forecast command".into(),
        )),
    }
}
