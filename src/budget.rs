// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};

use crate::experimentation::{CandidateExecution, ExperimentRequest};

/// Hard caps on explicitly scheduled work. Native agent tool calls are not observable here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExperienceBudget {
    #[serde(alias = "max_trials")]
    pub max_realities: usize,
    pub max_agent_runs: usize,
    pub max_duration_ms: Option<u64>,
    pub max_commands_per_reality: Option<usize>,
    pub max_curriculum_trials: Option<usize>,
    pub max_parallel_trials: Option<usize>,
}

impl Default for ExperienceBudget {
    fn default() -> Self {
        Self {
            max_realities: 3,
            max_agent_runs: 3,
            max_duration_ms: Some(300_000),
            max_commands_per_reality: None,
            max_curriculum_trials: None,
            max_parallel_trials: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperienceUsage {
    pub realities: usize,
    pub agent_runs: usize,
    pub duration_ms: u64,
    /// Candidate shell entries plus evaluator processes actually launched.
    pub commands: usize,
    /// None means native internal tool-call counts are unavailable, not zero.
    pub tool_calls: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "decision", content = "detail", rename_all = "snake_case")]
pub enum BudgetDecision {
    Approved,
    Reduced(ExperienceBudget),
    Rejected(String),
}

pub trait ExperienceBudgetPolicy {
    fn evaluate(&self, request: &ExperimentRequest, usage: &ExperienceUsage) -> BudgetDecision;
}

/// Reject the entire comparison rather than quietly omit candidates.
pub struct StrictBudgetPolicy;
impl ExperienceBudgetPolicy for StrictBudgetPolicy {
    fn evaluate(&self, request: &ExperimentRequest, usage: &ExperienceUsage) -> BudgetDecision {
        let budget = &request.budget;
        let agents = request
            .candidates
            .iter()
            .filter(|c| matches!(c.execution, CandidateExecution::AgentTask { .. }))
            .count();
        let reason = if request.candidates.len()
            > budget.max_realities.saturating_sub(usage.realities)
        {
            Some("Requested candidates exceed the remaining Reality budget")
        } else if agents > budget.max_agent_runs.saturating_sub(usage.agent_runs) {
            Some("Requested candidates exceed the remaining agent-run budget")
        } else if budget
            .max_duration_ms
            .is_some_and(|limit| usage.duration_ms >= limit)
        {
            Some("Experiment duration budget exhausted")
        } else if budget.max_commands_per_reality.is_some_and(|limit| {
            request.candidates.iter().any(|c| match &c.execution {
                CandidateExecution::Shell { commands } => {
                    commands
                        .len()
                        .saturating_add(request.evaluator.checks.len())
                        > limit
                }
                // Native agents may launch arbitrary tool calls. A cap cannot honestly be enforced.
                CandidateExecution::AgentTask { .. } => true,
            })
        }) {
            Some(
                "Command cap exceeded or unenforceable for an AgentTask; bound native runs by duration and agent-run count",
            )
        } else {
            None
        };
        reason
            .map(|s| BudgetDecision::Rejected(s.into()))
            .unwrap_or(BudgetDecision::Approved)
    }
}
