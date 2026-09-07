// SPDX-License-Identifier: Apache-2.0

use super::{Cli, Commands, ExperienceCommand};
use crate::{
    Error, Result,
    budget::ExperienceBudget,
    core::{ExperienceOpportunityId, ExperiencePortfolioId},
    economics::{ExperienceOpportunityStatus, ExperiencePortfolioObjective, benchmark},
    effects::EffectRisk,
    store::Store,
};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ObjectiveArg {
    Balanced,
    Resilience,
    Assurance,
    Research,
    Efficiency,
}

impl From<ObjectiveArg> for ExperiencePortfolioObjective {
    fn from(value: ObjectiveArg) -> Self {
        match value {
            ObjectiveArg::Balanced => Self::Balanced,
            ObjectiveArg::Resilience => Self::Resilience,
            ObjectiveArg::Assurance => Self::Assurance,
            ObjectiveArg::Research => Self::Research,
            ObjectiveArg::Efficiency => Self::Efficiency,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EffectRiskArg {
    ReadOnly,
    Low,
    Medium,
    High,
    Critical,
}

impl From<EffectRiskArg> for EffectRisk {
    fn from(value: EffectRiskArg) -> Self {
        match value {
            EffectRiskArg::ReadOnly => Self::ReadOnly,
            EffectRiskArg::Low => Self::Low,
            EffectRiskArg::Medium => Self::Medium,
            EffectRiskArg::High => Self::High,
            EffectRiskArg::Critical => Self::Critical,
        }
    }
}

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[arg(long, default_value_t = 8)]
    budget_trials: usize,
    #[arg(long, default_value_t = 3)]
    max_agent_runs: usize,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    max_duration_minutes: u64,
    #[arg(long, default_value_t = 1)]
    max_parallel_trials: usize,
    #[arg(long, default_value_t = 0)]
    max_human_approvals: usize,
    #[arg(long, value_enum, default_value = "read-only")]
    allowed_effect_risk: EffectRiskArg,
    #[arg(long, value_enum, default_value = "balanced")]
    objective: ObjectiveArg,
}

#[derive(Debug, Subcommand)]
pub enum ExploreCommand {
    /// Generate and persist a deterministic bounded acquisition portfolio.
    Plan(PlanArgs),
    /// Compile the latest or named portfolio into existing learning engines.
    Run {
        portfolio: Option<ExperiencePortfolioId>,
    },
    /// Show current opportunity and portfolio state.
    Status,
    /// Show one opportunity and its latest selection or deferral decision.
    Show {
        opportunity: ExperienceOpportunityId,
    },
    /// Explain why an opportunity was selected or deferred.
    Why {
        opportunity: ExperienceOpportunityId,
    },
    /// Report estimated value, actual outcomes, costs, and experience debt.
    Report,
    /// Show immutable portfolio revisions and economics events.
    History {
        portfolio: Option<ExperiencePortfolioId>,
    },
    /// Recompute a portfolio without mutating its recorded history.
    Replay { portfolio: ExperiencePortfolioId },
    /// Run the deterministic 20-trial, 5-agent comparative benchmark.
    Benchmark,
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Explore { .. }
            | Commands::Experience {
                command: ExperienceCommand::Debt
            }
    )
}

fn latest(store: &Store) -> Result<crate::economics::ExperiencePortfolio> {
    store.latest_experience_portfolio()?.ok_or_else(|| {
        Error::InvalidInput("No experience portfolio exists; run `hardknock explore plan`".into())
    })
}

fn opportunity_detail(store: &Store, id: &ExperienceOpportunityId) -> Result<Value> {
    let opportunity = store.experience_opportunity(id)?;
    let mut decisions = Vec::new();
    for portfolio in store.experience_portfolios()? {
        for revision in store.experience_portfolio_history(&portfolio.id)? {
            if let Some(item) = revision
                .selected
                .iter()
                .find(|item| &item.opportunity == id)
            {
                decisions.push(json!({"portfolio":revision.id,"revision":revision.revision,"decision":"selected","priority":item.priority,"category":item.category,"reasons":item.reasons,"reserved_cost":item.reserved_cost}));
            } else if let Some(item) = revision
                .deferred
                .iter()
                .find(|item| &item.opportunity == id)
            {
                decisions.push(json!({"portfolio":revision.id,"revision":revision.revision,"decision":"deferred","reasons":item.reasons}));
            }
        }
    }
    let actual_result = match store.opportunity_result(id) {
        Ok(result) => Some(result),
        Err(Error::NotFound(_)) => None,
        Err(error) => return Err(error),
    };
    Ok(json!({
        "kind":"experience_opportunity_explanation",
        "opportunity":opportunity,
        "actual_result":actual_result,
        "latest_portfolio_decision":decisions.last(),
        "portfolio_history":decisions,
        "dimensions_are_not_collapsed":true
    }))
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    match &cli.command {
        Commands::Explore { command } => match command {
            ExploreCommand::Plan(args) => {
                if args.budget_trials == 0 || args.max_parallel_trials == 0 {
                    return Err(Error::InvalidInput(
                        "Budget trials and parallel trials must be greater than zero".into(),
                    ));
                }
                let objective = args.objective.into();
                let context = store.experience_planning_context(objective)?;
                let opportunities = store.generate_experience_opportunities(&context)?;
                let budget = ExperienceBudget {
                    max_realities: args.budget_trials,
                    max_agent_runs: args.max_agent_runs,
                    max_duration_ms: Some(args.max_duration_minutes.saturating_mul(60_000)),
                    max_commands_per_reality: None,
                    max_curriculum_trials: Some(args.budget_trials),
                    max_parallel_trials: Some(args.max_parallel_trials),
                    max_human_approvals: Some(args.max_human_approvals),
                    allowed_effect_risk: args.allowed_effect_risk.into(),
                };
                let portfolio =
                    store.create_experience_portfolio(&opportunities, &budget, &context)?;
                Ok(json!({
                    "kind":"experience_portfolio_plan",
                    "opportunities_found":opportunities.len(),
                    "portfolio":portfolio,
                    "notice":"Planning records reservations only; it does not execute trials"
                }))
            }
            ExploreCommand::Run { portfolio } => {
                let portfolio = match portfolio {
                    Some(id) => store.experience_portfolio(id)?,
                    None => latest(store)?,
                };
                let plans = store.begin_experience_portfolio(&portfolio.id)?;
                Ok(json!({
                    "kind":"experience_portfolio_run",
                    "portfolio":portfolio.id,
                    "revision":portfolio.revision,
                    "delegated_plans":plans,
                    "notice":"Each typed plan is delegated to the existing Curriculum, Experiment, Causal, Federation, or Capability engine; execution results must be recorded before adaptive replanning"
                }))
            }
            ExploreCommand::Status => {
                let opportunities = store.experience_opportunities()?;
                let mut counts = serde_json::Map::new();
                for status in [
                    ExperienceOpportunityStatus::Candidate,
                    ExperienceOpportunityStatus::Eligible,
                    ExperienceOpportunityStatus::Selected,
                    ExperienceOpportunityStatus::Running,
                    ExperienceOpportunityStatus::Completed,
                    ExperienceOpportunityStatus::Deferred,
                    ExperienceOpportunityStatus::Saturated,
                    ExperienceOpportunityStatus::Blocked,
                    ExperienceOpportunityStatus::Invalidated,
                ] {
                    counts.insert(
                        format!("{status:?}").to_lowercase(),
                        opportunities
                            .iter()
                            .filter(|item| item.status == status)
                            .count()
                            .into(),
                    );
                }
                Ok(json!({
                    "kind":"experience_economics_status",
                    "opportunity_counts":counts,
                    "latest_portfolio":store.latest_experience_portfolio()?,
                    "experience_debt":store.experience_debt()?
                }))
            }
            ExploreCommand::Show { opportunity } | ExploreCommand::Why { opportunity } => {
                opportunity_detail(store, opportunity)
            }
            ExploreCommand::Report => Ok(json!({
                "kind":"experience_economics_report",
                "report":store.experience_economics_report()?
            })),
            ExploreCommand::History { portfolio } => {
                let history = match portfolio {
                    Some(id) => store.experience_portfolio_history(id)?,
                    None => store.experience_portfolios()?,
                };
                Ok(json!({
                    "kind":"experience_portfolio_history",
                    "portfolios":history,
                    "events":store.experience_economics_events()?
                }))
            }
            ExploreCommand::Replay { portfolio } => Ok(json!({
                "kind":"experience_portfolio_replay",
                "original":store.experience_portfolio(portfolio)?,
                "recomputed":store.replay_experience_portfolio(portfolio)?,
                "mutated":false
            })),
            ExploreCommand::Benchmark => Ok(json!({
                "kind":"experience_economics_benchmark",
                "benchmark":benchmark::run()?
            })),
        },
        Commands::Experience {
            command: ExperienceCommand::Debt,
        } => Ok(json!({
            "kind":"experience_debt",
            "items":store.experience_debt()?,
            "definition":"Persistent high-impact unresolved evidence gaps; this is not ordinary code debt"
        })),
        _ => Err(Error::InvalidInput(
            "Experience economics command dispatch failed".into(),
        )),
    }
}
