// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, budget::ExperienceBudget, core::EnvironmentMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentsConfig {
    pub max_parallel_realities: usize,
    /// Home-wide leases, acquired before any worktree is created.
    pub provider_capacity: usize,
    pub continue_after_session_end: bool,
    pub agent_requests: AgentRequestsConfig,
    /// Executor templates are trusted local configuration, never arbitrary wire commands.
    pub agents: BTreeMap<String, ExperimentAgentConfig>,
}
impl Default for ExperimentsConfig {
    fn default() -> Self {
        Self {
            max_parallel_realities: 3,
            provider_capacity: 8,
            continue_after_session_end: false,
            agent_requests: AgentRequestsConfig::default(),
            agents: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentRequestsConfig {
    pub enabled: bool,
    pub max_realities: usize,
    pub max_parallel: usize,
    pub allow_network: bool,
    /// Deliberate requests only. Automatic initiation is not implemented.
    pub auto_request: bool,
}
impl Default for AgentRequestsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_realities: 3,
            max_parallel: 2,
            allow_network: false,
            auto_request: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentAgentConfig {
    pub command: String,
    #[serde(default)]
    pub environment: EnvironmentMode,
    #[serde(default)]
    pub version: Option<String>,
    /// Must describe the configured executor; not a model-routing instruction.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExperienceBudgetConfig {
    pub max_realities: usize,
    pub max_agent_runs: usize,
    pub max_duration_seconds: u64,
    pub max_commands_per_reality: Option<usize>,
}
impl Default for ExperienceBudgetConfig {
    fn default() -> Self {
        Self {
            max_realities: 3,
            max_agent_runs: 3,
            max_duration_seconds: 300,
            max_commands_per_reality: None,
        }
    }
}
impl ExperienceBudgetConfig {
    pub fn budget(&self) -> ExperienceBudget {
        ExperienceBudget {
            max_realities: self.max_realities,
            max_agent_runs: self.max_agent_runs,
            max_duration_ms: Some(self.max_duration_seconds.saturating_mul(1000)),
            max_commands_per_reality: self.max_commands_per_reality,
        }
    }
}

impl ExperimentsConfig {
    pub fn validate(&self, budget: &ExperienceBudgetConfig) -> Result<()> {
        if !(1..=32).contains(&self.max_parallel_realities)
            || !(1..=32).contains(&self.provider_capacity)
            || !(1..=32).contains(&self.agent_requests.max_parallel)
            || self.agent_requests.max_realities > 32
            || budget.max_realities > 32
            || budget.max_agent_runs > 32
            || !(1..=86_400).contains(&budget.max_duration_seconds)
            || self.agent_requests.auto_request
        {
            return Err(Error::InvalidInput("Experiment limits out of range (1–32 parallel/capacity, up to 32 runs, 1–86400 seconds); auto_request is unsupported".into()));
        }
        for agent in self.agents.values() {
            crate::agent::GenericShellAdapter::new(&agent.command)?;
        }
        Ok(())
    }
}
