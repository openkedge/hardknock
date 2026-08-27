// SPDX-License-Identifier: Apache-2.0

use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::{
    Error, Result,
    agent::{AgentAdapter, GenericShellAdapter},
    cancellation::Cancellation,
    core::{
        ArtifactRef, ExecutionId, ExecutionRecord, ExperienceId, Reality, RealityId, RealityStatus,
    },
    dojo::{GitRealityProvider, RealityProvider, capture_state, resolve_home},
    evaluation::EvaluationSpec,
    experience::Experience,
    store::{ExperienceQuery, ExperienceStore, ExperienceSummary, Store, artifact},
    workflow::{RunRequest, run_once},
};

pub const ISOLATION_WARNING: &str = "Dojo backend: git-worktree\nIsolation: repository filesystem only (not a security sandbox)\nNetwork: shared\nCredentials: shared\nHost filesystem outside worktree: accessible\nGit objects, refs, and repository configuration: shared\nOnly run trusted commands. Default cleanup removes trial changes after capturing a diff.";

#[derive(Debug, Parser)]
#[command(
    name = "hardknock",
    version,
    about = "Agent experience infrastructure — experimental execution substrate"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Emit JSON results on stdout and JSON diagnostics on stderr"
    )]
    pub json: bool,
    #[arg(long, global = true, conflicts_with_all = ["json", "verbose"], help = "Suppress normal output; never suppress safety warnings")]
    pub quiet: bool,
    #[arg(
        long,
        global = true,
        help = "Enable debug logs on stderr (or use RUST_LOG)"
    )]
    pub verbose: bool,
    #[arg(long, global = true)]
    pub no_emoji: bool,
    #[arg(
        long,
        global = true,
        env = "HARDKNOCK_HOME",
        help = "Dedicated data directory; defaults to ~/.hardknock"
    )]
    pub home: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        default_value = ".",
        help = "Source Git repository (requires a clean committed snapshot)"
    )]
    pub repo: PathBuf,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a noninteractive command in a detached worktree; capture output and diff.
    Run(RunArgs),
    /// Inspect and manage disposable Git working states.
    Reality {
        #[command(subcommand)]
        command: RealityCommand,
    },
    /// Inspect immutable evaluated observations.
    Experience {
        #[command(subcommand)]
        command: ExperienceCommand,
    },
    /// Inspect raw process records (not evaluated Experiences).
    Execution {
        #[command(subcommand)]
        command: ExecutionCommand,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(
        long,
        help = "Command template with exactly one complete {task} argument; no implicit shell"
    )]
    pub agent_command: String,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=86400))]
    pub timeout_secs: u64,
    #[arg(
        long = "check",
        help = "Required check, executed by /bin/sh -c; repeat for multiple checks"
    )]
    pub checks: Vec<String>,
    #[arg(
        long,
        help = "Keep the trial worktree for inspection; otherwise discard it after artifact capture"
    )]
    pub keep: bool,
    pub task: String,
}

#[derive(Debug, Subcommand)]
pub enum RealityCommand {
    Create,
    List,
    Show {
        id: RealityId,
    },
    /// Recreate the parent's original snapshot, not its current modifications.
    Fork {
        id: RealityId,
    },
    /// Show tracked and nonignored new-file changes against the starting commit.
    Diff {
        id: RealityId,
    },
    /// Delete this managed worktree, including uncommitted trial changes.
    Discard {
        id: RealityId,
    },
    /// Delete unlocked orphaned automatic-run worktrees. Stop abandoned commands first.
    Cleanup,
}

#[derive(Debug, Subcommand)]
pub enum ExecutionCommand {
    List,
    Show { id: ExecutionId },
}

#[derive(Debug, Subcommand)]
pub enum ExperienceCommand {
    List,
    Show { id: ExperienceId },
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Response {
    RunCompleted {
        execution: Box<ExecutionRecord>,
        reality: Box<Reality>,
        experience: Box<Experience>,
    },
    Experience {
        experience: Box<Experience>,
    },
    Experiences {
        experiences: Vec<ExperienceSummary>,
    },
    Reality {
        reality: Reality,
    },
    Realities {
        realities: Vec<Reality>,
    },
    Execution {
        execution: Box<ExecutionRecord>,
    },
    Executions {
        executions: Vec<ExecutionRecord>,
    },
    RealityDiff {
        reality_id: RealityId,
        artifact: ArtifactRef,
    },
    CleanupCompleted {
        discarded: Vec<RealityId>,
        skipped_active: Vec<RealityId>,
    },
}

impl Response {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::RunCompleted {
                execution,
                experience,
                ..
            } => experience.exit_code(execution.status),
            _ => 0,
        }
    }

    pub fn print(&self, cli: &Cli) -> Result<()> {
        if cli.quiet {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        if cli.json {
            serde_json::to_writer(&mut stdout, self)?;
            writeln!(stdout)?;
            return Ok(());
        }
        match self {
            Self::RunCompleted {
                execution,
                reality,
                experience,
            } => {
                writeln!(
                    stdout,
                    "{}Dojo · {}",
                    if cli.no_emoji { "" } else { "🌸 " },
                    reality.id
                )?;
                writeln!(
                    stdout,
                    "Process {:?} · exit {:?} · {} ms",
                    execution.status, execution.action.exit_code, execution.action.duration_ms
                )?;
                writeln!(
                    stdout,
                    "Evaluation: {:?} · {}",
                    experience.outcome, experience.evaluation.summary
                )?;
                for check in &experience.evaluation.checks {
                    writeln!(stdout, "  {:?} · {}", check.status, check.command)?;
                }
                writeln!(stdout, "Experience: {}", experience.id)?;
                writeln!(stdout, "Execution: {}", execution.id)?;
                writeln!(
                    stdout,
                    "Reality: {:?} · {}",
                    reality.status,
                    reality.root.display()
                )?;
                writeln!(
                    stdout,
                    "stdout: {}\nstderr: {}\ndiff: {}",
                    execution.action.stdout.path.display(),
                    execution.action.stderr.path.display(),
                    execution.diff.path.display()
                )?;
            }
            Self::Experience { experience } => {
                serde_json::to_writer_pretty(&mut stdout, experience)?;
                writeln!(stdout)?;
            }
            Self::Experiences { experiences } => {
                for e in experiences {
                    writeln!(stdout, "{}\t{:?}\t{}", e.id, e.outcome, e.goal)?;
                }
                if experiences.is_empty() {
                    writeln!(stdout, "No experiences recorded.")?;
                }
            }
            Self::Reality { reality } => writeln!(
                stdout,
                "{}\t{:?}\t{}",
                reality.id,
                reality.status,
                reality.root.display()
            )?,
            Self::Realities { realities } => {
                for r in realities {
                    writeln!(stdout, "{}\t{:?}\t{}", r.id, r.status, r.root.display())?;
                }
                if realities.is_empty() {
                    writeln!(stdout, "No realities recorded.")?;
                }
            }
            Self::Execution { execution } => {
                serde_json::to_writer_pretty(&mut stdout, execution)?;
                writeln!(stdout)?;
            }
            Self::Executions { executions } => {
                for e in executions {
                    writeln!(stdout, "{}\t{:?}\t{}", e.id, e.status, e.reality_id)?;
                }
                if executions.is_empty() {
                    writeln!(stdout, "No executions recorded.")?;
                }
            }
            Self::RealityDiff { artifact, .. } => {
                io::copy(&mut fs::File::open(&artifact.path)?, &mut stdout)?;
            }
            Self::CleanupCompleted {
                discarded,
                skipped_active,
            } => writeln!(
                stdout,
                "Discarded {} orphaned realities; skipped {} active realities.",
                discarded.len(),
                skipped_active.len()
            )?,
        }
        Ok(())
    }
}

pub fn warning(json: bool) -> Result<()> {
    let mut stderr = io::stderr().lock();
    if json {
        serde_json::to_writer(
            &mut stderr,
            &serde_json::json!({"event":"isolation_warning", "message":ISOLATION_WARNING}),
        )?;
        writeln!(stderr)?;
    } else {
        writeln!(stderr, "{ISOLATION_WARNING}")?;
    }
    Ok(())
}

pub async fn execute(cli: &Cli, cancel: &Cancellation) -> Result<Response> {
    let raw_home = cli
        .home
        .clone()
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".hardknock")))
        .ok_or_else(|| {
            Error::Intervention("Set HARDKNOCK_HOME or --home; HOME is unavailable.".into())
        })?;
    let home = resolve_home(&raw_home)?;
    // Validate input before creating a database or touching a repository.
    let state = if matches!(
        cli.command,
        Commands::Run(_)
            | Commands::Reality {
                command: RealityCommand::Create
            }
    ) {
        let state = capture_state(&cli.repo)?;
        if home.starts_with(&state.repo_path) {
            return Err(Error::Intervention(
                "HARDKNOCK_HOME must be outside the source repository.".into(),
            ));
        }
        Some(state)
    } else {
        None
    };
    if let Commands::Run(args) = &cli.command {
        GenericShellAdapter::new(&args.agent_command)?;
    }
    let store = Store::open(&home)?;
    let provider = GitRealityProvider::new(&store);
    match &cli.command {
        Commands::Run(args) => {
            warning(cli.json)?;
            let state =
                state.ok_or_else(|| Error::InvalidInput("Missing starting state".into()))?;
            let adapter = GenericShellAdapter::new(&args.agent_command)?;
            let spec = adapter.build_command(&args.task)?;
            let result = run_once(
                &store,
                RunRequest {
                    state,
                    goal: args.task.clone(),
                    agent: adapter.identity(),
                    command: spec,
                    evaluation: EvaluationSpec {
                        checks: args.checks.clone(),
                    },
                    timeout_secs: args.timeout_secs,
                    keep: args.keep,
                    replay: None,
                    perturbations: vec![],
                    expected_fingerprint: None,
                },
                cancel,
            )
            .await?;
            Ok(Response::RunCompleted {
                execution: Box::new(result.execution),
                reality: Box::new(result.reality),
                experience: Box::new(result.experience),
            })
        }
        Commands::Experience { command } => match command {
            ExperienceCommand::List => Ok(Response::Experiences {
                experiences: ExperienceStore::list(&store, ExperienceQuery::default())?,
            }),
            ExperienceCommand::Show { id } => Ok(Response::Experience {
                experience: Box::new(store.experience(id)?),
            }),
        },
        Commands::Reality { command } => match command {
            RealityCommand::Create => {
                warning(cli.json)?;
                Ok(Response::Reality {
                    reality: provider
                        .create(&state.ok_or_else(|| {
                            Error::InvalidInput("Missing starting state".into())
                        })?)?,
                })
            }
            RealityCommand::List => Ok(Response::Realities {
                realities: store.realities()?,
            }),
            RealityCommand::Show { id } => Ok(Response::Reality {
                reality: store.reality(id)?,
            }),
            RealityCommand::Fork { id } => {
                warning(cli.json)?;
                let _lease = store.lock_reality(id)?;
                Ok(Response::Reality {
                    reality: provider.fork(&store.reality(id)?)?,
                })
            }
            RealityCommand::Diff { id } => {
                let _lease = store.lock_reality(id)?;
                let patch = provider.diff(&store.reality(id)?)?;
                let path = store
                    .home
                    .join("artifacts")
                    .join(format!("diff-{}.patch", uuid::Uuid::new_v4()));
                fs::write(&path, patch)?;
                Ok(Response::RealityDiff {
                    reality_id: id.clone(),
                    artifact: artifact(&path)?,
                })
            }
            RealityCommand::Discard { id } => {
                let _lease = store.lock_reality(id)?;
                let mut reality = store.reality(id)?;
                provider.discard(&mut reality)?;
                Ok(Response::Reality { reality })
            }
            RealityCommand::Cleanup => {
                let mut discarded = Vec::new();
                let mut skipped_active = Vec::new();
                for reality in store.realities()? {
                    if !reality.ephemeral || reality.status == RealityStatus::Discarded {
                        continue;
                    }
                    let _lease = match store.lock_reality(&reality.id) {
                        Ok(lease) => lease,
                        Err(Error::Intervention(_)) => {
                            skipped_active.push(reality.id.clone());
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    // A run may finish (or retain its state after a capture error)
                    // between listing and acquisition of the lease.
                    let mut reality = store.reality(&reality.id)?;
                    if !reality.ephemeral || reality.status == RealityStatus::Discarded {
                        continue;
                    }
                    provider.discard(&mut reality)?;
                    discarded.push(reality.id);
                }
                Ok(Response::CleanupCompleted {
                    discarded,
                    skipped_active,
                })
            }
        },
        Commands::Execution { command } => match command {
            ExecutionCommand::List => Ok(Response::Executions {
                executions: store.executions()?,
            }),
            ExecutionCommand::Show { id } => Ok(Response::Execution {
                execution: Box::new(store.execution(id)?),
            }),
        },
    }
}
