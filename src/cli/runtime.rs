// SPDX-License-Identifier: Apache-2.0

use std::{fs, io::Write, path::PathBuf};

use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use super::{Cli, Commands};
use crate::{
    Error, Result,
    bridge::{config::Config, protocol::NormalizedAction},
    core::RuntimeDecisionId,
    curriculum::Severity,
    runtime::*,
    store::{RuntimeStore, Store},
};

#[derive(Debug, Subcommand)]
pub enum RuntimeCommand {
    /// Show configured runtime mode, policy, and aggregate decision counts.
    Status,
    /// Audit recent decisions and observed outcomes.
    Audit {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Group recurring unknown, stale, contradicted, and abstention contexts.
    Gaps,
    /// Show the active inspectable runtime policy.
    Policy,
    /// Run the deterministic 60-scenario control and latency benchmark.
    Benchmark,
}

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    List {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Show {
        id: RuntimeDecisionId,
    },
    Replay {
        id: RuntimeDecisionId,
        #[arg(long, value_enum)]
        policy: Option<RuntimePolicyProfile>,
    },
    Simulate(SimulateArgs),
    Compare(CompareArgs),
    Feedback(FeedbackArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RiskArg {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}

impl From<RiskArg> for Severity {
    fn from(value: RiskArg) -> Self {
        match value {
            RiskArg::Informational => Self::Informational,
            RiskArg::Low => Self::Low,
            RiskArg::Medium => Self::Medium,
            RiskArg::High => Self::High,
            RiskArg::Critical => Self::Critical,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum KnowledgeArg {
    Supported,
    Contradicted,
    Stale,
    Unknown,
    OutOfScope,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("input").required(true).args(["scenario", "action"])))]
pub struct SimulateArgs {
    #[arg(long, conflicts_with = "action")]
    scenario: Option<PathBuf>,
    #[arg(long, conflicts_with = "scenario")]
    action: Option<String>,
    #[arg(long, value_enum)]
    policy: Option<RuntimePolicyProfile>,
    #[arg(long, value_enum, default_value = "low")]
    risk: RiskArg,
    #[arg(long, value_enum, default_value = "unknown")]
    knowledge: KnowledgeArg,
    #[arg(long)]
    testable: bool,
    #[arg(long)]
    no_record: bool,
}

#[derive(Debug, Args)]
pub struct CompareArgs {
    #[arg(long)]
    scenario: PathBuf,
    #[arg(long, default_value = "balanced,conservative")]
    policies: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutcomeArg {
    Successful,
    Failed,
    AvoidedFailure,
    UnnecessaryIntervention,
    Inconclusive,
}

impl From<OutcomeArg> for DecisionOutcome {
    fn from(value: OutcomeArg) -> Self {
        match value {
            OutcomeArg::Successful => Self::Successful,
            OutcomeArg::Failed => Self::Failed,
            OutcomeArg::AvoidedFailure => Self::AvoidedFailure,
            OutcomeArg::UnnecessaryIntervention => Self::UnnecessaryIntervention,
            OutcomeArg::Inconclusive => Self::Inconclusive,
        }
    }
}

#[derive(Debug, Args)]
pub struct FeedbackArgs {
    id: RuntimeDecisionId,
    #[arg(long, value_enum)]
    outcome: OutcomeArg,
    #[arg(long = "experience")]
    experiences: Vec<String>,
    #[arg(long)]
    agent_disagreed: bool,
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Runtime { .. } | Commands::Decision { .. }
    )
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    let configured = Config::load(&store.home)?.runtime.policy_config();
    match &cli.command {
        Commands::Runtime { command } => match command {
            RuntimeCommand::Status => Ok(json!({
                "kind":"runtime_status",
                "configuration":configured,
                "audit":store.runtime_audit(10_000)?,
                "gaps":store.runtime_gaps()?.len(),
            })),
            RuntimeCommand::Audit { limit } => Ok(json!({
                "kind":"runtime_audit",
                "limit":limit,
                "audit":store.runtime_audit(*limit)?,
            })),
            RuntimeCommand::Gaps => Ok(json!({
                "kind":"runtime_gaps",
                "gaps":store.runtime_gaps()?,
                "curriculum_recommendations":store.runtime_curriculum_recommendations()?,
                "auto_run":false,
            })),
            RuntimeCommand::Policy => Ok(json!({
                "kind":"runtime_policy",
                "policy":configured,
                "precedence":[
                    "hard_security_policy",
                    "capability_availability",
                    "effect_authority",
                    "critical_assurance_blockers",
                    "runtime_evidence",
                    "agent_preference"
                ],
                "self_modifying":false,
            })),
            RuntimeCommand::Benchmark => Ok(json!({
                "kind":"runtime_benchmark",
                "report":run_runtime_benchmark()?,
            })),
        },
        Commands::Decision { command } => match command {
            DecisionCommand::List { limit } => {
                if !(1..=10_000).contains(limit) {
                    return Err(Error::InvalidInput(
                        "Decision list limit must be between 1 and 10000".into(),
                    ));
                }
                let decisions = store.runtime_decisions()?;
                Ok(json!({
                    "kind":"decision_list",
                    "decisions": decisions.iter().rev().take(*limit).map(summary).collect::<Vec<_>>(),
                }))
            }
            DecisionCommand::Show { id } => Ok(json!({
                "kind":"decision_show",
                "record":store.runtime_decision(id)?,
                "feedback":store.runtime_feedback(id)?,
            })),
            DecisionCommand::Replay { id, policy } => {
                let mut selected = configured;
                if let Some(policy) = policy {
                    selected.profile = *policy;
                }
                let previous = store.runtime_decision(id)?;
                let current = store.replay_runtime_decision(id, selected)?;
                Ok(json!({
                    "kind":"decision_replay",
                    "original":summary(&previous),
                    "replay":current,
                    "original_mutated":false,
                }))
            }
            DecisionCommand::Simulate(args) => {
                let scenario = if let Some(path) = &args.scenario {
                    read_scenario(path)?
                } else {
                    action_scenario(
                        args.action.as_deref().unwrap_or_default(),
                        args.risk,
                        args.knowledge,
                        args.testable,
                        &cli.repo,
                    )
                };
                let mut selected = configured;
                if let Some(profile) = args.policy {
                    selected.profile = profile;
                }
                selected.experiment_mode = scenario.experiments.mode;
                let context = scenario.decision_context()?;
                if args.no_record {
                    let evaluation = DeterministicRuntimeController::with_config(selected)?
                        .evaluate(&context)?;
                    Ok(json!({
                        "kind":"decision_simulation",
                        "recorded":false,
                        "scenario":scenario.name,
                        "evaluation":evaluation,
                    }))
                } else {
                    Ok(json!({
                        "kind":"decision_simulation",
                        "recorded":true,
                        "scenario":scenario.name,
                        "record":store.record_runtime_decision(&context, selected)?,
                    }))
                }
            }
            DecisionCommand::Compare(args) => {
                let scenario = read_scenario(&args.scenario)?;
                let policies = parse_policies(&args.policies)?;
                let mut comparisons = Vec::new();
                for profile in policies {
                    let mut selected = configured.clone();
                    selected.profile = profile;
                    selected.experiment_mode = scenario.experiments.mode;
                    comparisons.push(json!({
                        "policy":profile,
                        "evaluation":DeterministicRuntimeController::with_config(selected)?.evaluate(&scenario.decision_context()?)?,
                    }));
                }
                Ok(json!({
                    "kind":"decision_compare",
                    "scenario":scenario.name,
                    "executed":false,
                    "comparisons":comparisons,
                }))
            }
            DecisionCommand::Feedback(args) => {
                let feedback = RuntimeDecisionFeedback {
                    decision_id: args.id.clone(),
                    outcome: args.outcome.into(),
                    evidence: args
                        .experiences
                        .iter()
                        .cloned()
                        .map(EvidenceRef::Experience)
                        .collect(),
                    observed_at: Utc::now(),
                    agent_disagreed: args.agent_disagreed,
                };
                store.record_runtime_feedback(&feedback)?;
                Ok(json!({"kind":"decision_feedback","feedback":feedback}))
            }
        },
        Commands::Why {
            decision: Some(id), ..
        } => {
            let record = store.runtime_decision(id)?;
            Ok(json!({
                "kind":"decision_why",
                "decision_id":id,
                "decision":record.decision,
                "knowledge":record.evaluation.knowledge,
                "reasons":record.evaluation.reasons,
                "evidence":record.evaluation.evidence,
                "blockers":record.evaluation.blockers,
                "risk":record.context.risk,
                "assurance":record.context.assurance,
                "authority":record.context.capability_context,
                "next":next_steps(&record.decision),
            }))
        }
        _ => Err(Error::InvalidInput("Runtime dispatch failed".into())),
    }
}

pub fn print(value: &Value, output: &mut impl Write) -> Result<()> {
    match value["kind"].as_str() {
        Some("decision_show" | "decision_simulation" | "decision_replay") => {
            let record = value
                .get("record")
                .or_else(|| value.get("replay"))
                .filter(|record| record.is_object());
            if let Some(record) = record {
                print_record(record, output)?;
            } else {
                serde_json::to_writer_pretty(&mut *output, value)?;
                writeln!(output)?;
            }
        }
        Some("decision_why") => {
            writeln!(
                output,
                "Hardknock selected {}.",
                decision_name(&value["decision"])
            )?;
            writeln!(output, "\nKnowledge\n  {}", value["knowledge"])?;
            writeln!(output, "\nWhy")?;
            for reason in value["reasons"].as_array().into_iter().flatten() {
                writeln!(output, "  {}", compact(reason))?;
            }
            if let Some(blockers) = value["blockers"].as_array()
                && !blockers.is_empty()
            {
                writeln!(output, "\nBlockers")?;
                for blocker in blockers {
                    writeln!(output, "  {}", compact(blocker))?;
                }
            }
            writeln!(output, "\nNext")?;
            for next in value["next"].as_array().into_iter().flatten() {
                writeln!(
                    output,
                    "  {}",
                    next["description"].as_str().unwrap_or("inspect")
                )?;
            }
        }
        Some("decision_list") => {
            for decision in value["decisions"].as_array().into_iter().flatten() {
                writeln!(
                    output,
                    "{} · {} · knowledge {} · {}",
                    decision["id"].as_str().unwrap_or("decision"),
                    decision["decision"].as_str().unwrap_or("unknown"),
                    decision["knowledge"].as_str().unwrap_or("unknown"),
                    decision["created_at"].as_str().unwrap_or("unknown time")
                )?;
            }
        }
        _ => {
            serde_json::to_writer_pretty(&mut *output, value)?;
            writeln!(output)?;
        }
    }
    Ok(())
}

fn print_record(value: &Value, output: &mut impl Write) -> Result<()> {
    writeln!(
        output,
        "{}\n\nDecision\n  {}\n\nKnowledge\n  {}\n\nPolicy\n  {}",
        value["id"].as_str().unwrap_or("runtime decision"),
        decision_name(&value["decision"]),
        value["evaluation"]["knowledge"]
            .as_str()
            .unwrap_or("unknown"),
        value["evaluation"]["policy_version"]
            .as_str()
            .unwrap_or("unknown")
    )?;
    writeln!(output, "\nReasons")?;
    for reason in value["evaluation"]["reasons"]
        .as_array()
        .into_iter()
        .flatten()
    {
        writeln!(output, "  {}", compact(reason))?;
    }
    Ok(())
}

fn decision_name(value: &Value) -> &str {
    value["decision"].as_str().unwrap_or("unknown")
}

fn compact(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("kind")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| value.to_string())
}

fn summary(record: &RuntimeDecisionRecord) -> Value {
    json!({
        "id":record.id,
        "session_id":record.session_id,
        "decision":record.decision.kind(),
        "knowledge":record.evaluation.knowledge,
        "policy_version":record.evaluation.policy_version,
        "created_at":record.created_at,
    })
}

fn read_scenario(path: &PathBuf) -> Result<RuntimeScenario> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::InvalidInput(
            "Runtime scenario must not be a symlink".into(),
        ));
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::InvalidInput("Runtime scenario exceeds 1 MiB".into()));
    }
    let scenario: RuntimeScenario = serde_json::from_slice(&bytes)?;
    scenario.validate()?;
    Ok(scenario)
}

fn action_scenario(
    action: &str,
    risk: RiskArg,
    knowledge: KnowledgeArg,
    testable: bool,
    repo: &std::path::Path,
) -> RuntimeScenario {
    let mut scenario = RuntimeScenario {
        name: "cli-simulation".into(),
        task: TaskDescriptor {
            description: action.into(),
            family: None,
            tags: Vec::new(),
        },
        proposed_action: Some(NormalizedAction::Shell {
            command: action.into(),
            cwd: repo.to_string_lossy().into(),
        }),
        risk: RuntimeRiskAssessment {
            severity: risk.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    match knowledge {
        KnowledgeArg::Supported => scenario.knowledge.local_supported = true,
        KnowledgeArg::Contradicted => {
            scenario.knowledge.local_supported = true;
            scenario.knowledge.local_contradicted = true;
        }
        KnowledgeArg::Stale => {
            scenario.knowledge.local_supported = true;
            scenario.knowledge.evidence_stale = true;
        }
        KnowledgeArg::Unknown => {}
        KnowledgeArg::OutOfScope => scenario.knowledge.context_in_scope = false,
    }
    scenario.experiments.safe_reality_available = testable;
    scenario.experiments.effect_safe = testable;
    scenario
}

fn parse_policies(value: &str) -> Result<Vec<RuntimePolicyProfile>> {
    let mut policies = Vec::new();
    for item in value.split(',') {
        let policy = match item.trim() {
            "developer" => RuntimePolicyProfile::Developer,
            "balanced" => RuntimePolicyProfile::Balanced,
            "conservative" => RuntimePolicyProfile::Conservative,
            other => {
                return Err(Error::InvalidInput(format!(
                    "Unknown runtime policy {other}; use developer, balanced, or conservative"
                )));
            }
        };
        if !policies.contains(&policy) {
            policies.push(policy);
        }
    }
    if policies.is_empty() {
        return Err(Error::InvalidInput(
            "At least one runtime policy is required".into(),
        ));
    }
    Ok(policies)
}

fn next_steps(decision: &RuntimeDecision) -> Vec<RuntimeAlternative> {
    match decision {
        RuntimeDecision::RequireApproval(decision) => decision.alternatives.clone(),
        RuntimeDecision::Abstain(decision) => decision.possible_next_steps.clone(),
        RuntimeDecision::Experiment(_) => vec![RuntimeAlternative {
            name: "run_experiment".into(),
            description: "Run only the bounded experiment within its Reality and budget".into(),
        }],
        RuntimeDecision::Replan(_) => vec![RuntimeAlternative {
            name: "replan".into(),
            description: "Change the strategy before continuing".into(),
        }],
        RuntimeDecision::Recover(_) => vec![RuntimeAlternative {
            name: "recover".into(),
            description: "Apply the matched validated Recovery within its scope".into(),
        }],
        RuntimeDecision::Act(_) => vec![RuntimeAlternative {
            name: "act".into(),
            description: "Proceed through ordinary capability and effect enforcement".into(),
        }],
    }
}
