// SPDX-License-Identifier: Apache-2.0

use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::{
    Error, Result,
    agent::{AgentAdapter, GenericShellAdapter},
    application::RunLearningOptions,
    cancellation::Cancellation,
    core::{
        AgentIdentity, ArtifactRef, CommandSpec, EnvironmentMode, ExecutionId, ExecutionRecord,
        ExperienceId, ExperimentId, LessonId, Reality, RealityId, RealityStatus,
    },
    dojo::{GitRealityProvider, RealityProvider, capture_state, resolve_home},
    evaluation::EvaluationSpec,
    experience::{Experience, ExperienceContext, ReplaySpec},
    experiment::{Experiment, ExperimentConclusion, ExperimentEngine, ExperimentStatus},
    explanation::Explanation,
    learning_loop::{LearningRunOptions, execute_learning_run},
    lesson::{ActionPattern, ConfidencePolicy, HeuristicConfidence, Lesson},
    reflection::{CandidateHypothesis, ManualReflection, ReflectionProvider},
    retrieval::{
        DeterministicRetriever, LessonRetriever, QueryContext, RetrievalOptions, RetrievalReport,
    },
    store::{
        ExperienceQuery, ExperienceStore, ExperienceSummary, LessonQuery, LessonStore,
        LessonSummary, Store, artifact,
    },
    workflow::{RunRequest, RunResult},
};

pub const ISOLATION_WARNING: &str = "Dojo backend: git-worktree\nIsolation: repository filesystem only (not a security sandbox)\nNetwork: shared\nCredentials: shared\nHost filesystem outside worktree: accessible\nGit objects, refs, and repository configuration: shared\nOnly run trusted commands. Default cleanup removes trial changes after capturing a diff.";

#[derive(Debug, Parser)]
#[command(
    name = "hardknock",
    version,
    about = "Agent experience infrastructure — local evidence and controlled experiments"
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
    /// Explain the latest recorded behavioral influence or a selected Experience.
    Why {
        #[arg(long)]
        experience: Option<ExperienceId>,
    },
    /// Count recorded evidence and Lesson states.
    Status,
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
    /// Propose and inspect scoped, revisable Lessons.
    Lesson {
        #[command(subcommand)]
        command: LessonCommand,
    },
    /// Run and inspect controlled baseline/alternative comparisons.
    Experiment {
        #[command(subcommand)]
        command: ExperimentCommand,
    },
    /// Inspect raw process records (not evaluated Experiences).
    Execution {
        #[command(subcommand)]
        command: ExecutionCommand,
    },
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("runner").required(true).args(["agent_command", "agent", "script"])))]
pub struct RunArgs {
    #[arg(
        long,
        help = "Command template with exactly one complete {task} argument; no implicit shell"
    )]
    pub agent_command: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Local deterministic fixture adapter; automatically tests its failed-run hypothesis"
    )]
    pub agent: Option<BuiltinAgent>,
    #[arg(
        long,
        help = "Explicit replayable /bin/sh script in a controlled environment; the task is recorded, not substituted"
    )]
    pub script: Option<String>,
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
    #[arg(long, conflicts_with_all=["with_experience","retry_with_experience"], help="Do not inject advice or learn/retry; still audit repeated mistakes")]
    pub no_experience: bool,
    #[arg(
        long,
        help = "Provide context files to a compatible generic/script adapter"
    )]
    pub with_experience: bool,
    #[arg(long, help = "Opt in to bounded retries using supported Lessons")]
    pub retry_with_experience: bool,
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(0..=10))]
    pub max_retries: u32,
    #[arg(
        long = "action",
        help = "Proposed action for retrieval; repeat for multiple actions"
    )]
    pub actions: Vec<String>,
    #[command(flatten)]
    pub retrieval: RetrievalArgs,
    pub task: String,
}

#[derive(Debug, Args)]
pub struct RetrievalArgs {
    #[arg(long, default_value_t = 0.50)]
    pub min_relevance: f64,
    #[arg(long, default_value_t = 0.70)]
    pub recommend_threshold: f64,
    #[arg(long, default_value_t = 0.85)]
    pub strong_threshold: f64,
}
impl RetrievalArgs {
    fn options(&self) -> Result<RetrievalOptions> {
        let options = RetrievalOptions {
            minimum: self.min_relevance.try_into()?,
            recommend: self.recommend_threshold.try_into()?,
            strong: self.strong_threshold.try_into()?,
            include_candidates: false,
        };
        options.validate()?;
        Ok(options)
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum BuiltinAgent {
    TestAgent,
}

impl RunArgs {
    fn adapter(&self) -> Result<(CommandSpec, AgentIdentity, Option<ReplaySpec>)> {
        if let Some(template) = &self.agent_command {
            let adapter = GenericShellAdapter::new(template)?;
            return Ok((adapter.build_command(&self.task)?, adapter.identity(), None));
        }
        let script = self
            .script
            .as_deref()
            .unwrap_or("./agent-script.sh baseline");
        if script.trim().is_empty() || script.contains('\0') {
            return Err(Error::InvalidInput(
                "Script must be nonempty and contain no NUL bytes".into(),
            ));
        }
        Ok((
            CommandSpec::shell(script, EnvironmentMode::Controlled),
            AgentIdentity {
                kind: if self.agent.is_some() {
                    "test-agent"
                } else {
                    "script"
                }
                .into(),
                executable: "/bin/sh".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                model: None,
            },
            Some(ReplaySpec {
                script: script.into(),
                timeout_secs: self.timeout_secs,
            }),
        ))
    }
}

#[derive(Debug, Subcommand)]
pub enum LessonCommand {
    List {
        #[arg(long)]
        include_retired: bool,
    },
    Search {
        #[arg(long = "action")]
        actions: Vec<String>,
        #[arg(long, default_value = "")]
        task: String,
        #[arg(long)]
        include_candidates: bool,
        #[command(flatten)]
        retrieval: RetrievalArgs,
    },
    Test {
        id: LessonId,
        #[arg(long = "check")]
        checks: Vec<String>,
        #[arg(long, default_value = "Lesson revalidation")]
        task: String,
    },
    Retire {
        id: LessonId,
        #[arg(long)]
        reason: Option<String>,
    },
    Show {
        id: LessonId,
    },
    Propose {
        #[arg(long)]
        experience: ExperienceId,
        #[arg(long)]
        claim: String,
        #[arg(long)]
        avoid: String,
        #[arg(long)]
        prefer: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExperimentCommand {
    List,
    Show {
        id: ExperimentId,
    },
    Run {
        #[arg(long)]
        lesson: LessonId,
    },
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
    LessonSearch {
        query: Box<QueryContext>,
        report: RetrievalReport,
    },
    Why {
        explanation: Box<Explanation>,
    },
    Status {
        counts: serde_json::Value,
    },
    RunCompleted {
        execution: Box<ExecutionRecord>,
        reality: Box<Reality>,
        experience: Box<Experience>,
        lesson: Option<Box<Lesson>>,
        experiment: Option<Box<Experiment>>,
        retries: Vec<RunResult>,
        retry_stop_reason: String,
        interrupted: bool,
    },
    Lesson {
        lesson: Box<Lesson>,
        hypothesis: Box<CandidateHypothesis>,
    },
    Lessons {
        lessons: Vec<LessonSummary>,
    },
    Experiment {
        experiment: Box<Experiment>,
    },
    Experiments {
        experiments: Vec<Experiment>,
    },
    ExperimentCompleted {
        experiment: Box<Experiment>,
        lesson: Box<Lesson>,
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
                retries,
                interrupted,
                ..
            } => {
                if *interrupted
                    || matches!(self, Self::RunCompleted { experiment: Some(e), .. } if e.status == ExperimentStatus::Interrupted)
                {
                    5
                } else {
                    retries
                        .last()
                        .map(|r| r.experience.exit_code(r.execution.status))
                        .unwrap_or_else(|| experience.exit_code(execution.status))
                }
            }
            Self::ExperimentCompleted { experiment, .. } => {
                match (experiment.status, experiment.conclusion) {
                    (ExperimentStatus::Interrupted, _) => 5,
                    (_, ExperimentConclusion::Inconclusive) => 3,
                    _ => 0,
                }
            }
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
                lesson,
                experiment,
                retries,
                retry_stop_reason,
                ..
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
                print_applications(&mut stdout, experience)?;
                for signature in &experience.failure_signatures {
                    writeln!(stdout, "  signature: {}", signature.signature)?;
                }
                if let Some(lesson) = lesson {
                    if experiment.is_some() {
                        writeln!(
                            stdout,
                            "Candidate created: {} · initial confidence {:.2}",
                            lesson.id,
                            f64::from(HeuristicConfidence.initial())
                        )?;
                    }
                    if let Some(experiment) = experiment {
                        print_experiment(&mut stdout, experiment)?;
                    }
                    writeln!(
                        stdout,
                        "Lesson: {:?} · confidence {:.2}",
                        lesson.status,
                        f64::from(lesson.confidence)
                    )?;
                    if retries.is_empty()
                        && experience.outcome == crate::experience::Outcome::Failure
                    {
                        writeln!(
                            stdout,
                            "Original task was not retried; its evaluation remains {:?}.",
                            experience.outcome
                        )?;
                    }
                }
                for (index, retry) in retries.iter().enumerate() {
                    writeln!(
                        stdout,
                        "Retry {}: {:?} · {}",
                        index + 1,
                        retry.experience.outcome,
                        retry.experience.id
                    )?;
                    print_applications(&mut stdout, &retry.experience)?;
                }
                if !retries.is_empty() {
                    writeln!(
                        stdout,
                        "{retry_stop_reason}. Original Experience remains {:?}.",
                        experience.outcome
                    )?;
                }
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
            Self::LessonSearch { report, .. } => {
                for retrieved in &report.matches {
                    writeln!(
                        stdout,
                        "{} · relevance {:.2} · {:?} · confidence {:.2}",
                        retrieved.lesson.id,
                        f64::from(retrieved.relevance),
                        retrieved.lesson.status,
                        f64::from(retrieved.lesson.confidence)
                    )?;
                    for matched in &retrieved.matched_context {
                        writeln!(
                            stdout,
                            "  {}: {} (+{:.2})",
                            matched.signal, matched.value, matched.weight
                        )?;
                    }
                    writeln!(stdout, "  Prefer: {:?}", retrieved.lesson.prefer)?;
                }
                if report.matches.is_empty() {
                    writeln!(stdout, "No applicable Lessons.")?;
                }
                for excluded in &report.excluded {
                    writeln!(
                        stdout,
                        "Excluded {}: {}",
                        excluded.lesson_id, excluded.reason
                    )?;
                }
            }
            Self::Why { explanation } => {
                writeln!(
                    stdout,
                    "Experience: {} · {:?}",
                    explanation.experience_id, explanation.outcome
                )?;
                for entry in &explanation.applications {
                    let a = &entry.application;
                    writeln!(
                        stdout,
                        "{} · {:?} · {:?} · relevance {:.2}",
                        a.lesson_id,
                        a.influence,
                        a.verification,
                        f64::from(a.relevance)
                    )?;
                    writeln!(
                        stdout,
                        "  {}\n  At application: revision {} · confidence {:.2}\n  Current: {:?} · confidence {:.2}\n  Source: {} · {:?}",
                        a.reason,
                        a.lesson_version,
                        f64::from(entry.lesson_at_application.confidence),
                        entry.current_lesson.status,
                        f64::from(entry.current_lesson.confidence),
                        entry.source.id,
                        entry.source.outcome
                    )?;
                    for m in &a.context_matches {
                        writeln!(stdout, "  Match: {} = {}", m.signal, m.value)?;
                    }
                    writeln!(
                        stdout,
                        "  Agent: {} ({})\n  Action: {:?} → {:?}",
                        explanation.agent.kind,
                        explanation.agent.executable,
                        entry.lesson_at_application.avoid,
                        a.resulting_action
                    )?;
                    for e in &entry.experiments {
                        writeln!(stdout, "  Evidence: {} · {:?}", e.id, e.conclusion)?;
                    }
                    for evidence in &entry.current_lesson.evidence {
                        if let crate::lesson::EvidenceRef::Experience {
                            experience_id,
                            relationship,
                        } = evidence
                        {
                            writeln!(
                                stdout,
                                "  Experience evidence: {} · {:?}",
                                experience_id, relationship
                            )?;
                        }
                    }
                }
                if explanation.applications.is_empty() {
                    writeln!(stdout, "No recorded Lesson influence for this Experience.")?;
                }
            }
            Self::Status { counts } => {
                serde_json::to_writer_pretty(&mut stdout, counts)?;
                writeln!(stdout)?;
            }
            Self::Lessons { lessons } => {
                for l in lessons {
                    writeln!(
                        stdout,
                        "{}\t{:?}\t{:.2}\t{}",
                        l.id,
                        l.status,
                        f64::from(l.confidence),
                        l.claim
                    )?;
                }
                if lessons.is_empty() {
                    writeln!(stdout, "No lessons recorded.")?;
                }
            }
            Self::Lesson { lesson, hypothesis } => {
                writeln!(
                    stdout,
                    "{} · {:?} · confidence {:.2} · revision {}",
                    lesson.id,
                    lesson.status,
                    f64::from(lesson.confidence),
                    lesson.version
                )?;
                writeln!(
                    stdout,
                    "Claim: {}\nRationale: {}\nSource Experience: {}\nHypothesis: {} ({})",
                    lesson.claim,
                    lesson.rationale,
                    lesson.source_experience,
                    hypothesis.id,
                    hypothesis.generated_by.kind
                )?;
                serde_json::to_writer_pretty(&mut stdout, lesson)?;
                writeln!(stdout)?;
            }
            Self::Experiments { experiments } => {
                for e in experiments {
                    writeln!(
                        stdout,
                        "{}\t{:?}\t{:?}\t{}",
                        e.id, e.status, e.conclusion, e.lesson_id
                    )?;
                }
                if experiments.is_empty() {
                    writeln!(stdout, "No experiments recorded.")?;
                }
            }
            Self::Experiment { experiment } | Self::ExperimentCompleted { experiment, .. } => {
                print_experiment(&mut stdout, experiment)?
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

fn print_advice(advice: &crate::application::PreparedAdvice, json: bool) -> Result<()> {
    let delivered: Vec<_> = advice
        .report
        .matches
        .iter()
        .filter(|r| advice.delivered.contains(&r.lesson.id))
        .collect();
    let mut out = io::stderr().lock();
    if json {
        let lessons: Vec<_> = delivered
            .iter()
            .map(|r| {
                serde_json::json!({
                    "lesson_id":r.lesson.id,"relevance":r.relevance,
                    "confidence":r.lesson.confidence,"prefer":r.lesson.prefer,
                    "context_matches":r.matched_context,
                })
            })
            .collect();
        serde_json::to_writer(
            &mut out,
            &serde_json::json!({
                "event":"relevant_experience", "delivered":lessons,
                "message":"Advice prepared before agent execution",
            }),
        )?;
        writeln!(out)?;
    } else if !delivered.is_empty() {
        writeln!(out, "Relevant experience (before execution)")?;
        for r in delivered {
            writeln!(
                out,
                "  {} · relevance {:.2} · confidence {:.2}\n  Prefer: {:?}",
                r.lesson.id,
                f64::from(r.relevance),
                f64::from(r.lesson.confidence),
                r.lesson.prefer
            )?;
        }
    }
    Ok(())
}

fn print_applications(out: &mut impl Write, experience: &Experience) -> Result<()> {
    for a in &experience.lesson_applications {
        writeln!(
            out,
            "Relevant experience: {} · {:?} · relevance {:.2} · delivered {}",
            a.lesson_id,
            a.influence,
            f64::from(a.relevance),
            a.delivered
        )?;
        writeln!(out, "  {}", a.reason)?;
    }
    if !experience.repeated_mistakes.is_empty() {
        writeln!(
            out,
            "Repeated mistakes: {}",
            experience.repeated_mistakes.len()
        )?;
    }
    Ok(())
}

fn print_experiment(out: &mut impl Write, e: &Experiment) -> Result<()> {
    writeln!(
        out,
        "Experiment: {} · {:?}\nStarting commit: {}\nSource: {}\nHypothesis: {}\nLesson: {}",
        e.id,
        e.status,
        e.starting_state.git_commit,
        e.source_experience,
        e.hypothesis_id,
        e.lesson_id
    )?;
    for t in &e.trials {
        writeln!(
            out,
            "  {} · {} · {:?}\n    trial: {} · reality: {}\n    Experience: {}",
            t.spec.name, t.spec.command, t.outcome, t.spec.id, t.reality_id, t.experience_id
        )?;
    }
    writeln!(out, "Conclusion: {:?}", e.conclusion)?;
    if let Some(failure) = &e.failure {
        writeln!(out, "Failure: {failure}")?;
    }
    Ok(())
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
            | Commands::Lesson {
                command: LessonCommand::Search { .. } | LessonCommand::Test { .. }
            }
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
        args.adapter()?;
        args.retrieval.options()?;
        EvaluationSpec {
            checks: args.checks.clone(),
        }
        .validate()?;
        if args.agent.is_some() {
            let root = &state
                .as_ref()
                .ok_or_else(|| Error::InvalidInput("Missing starting state".into()))?
                .repo_path;
            let marker = crate::experience::fixture_metadata(root).map_err(|e| Error::Intervention(format!("test-agent requires a valid initialized Hardknock fixture; see docs/experiments.md: {e}")))?;
            if !matches!(
                marker["kind"].as_str(),
                Some(
                    "pnpm-workspace-conflict"
                        | "pnpm-workspace-transfer"
                        | "pnpm-workspace-contradiction"
                        | "npm-ordinary"
                )
            ) || !matches!(marker["version"].as_u64(), Some(1 | 2))
            {
                return Err(Error::InvalidInput("Unsupported test-agent fixture".into()));
            }
        }
    }
    let store = Store::open(&home)?;
    let provider = GitRealityProvider::new(&store);
    match &cli.command {
        Commands::Why { experience } => Ok(Response::Why {
            explanation: Box::new(store.explain(experience.as_ref())?),
        }),
        Commands::Status => Ok(Response::Status {
            counts: store.status_counts()?,
        }),
        Commands::Run(args) => {
            warning(cli.json)?;
            let state =
                state.ok_or_else(|| Error::InvalidInput("Missing starting state".into()))?;
            let (mut spec, agent, replay) = args.adapter()?;
            let fixture = if args.agent.is_some() {
                let marker = crate::experience::fixture_metadata(&state.repo_path)?;
                marker["version"] == 2
            } else {
                false
            };
            if fixture && !args.no_experience {
                spec = CommandSpec::shell("./agent-script.sh run", EnvironmentMode::Controlled);
            }
            let actions = if args.actions.is_empty() {
                if args.agent.is_some() {
                    vec![ActionPattern::shell("./agent-script.sh baseline")]
                } else {
                    args.script
                        .iter()
                        .map(|s| ActionPattern::shell(s))
                        .collect()
                }
            } else {
                args.actions
                    .iter()
                    .map(|s| ActionPattern::shell(s))
                    .collect()
            };
            let cycle = execute_learning_run(
                &store,
                RunRequest {
                    state,
                    goal: args.task.clone(),
                    agent,
                    command: spec,
                    evaluation: EvaluationSpec {
                        checks: args.checks.clone(),
                    },
                    timeout_secs: args.timeout_secs,
                    keep: args.keep,
                    replay,
                    perturbations: vec![],
                    expected_fingerprint: None,
                },
                LearningRunOptions {
                    learning: RunLearningOptions {
                        enabled: !args.no_experience
                            && (args.agent.is_some()
                                || args.with_experience
                                || args.retry_with_experience),
                        audit: args.agent.is_some() || args.no_experience,
                        fixture,
                        proposed_actions: actions,
                        retrieval: args.retrieval.options()?,
                        relations: vec![],
                        on_advice: if cli.quiet {
                            None
                        } else {
                            let json = cli.json;
                            Some(std::sync::Arc::new(move |advice| {
                                print_advice(advice, json)
                            }))
                        },
                    },
                    auto_reflect: args.agent.is_some() && !args.no_experience,
                    retry: args.retry_with_experience,
                    max_retries: args.max_retries,
                },
                cancel,
            )
            .await?;
            Ok(Response::RunCompleted {
                execution: Box::new(cycle.initial.execution),
                reality: Box::new(cycle.initial.reality),
                experience: Box::new(cycle.initial.experience),
                lesson: cycle.lessons.into_iter().next().map(Box::new),
                experiment: cycle.experiments.into_iter().next().map(Box::new),
                retries: cycle.retries,
                retry_stop_reason: cycle.retry_stop_reason,
                interrupted: cycle.interrupted,
            })
        }
        Commands::Lesson { command } => match command {
            LessonCommand::List { include_retired } => Ok(Response::Lessons {
                lessons: LessonStore::list(
                    &store,
                    LessonQuery {
                        status: None,
                        include_retired: *include_retired,
                    },
                )?,
            }),
            LessonCommand::Search {
                actions,
                task,
                include_candidates,
                retrieval,
            } => {
                let state = state
                    .as_ref()
                    .ok_or_else(|| Error::InvalidInput("Missing search state".into()))?;
                let context = ExperienceContext::capture(
                    state,
                    &state.repo_path,
                    EnvironmentMode::Controlled,
                )?;
                let query = QueryContext::new(
                    &context,
                    task,
                    actions.iter().map(|a| ActionPattern::shell(a)).collect(),
                );
                let mut options = retrieval.options()?;
                options.include_candidates = *include_candidates;
                let report = DeterministicRetriever {
                    store: &store,
                    options,
                }
                .retrieve(&query)?;
                Ok(Response::LessonSearch {
                    query: Box::new(query),
                    report,
                })
            }
            LessonCommand::Retire { id, reason } => {
                let lesson = store.retire_lesson(id, reason.clone())?;
                Ok(Response::Lesson {
                    hypothesis: Box::new(store.hypothesis(&lesson.hypothesis_id)?),
                    lesson: Box::new(lesson),
                })
            }
            LessonCommand::Test { id, checks, task } => {
                warning(cli.json)?;
                let state =
                    state.ok_or_else(|| Error::InvalidInput("Missing retest state".into()))?;
                let evaluation = if checks.is_empty() {
                    if crate::experience::fixture_metadata(&state.repo_path).is_err() {
                        return Err(Error::InvalidInput(
                            "Non-fixture lesson tests require at least one --check".into(),
                        ));
                    }
                    EvaluationSpec {
                        checks: vec!["./test.sh".into()],
                    }
                } else {
                    EvaluationSpec {
                        checks: checks.clone(),
                    }
                };
                let experiment = ExperimentEngine { store: &store }
                    .execute_at(id, Some((state, evaluation, task.clone())), cancel)
                    .await?;
                Ok(Response::ExperimentCompleted {
                    experiment: Box::new(experiment),
                    lesson: Box::new(store.lesson(id)?),
                })
            }
            LessonCommand::Show { id } => {
                let lesson = store.lesson(id)?;
                Ok(Response::Lesson {
                    hypothesis: Box::new(store.hypothesis(&lesson.hypothesis_id)?),
                    lesson: Box::new(lesson),
                })
            }
            LessonCommand::Propose {
                experience,
                claim,
                avoid,
                prefer,
            } => {
                let source = store.experience(experience)?;
                let h = ManualReflection {
                    claim: claim.clone(),
                    avoid: avoid.clone(),
                    prefer: prefer.clone(),
                }
                .reflect(&source)?
                .into_iter()
                .next()
                .ok_or_else(|| Error::InvalidInput("No hypothesis proposed".into()))?;
                store.insert_hypothesis(&h)?;
                let lesson = Lesson::candidate(&h, &HeuristicConfidence);
                LessonStore::insert(&store, &lesson)?;
                Ok(Response::Lesson {
                    lesson: Box::new(lesson),
                    hypothesis: Box::new(h),
                })
            }
        },
        Commands::Experiment { command } => match command {
            ExperimentCommand::List => Ok(Response::Experiments {
                experiments: store.experiments()?,
            }),
            ExperimentCommand::Show { id } => Ok(Response::Experiment {
                experiment: Box::new(store.experiment(id)?),
            }),
            ExperimentCommand::Run { lesson } => {
                warning(cli.json)?;
                let experiment = ExperimentEngine { store: &store }
                    .execute(lesson, cancel)
                    .await?;
                Ok(Response::ExperimentCompleted {
                    experiment: Box::new(experiment),
                    lesson: Box::new(store.lesson(lesson)?),
                })
            }
        },
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
