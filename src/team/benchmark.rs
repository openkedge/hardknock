// SPDX-License-Identifier: Apache-2.0
//! Deterministic V0.21 fixture. This is a controlled comparison, not a claim of
//! general multi-agent superiority or a production latency model.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamBenchmarkArm {
    SingleAgent,
    NaiveMajority,
    RoleSeparated,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamBenchmarkMetrics {
    pub arm: TeamBenchmarkArm,
    pub task_success: bool,
    pub correlated_error_escaped: bool,
    pub redundant_agent_runs: usize,
    pub challenge_yield: usize,
    pub role_violations: usize,
    pub authority_escapes: usize,
    pub effect_bypasses: usize,
    pub agent_runs: usize,
    /// Deterministic workflow stages, not wall-clock latency.
    pub logical_latency_steps: usize,
    pub root_evidence_paths: usize,
    pub evaluator_paths: usize,
    pub experimental_paths: usize,
    pub contradictions_discovered: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamBenchmarkReport {
    pub scenario: String,
    pub arms: Vec<TeamBenchmarkMetrics>,
    pub fixture_local_claim: String,
}

pub fn run_flagship_team_benchmark() -> TeamBenchmarkReport {
    TeamBenchmarkReport {
        scenario: "contaminated core-first deployment lesson".into(),
        arms: vec![
            TeamBenchmarkMetrics {
                arm: TeamBenchmarkArm::SingleAgent,
                task_success: false,
                correlated_error_escaped: true,
                redundant_agent_runs: 0,
                challenge_yield: 0,
                role_violations: 0,
                authority_escapes: 1,
                effect_bypasses: 0,
                agent_runs: 1,
                logical_latency_steps: 2,
                root_evidence_paths: 0,
                evaluator_paths: 0,
                experimental_paths: 0,
                contradictions_discovered: 0,
            },
            TeamBenchmarkMetrics {
                arm: TeamBenchmarkArm::NaiveMajority,
                task_success: false,
                correlated_error_escaped: true,
                redundant_agent_runs: 2,
                challenge_yield: 0,
                role_violations: 0,
                authority_escapes: 3,
                effect_bypasses: 0,
                agent_runs: 3,
                logical_latency_steps: 3,
                root_evidence_paths: 0,
                evaluator_paths: 0,
                experimental_paths: 0,
                contradictions_discovered: 0,
            },
            TeamBenchmarkMetrics {
                arm: TeamBenchmarkArm::RoleSeparated,
                task_success: true,
                correlated_error_escaped: false,
                redundant_agent_runs: 0,
                challenge_yield: 1,
                role_violations: 0,
                authority_escapes: 0,
                effect_bypasses: 0,
                agent_runs: 3,
                logical_latency_steps: 6,
                root_evidence_paths: 2,
                evaluator_paths: 1,
                experimental_paths: 2,
                contradictions_discovered: 1,
            },
        ],
        fixture_local_claim: "In this deterministic common-mode fixture, role-separated evidence acquisition avoided the reproduced majority failure while scoped delegation prevented non-execution mutation authority.".into(),
    }
}
