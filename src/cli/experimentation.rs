// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands, ExperimentCommand, RealityCommand};
use crate::{
    Error, Result,
    bridge::config::Config,
    cancellation::Cancellation,
    core::{AgentIdentity, CandidateId, ExperimentRequestId, Reality},
    dojo::capture_state,
    experimentation::*,
    store::{ExperimentStore, Store},
};
use clap::Args;
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Args)]
pub struct TryArgs {
    #[arg(default_value = "Which candidate performs better?")]
    pub question: String,
    /// Without --agent, each candidate value is an explicit shell script.
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long = "candidate", required = true, value_name = "NAME=STRATEGY")]
    pub candidates: Vec<String>,
    #[arg(long = "check")]
    pub checks: Vec<String>,
    #[arg(long)]
    pub budget_realities: Option<usize>,
    #[arg(long)]
    pub budget_agent_runs: Option<usize>,
    #[arg(long, value_parser = parse_duration)]
    pub budget_duration: Option<u64>,
    #[arg(long)]
    pub max_commands_per_reality: Option<usize>,
    #[arg(long)]
    pub minimize_diff_size: bool,
    #[arg(long)]
    pub minimize_duration: bool,
    #[arg(long)]
    pub allow_network: bool,
    /// Request through an active native agent's Bridge session instead of a user CLI run.
    #[arg(long)]
    pub session: Option<String>,
}

fn parse_duration(s: &str) -> std::result::Result<u64, String> {
    let (number, multiplier) = if let Some(s) = s.strip_suffix("ms") {
        (s, 1)
    } else if let Some(s) = s.strip_suffix('s') {
        (s, 1000)
    } else if let Some(s) = s.strip_suffix('m') {
        (s, 60_000)
    } else {
        return Err("Use milliseconds, seconds, or minutes, e.g. 500ms, 30s, 5m".into());
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| *n > 0 && *n <= 86_400_000)
        .ok_or_else(|| "Duration must be positive and at most 24h".into())
}
fn identity(name: &str) -> AgentIdentity {
    AgentIdentity {
        kind: name.into(),
        executable: name.into(),
        version: None,
        model: None,
    }
}
fn candidates(values: &[String], agent: Option<&str>) -> Result<Vec<ExperimentCandidate>> {
    values
        .iter()
        .map(|s| {
            let (name, strategy) = s
                .split_once('=')
                .ok_or_else(|| Error::InvalidInput("Use --candidate 'name=strategy'".into()))?;
            Ok(ExperimentCandidate {
                id: CandidateId::new(),
                name: name.into(),
                description: String::new(),
                execution: match agent {
                    Some(a) => CandidateExecution::AgentTask {
                        prompt: strategy.into(),
                        agent: Some(identity(a)),
                    },
                    None => CandidateExecution::Shell {
                        commands: vec![strategy.into()],
                    },
                },
                expected_outcome: None,
            })
        })
        .collect()
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExperimentResponse {
    Experiment {
        experiment: Box<StrategyExperiment>,
        relations: Vec<ExperimentRelation>,
        progress: Vec<(u64, ExperimentProgress)>,
        partial_candidates: Vec<CandidateResult>,
    },
    List {
        experiments: Vec<StrategyExperiment>,
        lesson_experiments: Vec<crate::experiment::Experiment>,
    },
    Legacy {
        experiment: Box<crate::experiment::Experiment>,
    },
    Cancel {
        experiment_id: crate::core::ExperimentId,
        cancellation_requested: bool,
    },
    Tree {
        realities: Vec<Reality>,
    },
    Export {
        reality_id: crate::core::RealityId,
        patch: PathBuf,
        bytes: usize,
    },
    Bridge {
        evidence: serde_json::Value,
    },
}
impl ExperimentResponse {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Experiment { experiment, .. } => match experiment.status {
                ExperimentStatus::Cancelled => 5,
                ExperimentStatus::Failed | ExperimentStatus::Rejected => 2,
                _ => 0,
            },
            Self::Bridge { evidence } => match evidence["status"].as_str() {
                Some("cancelled") => 5,
                Some("failed" | "rejected") => 2,
                _ => 0,
            },
            _ => 0,
        }
    }
    pub fn print(&self, out: &mut impl Write) -> Result<()> {
        match self {
            Self::Experiment {
                experiment: e,
                relations,
                partial_candidates,
                ..
            } => {
                writeln!(
                    out,
                    "Experiment {} · {:?}\nQuestion: {}\nOrigin: {:?} / {}\nIntent: {:?}",
                    e.id,
                    e.status,
                    e.request.question,
                    e.request.origin,
                    e.request.requested_by.kind,
                    e.request.intent
                )?;
                writeln!(
                    out,
                    "Budget ceilings: {} Realities, {} agent runs, {}ms",
                    e.effective_budget.max_realities,
                    e.effective_budget.max_agent_runs,
                    e.effective_budget.max_duration_ms.unwrap_or(0)
                )?;
                if let Some(result) = &e.result {
                    if let Some(proof) = &result.starting_state {
                        writeln!(
                            out,
                            "\nStarting commit: {}\nFingerprint: {}\nEquivalent start: verified for launched candidates\nScope: {}",
                            proof.state_ref.git_commit, proof.fingerprint, proof.scope
                        )?;
                    }
                    writeln!(
                        out,
                        "\nQuality: {}\nChanged variables: {}",
                        format!("{:?}", result.quality).to_uppercase(),
                        result
                            .changed_variables
                            .iter()
                            .map(|v| v.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                    for c in &result.candidates {
                        let verdict = match c.evaluation.status {
                            crate::evaluation::EvaluationStatus::Completed
                                if c.evaluation.success =>
                            {
                                "PASS"
                            }
                            crate::evaluation::EvaluationStatus::Completed => "FAIL",
                            crate::evaluation::EvaluationStatus::Interrupted => "INTERRUPTED",
                            crate::evaluation::EvaluationStatus::TimedOut => "TIMED OUT",
                            _ => "INCONCLUSIVE",
                        };
                        writeln!(
                            out,
                            "\n{}\n  evaluator: {verdict} · {}\n  execution: {:?}\n  duration: {}ms\n  Reality: {}\n  Experience: {}",
                            c.name,
                            c.evaluation.summary,
                            c.execution_status,
                            c.duration_ms,
                            c.reality_id,
                            c.experience_id
                        )?;
                        if let Some(diff) = &c.diff_summary {
                            writeln!(
                                out,
                                "  diff: {} files, +{} / -{}, {} binary",
                                diff.files_changed,
                                diff.insertions,
                                diff.deletions,
                                diff.binary_files
                            )?;
                        }
                    }
                    let name = result
                        .candidates
                        .iter()
                        .find(|c| Some(&c.candidate_id) == result.recommendation.as_ref())
                        .map(|c| c.name.as_str())
                        .unwrap_or("No definitive winner");
                    writeln!(
                        out,
                        "\nRecommendation: {name}\nComparison policy: {}\nEvidence: {}",
                        result.comparison.policy, result.comparison.evidence_weight
                    )?;
                    for reason in &result.comparison.reasons {
                        writeln!(out, "  {reason}")?;
                    }
                    writeln!(
                        out,
                        "\nRealities: {} · Agent runs: {} · Duration: {}ms\nExperience gained: {} Experiences, {} Candidate Lessons",
                        result.usage.realities,
                        result.usage.agent_runs,
                        result.usage.duration_ms,
                        result.created_experience.len(),
                        result.candidate_lessons.len()
                    )?;
                }
                if let Some(error) = &e.failure {
                    writeln!(out, "\nReason: {error}")?;
                }
                if e.result.is_none() {
                    for candidate in partial_candidates {
                        writeln!(
                            out,
                            "{}: {} (partial; no comparison yet)",
                            candidate.name, candidate.evaluation.summary
                        )?;
                    }
                }
                for relation in relations {
                    writeln!(
                        out,
                        "Lineage: {} -> {} ({:?})",
                        relation.parent, relation.child, relation.relation
                    )?;
                }
                writeln!(
                    out,
                    "\nNo candidate was applied to your repository. Use reality export to inspect a saved patch."
                )?;
                for notice in &e.notices {
                    writeln!(out, "{notice}")?;
                }
            }
            Self::List {
                experiments,
                lesson_experiments,
            } => {
                for e in experiments {
                    writeln!(
                        out,
                        "{}  {:?}  {}  {}",
                        e.id, e.status, e.request.requested_by.kind, e.request.question
                    )?;
                }
                for e in lesson_experiments {
                    writeln!(out, "{}  {:?}  lesson-validation", e.id, e.status)?;
                }
            }
            Self::Legacy { experiment } => super::print_experiment(out, experiment)?,
            Self::Cancel {
                experiment_id,
                cancellation_requested,
            } => writeln!(
                out,
                "{experiment_id}: cancellation requested={cancellation_requested}; inspect experiment show for terminal cleanup status"
            )?,
            Self::Tree { realities } => {
                fn walk(
                    out: &mut impl Write,
                    all: &[Reality],
                    parent: Option<&crate::core::RealityId>,
                    depth: usize,
                ) -> Result<()> {
                    if depth > all.len() {
                        return Ok(());
                    }
                    for r in all.iter().filter(|r| r.parent.as_ref() == parent) {
                        writeln!(
                            out,
                            "{}{} · {:?} · {:?} · {}",
                            "  ".repeat(depth),
                            r.id,
                            r.fork_reason,
                            r.status,
                            &r.starting_state.git_commit[..8]
                        )?;
                        walk(out, all, Some(&r.id), depth + 1)?;
                    }
                    Ok(())
                }
                walk(out, realities, None, 0)?;
            }
            Self::Export {
                reality_id,
                patch,
                bytes,
            } => writeln!(
                out,
                "Exported {bytes} bytes from {reality_id} to {} (not applied)",
                patch.display()
            )?,
            Self::Bridge { evidence } => {
                writeln!(out, "{}", serde_json::to_string_pretty(evidence)?)?
            }
        }
        Ok(())
    }
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Try(_)
            | Commands::Why {
                experiment: Some(_),
                ..
            }
            | Commands::Reality {
                command: RealityCommand::Tree | RealityCommand::Export { .. }
            }
    ) || matches!(command,Commands::Experiment { command } if !matches!(command,ExperimentCommand::Run { .. }))
}

pub async fn execute(
    cli: &Cli,
    store: &Store,
    cancel: &Cancellation,
) -> Result<ExperimentResponse> {
    let config = Config::load(&store.home)?;
    let orchestrator = ExperimentOrchestrator {
        store,
        config: &config,
    };
    let wrap = |experiment: StrategyExperiment| -> Result<ExperimentResponse> {
        let relations = store.experiment_relations(&experiment.id)?;
        Ok(ExperimentResponse::Experiment {
            progress: store.experiment_progress(&experiment.id, 0)?,
            partial_candidates: if experiment.result.is_none() {
                store.candidate_results(&experiment.id)?
            } else {
                vec![]
            },
            experiment: Box::new(experiment),
            relations,
        })
    };
    match &cli.command {
        Commands::Try(args) => {
            if std::env::var_os("HARDKNOCK_EXPERIMENT_CANDIDATE").is_some() {
                return Err(Error::Intervention(
                    "Nested experiment requests from candidate processes are disabled".into(),
                ));
            }
            let mut budget = config.experience_budget.budget();
            if let Some(n) = args.budget_realities {
                budget.max_realities = n;
            }
            if let Some(n) = args.budget_agent_runs {
                budget.max_agent_runs = n;
            }
            if let Some(n) = args.budget_duration {
                budget.max_duration_ms = Some(n);
            }
            if let Some(n) = args.max_commands_per_reality {
                budget.max_commands_per_reality = Some(n);
            }
            let candidates = candidates(&args.candidates, args.agent.as_deref())?;
            let evaluator = crate::evaluation::EvaluationSpec {
                checks: args.checks.clone(),
            };
            let criteria = ComparisonCriteria {
                minimize_diff_size: args.minimize_diff_size,
                minimize_duration: args.minimize_duration,
                ..Default::default()
            };
            let capabilities = ExperimentCapabilities {
                allow_network: args.allow_network,
                ..Default::default()
            };
            super::warning(cli.json)?;
            if let Some(session) = &args.session {
                return bridge_try(
                    store,
                    args,
                    session,
                    budget,
                    candidates,
                    evaluator,
                    criteria,
                    capabilities,
                    cancel,
                )
                .await;
            }
            let request = ExperimentRequest {
                id: ExperimentRequestId::new(),
                session_id: format!("cli-{}", uuid::Uuid::new_v4()),
                question: args.question.clone(),
                hypothesis: None,
                candidates,
                starting_state: ExperimentStartingState {
                    state_ref: capture_state(&cli.repo)?,
                    expected_fingerprint: None,
                    parent_reality: None,
                    source: SnapshotSource::RepositoryCommit,
                },
                evaluator,
                budget,
                requested_by: identity(args.agent.as_deref().unwrap_or("user")),
                created_at: chrono::Utc::now(),
                criteria,
                origin: ExperimentOrigin::User,
                intent: ExperimentIntent::CompareStrategies,
                capabilities,
            };
            let accepted = orchestrator.submit(request)?;
            if !cli.json && !cli.quiet {
                eprintln!(
                    "Trying experiment {} (cancel with: hardknock experiment cancel {})",
                    accepted.id, accepted.id
                );
            }
            wrap(run_with_progress(&orchestrator, &accepted.id, cli, cancel).await?)
        }
        Commands::Experiment {
            command: ExperimentCommand::List { agent },
        } => Ok(ExperimentResponse::List {
            experiments: ExperimentStore::list(store, agent.as_deref())?,
            lesson_experiments: if agent.is_none() {
                store.experiments()?
            } else {
                vec![]
            },
        }),
        Commands::Experiment {
            command: ExperimentCommand::Show { id },
        }
        | Commands::Why {
            experiment: Some(id),
            ..
        } => match ExperimentStore::get(store, id)? {
            Some(e) => wrap(e),
            None => Ok(ExperimentResponse::Legacy {
                experiment: Box::new(store.experiment(id)?),
            }),
        },
        Commands::Experiment {
            command: ExperimentCommand::Cancel { id },
        } => Ok(ExperimentResponse::Cancel {
            experiment_id: id.clone(),
            cancellation_requested: store.cancel_experiment(id)?,
        }),
        Commands::Experiment {
            command: ExperimentCommand::Replay { id, .. } | ExperimentCommand::Fork { id, .. },
        } => {
            let parent = store.strategy_experiment(id)?;
            let mut request = parent.request.clone();
            request.id = ExperimentRequestId::new();
            request.created_at = chrono::Utc::now();
            request.origin = ExperimentOrigin::User;
            request.session_id = format!("cli-{}", uuid::Uuid::new_v4());
            request.starting_state.expected_fingerprint = None;
            let relation = if let Commands::Experiment {
                command:
                    ExperimentCommand::Fork {
                        candidates: additions,
                        ..
                    },
            } = &cli.command
            {
                let agent = request.candidates.first().and_then(|c| match &c.execution {
                    CandidateExecution::AgentTask { agent, .. } => Some(
                        agent
                            .as_ref()
                            .unwrap_or(&request.requested_by)
                            .kind
                            .as_str(),
                    ),
                    _ => None,
                });
                request.candidates.extend(candidates(additions, agent)?);
                ExperimentRelationType::Extension
            } else {
                if let Commands::Experiment {
                    command:
                        ExperimentCommand::Replay {
                            candidate: Some(name),
                            ..
                        },
                } = &cli.command
                {
                    request.candidates.retain(|c| c.name == *name);
                    if request.candidates.is_empty() {
                        return Err(Error::InvalidInput(format!("Unknown candidate {name}")));
                    }
                }
                ExperimentRelationType::Replay
            };
            for c in &mut request.candidates {
                c.id = CandidateId::new();
            }
            let mut accepted = orchestrator.submit(request)?;
            let current = orchestrator.starting_proof(&accepted.request);
            let original = parent
                .result
                .as_ref()
                .and_then(|r| r.starting_state.as_ref());
            if current
                .as_ref()
                .ok()
                .zip(original)
                .is_some_and(|(a, b)| a.fingerprint != b.fingerprint)
            {
                accepted.notices.push("Replay environment differs from original experiment. Recorded as new evidence.".into());
            } else {
                accepted.notices.push("Replay uses the recorded commit with newly measured runtime fingerprints; inherited native settings and remote services remain unverified.".into());
            }
            if !accepted.status.terminal() {
                ExperimentStore::update_status(store, &accepted)?;
            }
            store.insert_experiment_relation(&ExperimentRelation {
                parent: id.clone(),
                child: accepted.id.clone(),
                relation,
            })?;
            super::warning(cli.json)?;
            wrap(run_with_progress(&orchestrator, &accepted.id, cli, cancel).await?)
        }
        Commands::Reality {
            command: RealityCommand::Tree,
        } => Ok(ExperimentResponse::Tree {
            realities: store.realities()?,
        }),
        Commands::Reality {
            command: RealityCommand::Export { id, patch },
        } => {
            store.reality(id)?;
            let execution = store
                .executions()?
                .into_iter()
                .find(|e| e.reality_id == *id)
                .ok_or_else(|| Error::NotFound("Reality has no captured execution patch".into()))?;
            let stored = &execution.diff;
            if !stored
                .path
                .canonicalize()?
                .starts_with(store.home.join("artifacts"))
            {
                return Err(Error::Intervention(
                    "Saved patch is outside the managed artifact directory".into(),
                ));
            }
            let bytes = fs::read(&stored.path)?;
            if blake3::hash(&bytes).to_hex().as_str() != stored.blake3
                || bytes.len() as u64 != stored.bytes
            {
                return Err(Error::Intervention(
                    "Saved patch failed artifact integrity verification".into(),
                ));
            }
            let absolute = if patch.is_absolute() {
                patch.clone()
            } else {
                std::env::current_dir()?.join(patch)
            };
            let parent = absolute.parent().unwrap_or(Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&absolute)
                .map_err(|e| Error::Io(e.error))?;
            Ok(ExperimentResponse::Export {
                reality_id: id.clone(),
                patch: absolute,
                bytes: bytes.len(),
            })
        }
        _ => Err(Error::InvalidInput("Unsupported experiment command".into())),
    }
}

async fn run_with_progress(
    orchestrator: &ExperimentOrchestrator<'_>,
    id: &crate::core::ExperimentId,
    cli: &Cli,
    cancel: &Cancellation,
) -> Result<StrategyExperiment> {
    if cli.json || cli.quiet {
        return orchestrator.execute(id, cancel).await;
    }
    let mut after = 0;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let work = orchestrator.execute(id, cancel);
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = ticker.tick() => {
                match orchestrator.store.experiment_progress(id,after) {
                    Ok(events) => for (sequence,event) in events { after=sequence; eprintln!("{:?}: {}",event.phase,event.message); },
                    Err(error) => tracing::warn!(%error,"Progress display unavailable"),
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn bridge_try(
    store: &Store,
    args: &TryArgs,
    session: &str,
    budget: crate::budget::ExperienceBudget,
    candidates: Vec<ExperimentCandidate>,
    evaluator: crate::evaluation::EvaluationSpec,
    criteria: ComparisonCriteria,
    capabilities: ExperimentCapabilities,
    cancel: &Cancellation,
) -> Result<ExperimentResponse> {
    use crate::bridge::{
        protocol::{AgentEvent, ExperimentRequested},
        transport::BridgeClient,
    };
    let mut client = BridgeClient::new(&store.home);
    // Deliberate experiment RPCs are not on the pre-tool hot path.
    client.timeout = Duration::from_secs(5);
    let accepted = client
        .request(AgentEvent::ExperimentRequested(ExperimentRequested {
            hardknock_session_id: session.into(),
            request_id: ExperimentRequestId::new(),
            question: args.question.clone(),
            hypothesis: None,
            candidates,
            evaluator,
            budget,
            criteria,
            capabilities,
            intent: ExperimentIntent::CompareStrategies,
        }))
        .await?;
    if accepted["event"] == "experiment_rejected" {
        return Ok(ExperimentResponse::Bridge { evidence: accepted });
    }
    let id: crate::core::ExperimentId = serde_json::from_value(accepted["experiment_id"].clone())?;
    let mut after = 0;
    let mut cancellation_sent = false;
    loop {
        if cancel.is_cancelled() && !cancellation_sent {
            client
                .request(AgentEvent::ExperimentCancelled {
                    hardknock_session_id: session.into(),
                    experiment_id: id.clone(),
                })
                .await?;
            cancellation_sent = true;
        }
        let evidence = client
            .request(AgentEvent::ExperimentProgress {
                hardknock_session_id: session.into(),
                experiment_id: id.clone(),
                after,
            })
            .await?;
        if let Some(progress) = evidence["progress"].as_array() {
            for entry in progress {
                after = after.max(entry[0].as_u64().unwrap_or(0));
            }
        }
        if matches!(
            evidence["status"].as_str(),
            Some("completed" | "cancelled" | "rejected" | "failed")
        ) {
            return Ok(ExperimentResponse::Bridge { evidence });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
