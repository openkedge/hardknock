// SPDX-License-Identifier: Apache-2.0
use crate::{Result, core::*, plan::*, runtime::RuntimeDecisionContext, store::Store};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    List,
    Replan {
        run: PlanRunId,
    },
    Validate {
        id: ExecutionPlanId,
        #[arg(long)]
        skill: String,
        #[arg(long)]
        requests: PathBuf,
        #[arg(long)]
        budget: PathBuf,
    },
    Forecast {
        run: PlanRunId,
    },
    Import {
        file: PathBuf,
    },
    Show {
        id: ExecutionPlanId,
    },
    Inspect {
        id: ExecutionPlanId,
    },
    Assumptions {
        id: ExecutionPlanId,
    },
    Checkpoints {
        id: ExecutionPlanId,
    },
    History {
        id: ExecutionPlanId,
    },
    Start {
        id: ExecutionPlanId,
    },
    Status {
        run: PlanRunId,
    },
    Why {
        run: PlanRunId,
        #[arg(long)]
        context: PathBuf,
    },
    Replay {
        run: PlanRunId,
    },
    Diff {
        id: ExecutionPlanId,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
    },
    Checkpoint {
        run: PlanRunId,
        checkpoint: PlanCheckpointId,
        #[arg(long)]
        context: PathBuf,
    },
    Resume {
        run: PlanRunId,
        snapshot: PlanCheckpointSnapshotId,
        #[arg(long)]
        context: PathBuf,
    },
    Recovery {
        run: PlanRunId,
    },
}
pub async fn execute(
    command: &PlanCommand,
    store: &Store,
    cancel: &crate::cancellation::Cancellation,
) -> Result<Value> {
    fn context(path: &PathBuf) -> Result<RuntimeDecisionContext> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    Ok(match command {
        PlanCommand::Validate {
            id,
            skill,
            requests,
            budget,
        } => {
            let p = store.execution_plan(id)?;
            let requests = serde_json::from_slice::<Vec<crate::experimentation::ExperimentRequest>>(
                &fs::read(requests)?,
            )?;
            let budget = serde_json::from_slice(&fs::read(budget)?)?;
            let curriculum = store.compile_plan_curriculum(
                &PlanRevisionRef {
                    plan: id.clone(),
                    revision: p.revision,
                },
                skill,
                &requests,
                crate::curriculum::CurriculumGoalKind::ValidatePlan,
                &budget,
            )?;
            let config = crate::bridge::config::Config::load(&store.home)?;
            json!(
                crate::curriculum::CurriculumExecutor {
                    store,
                    config: &config
                }
                .run(&curriculum.id, cancel)
                .await?
            )
        }
        PlanCommand::Forecast { run } => json!(store.plan_forecasts(run)?),
        PlanCommand::Replan { run } => {
            json!(store.replan_run(run, PlanRevisionReason::UserChange)?)
        }
        PlanCommand::List => json!({"plans":store.execution_plans()?}),
        PlanCommand::Import { file } => json!(store.save_execution_plan(
            &serde_json::from_slice(&fs::read(file)?)?,
            PlanRevisionReason::UserChange
        )?),
        PlanCommand::Show { id } => json!(store.execution_plan(id)?),
        PlanCommand::Inspect { id } => {
            let plan = store.execution_plan(id)?;
            json!({"dependencies":validate_plan(&plan)?,"plan":plan})
        }
        PlanCommand::Assumptions { id } => json!(store.execution_plan(id)?.assumptions),
        PlanCommand::Checkpoints { id } => json!(store.execution_plan(id)?.checkpoints),
        PlanCommand::History { id } => json!(store.plan_history(id)?),
        PlanCommand::Start { id } => json!(store.start_plan_run(id)?),
        PlanCommand::Status { run } => json!(store.plan_run(run)?),
        PlanCommand::Why { run, context: path } => {
            json!(store.assess_plan_run(run, &context(path)?, true)?)
        }
        PlanCommand::Replay { run } => store.plan_replay(run)?,
        PlanCommand::Diff { id, from, to } => {
            json!({"from":store.plan_revision(id,*from)?,"to":store.plan_revision(id,*to)?})
        }
        PlanCommand::Checkpoint {
            run,
            checkpoint,
            context: path,
        } => json!(store.capture_plan_checkpoint(run, checkpoint, &context(path)?)?),
        PlanCommand::Resume {
            run,
            snapshot,
            context: path,
        } => json!(store.resume_plan_checkpoint(run, snapshot, &context(path)?)?),
        PlanCommand::Recovery { run } => json!(store.plan_recovery_context(run)?),
    })
}
