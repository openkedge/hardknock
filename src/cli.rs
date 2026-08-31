// SPDX-License-Identifier: Apache-2.0

use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
pub(crate) mod assurance;
pub(crate) mod attestation;
pub(crate) mod capability;
pub mod curriculum;
mod development;
mod effects;
mod experimentation;
mod federation;
pub mod integrations;
mod resilience;
pub(crate) mod tools;
use resilience::{ChaosCommand, EnvelopeCommand, RecoveryCommand, ReflexCommand, SkillCommand};

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
        CapabilityStore, ExperienceQuery, ExperienceStore, ExperienceSummary, LessonQuery,
        LessonStore, LessonSummary, Store, artifact,
    },
    workflow::{RunRequest, RunResult},
};

pub const ISOLATION_WARNING: &str = "Dojo backend: git-worktree\nIsolation: repository filesystem only (not a security sandbox)\nNetwork: shared\nCredentials: shared\nHost filesystem outside worktree: accessible\nExternal effects: shared unless explicitly routed through a governed effect adapter; arbitrary shell/network calls are not intercepted\nGit objects, refs, and repository configuration: shared\nOnly run trusted commands. Default cleanup removes trial changes after capturing a diff.";

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
    /// Define, validate, inspect, and revision behavioral contracts.
    Contract {
        #[command(subcommand)]
        command: assurance::ContractCommand,
    },
    /// Evaluate, export, verify, compare, and revoke empirical assurance.
    Assurance {
        #[command(subcommand)]
        command: assurance::AssuranceCommand,
    },
    /// Register and execute portable, least-authority tools.
    Tool {
        #[command(subcommand)]
        command: tools::ToolCommand,
    },
    /// Inspect and verify immutable per-tool execution attestations.
    Attestation {
        #[command(subcommand)]
        command: attestation::AttestationCommand,
    },
    /// Inspect, explain, audit, compare, and revoke execution capabilities.
    Capability {
        #[command(subcommand)]
        command: capability::CapabilityCommand,
    },
    /// Prepare, inspect, explicitly commit, discard, and reconcile governed effects.
    Effect {
        #[command(subcommand)]
        command: effects::EffectCommand,
    },
    /// Manage local cryptographic peer relationships.
    Peer {
        #[command(subcommand)]
        command: federation::PeerCommand,
    },
    /// Export, import, reproduce, and inspect signed external evidence.
    Federate {
        #[command(subcommand)]
        command: federation::FederateCommand,
    },
    /// Trace local and cross-node evidence lineage.
    Provenance { object: String },
    /// Inspect and experimentally resolve local/remote evidence conflicts.
    Conflict {
        #[command(subcommand)]
        command: federation::ConflictCommand,
    },
    /// Inspect reconstructable, scoped development profiles.
    Profile {
        #[command(subcommand)]
        command: development::ProfileCommand,
    },
    /// Compare recorded, comparable profile windows.
    Growth(development::SubjectArgs),
    /// Inspect append-only evidence and revision events.
    Timeline(development::TimelineArgs),
    Revalidation {
        #[command(subcommand)]
        command: development::RevalidationCommand,
    },
    Episode {
        #[command(subcommand)]
        command: development::EpisodeCommand,
    },
    Benchmark {
        #[command(subcommand)]
        command: development::BenchmarkCommand,
    },
    /// Local database and experience health; never runs experiments.
    Doctor,
    /// Plan and explicitly run bounded experience curricula.
    Curriculum {
        #[command(subcommand)]
        command: curriculum::CurriculumCommand,
    },
    /// Group task contexts using explicit examples.
    TaskFamily {
        #[command(subcommand)]
        command: curriculum::TaskFamilyCommand,
    },
    /// Manage the authenticated local lifecycle Bridge.
    Bridge {
        #[command(subcommand)]
        command: integrations::BridgeCommand,
    },
    /// Install and diagnose native adapters.
    Integrate {
        #[command(subcommand)]
        command: integrations::IntegrationCommand,
    },
    /// Native hook entry point; bounded JSON is read from stdin.
    IntegrationEvent {
        #[arg(long, value_enum)]
        agent: integrations::HookAgent,
    },
    Agent {
        #[command(subcommand)]
        command: integrations::AgentCommand,
    },
    Events {
        #[command(subcommand)]
        command: integrations::EventsCommand,
    },
    /// Run controlled local adversity around a healthy strategy.
    Chaos {
        #[command(subcommand)]
        command: ChaosCommand,
    },
    /// Inspect sparse, empirically tested operating conditions.
    Envelope {
        #[command(subcommand)]
        command: EnvelopeCommand,
    },
    /// Test and explicitly activate scoped early-failure rules.
    Reflex {
        #[command(subcommand)]
        command: ReflexCommand,
    },
    /// Test procedures after reproducing a failure.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
    /// Manually register known-successful replayable procedures.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Explain the latest recorded behavioral influence or a selected Experience.
    Why {
        #[arg(long)]
        experience: Option<ExperienceId>,
        #[arg(long, conflicts_with = "experience")]
        experiment: Option<ExperimentId>,
    },
    /// Count recorded evidence and Lesson states.
    Status,
    /// Run a noninteractive command in a detached worktree; capture output and diff.
    Run(RunArgs),
    /// Stop guessing: try explicit alternatives from an equivalent committed state.
    Try(experimentation::TryArgs),
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
    #[arg(long, value_enum, default_value = "git-worktree")]
    pub provider: RealityProviderChoice,
    #[arg(long = "capabilities", requires = "provider")]
    pub capability_profile: Option<String>,
    #[arg(long, requires = "capability_profile")]
    pub image: Option<String>,
    #[arg(long, help = "Maximum additional executions shared by controlled trials and retries", value_parser = clap::value_parser!(u32).range(0..=100))]
    pub experience_budget: Option<u32>,
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
    History {
        id: LessonId,
    },
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
        #[arg(
            long,
            help = "Include separately labeled advisory federated candidates"
        )]
        include_federated: bool,
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
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    Show {
        id: ExperimentId,
    },
    Run {
        #[arg(long)]
        lesson: LessonId,
    },
    Replay {
        id: ExperimentId,
        #[arg(long, conflicts_with = "candidate")]
        all: bool,
        #[arg(long)]
        candidate: Option<String>,
    },
    Fork {
        id: ExperimentId,
        #[arg(long = "candidate", required = true)]
        candidates: Vec<String>,
    },
    Cancel {
        id: ExperimentId,
    },
}

#[derive(Debug, Subcommand)]
pub enum RealityCommand {
    Create {
        #[arg(long, value_enum, default_value = "git-worktree")]
        provider: RealityProviderChoice,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, requires = "profile")]
        image: Option<String>,
    },
    List,
    Tree,
    Export {
        id: RealityId,
        #[arg(long)]
        patch: PathBuf,
    },
    Show {
        id: RealityId,
    },
    Inspect {
        id: RealityId,
    },
    Freeze {
        id: RealityId,
    },
    Execute {
        id: RealityId,
        #[arg(last = true, required = true)]
        command: Vec<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RealityProviderChoice {
    GitWorktree,
    Container,
}

#[derive(Debug, Subcommand)]
pub enum ExecutionCommand {
    List,
    Show { id: ExecutionId },
}

#[derive(Debug, Subcommand)]
pub enum ExperienceCommand {
    Health(development::SubjectArgs),
    Maintain(development::SubjectArgs),
    List,
    Show { id: ExperienceId },
}

#[derive(Serialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Response {
    Assurance {
        result: serde_json::Value,
    },
    Tools {
        result: serde_json::Value,
    },
    Attestations {
        result: serde_json::Value,
    },
    Capability {
        result: serde_json::Value,
    },
    Effects {
        result: serde_json::Value,
    },
    Federation {
        result: serde_json::Value,
    },
    Development {
        result: serde_json::Value,
    },
    Curriculum {
        result: Box<curriculum::CurriculumResponse>,
    },
    Experimentation {
        result: Box<experimentation::ExperimentResponse>,
    },
    Integration {
        result: serde_json::Value,
    },
    Resilience {
        result: Box<resilience::ResilienceResponse>,
    },
    LessonSearch {
        query: Box<QueryContext>,
        report: RetrievalReport,
        federated: Vec<crate::federation::FederatedObject>,
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
        provenance: serde_json::Value,
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
        strategy_experiments: Vec<crate::experimentation::StrategyExperiment>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        effects: Option<serde_json::Value>,
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
            Self::Assurance { result } => assurance::exit_code(result),
            Self::Curriculum { result } => result.exit_code(),
            Self::Experimentation { result } => result.exit_code(),
            Self::Resilience { result } => result.exit_code(),
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
        if let Self::Integration { result } = self {
            serde_json::to_writer(&mut io::stdout().lock(), result)?;
            println!();
            return Ok(());
        }
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
            Self::Assurance { result } => assurance::print(result, &mut stdout)?,
            Self::Tools { result } | Self::Attestations { result } => {
                serde_json::to_writer_pretty(&mut stdout, result)?;
                writeln!(stdout)?;
            }
            Self::Capability { result } => {
                serde_json::to_writer_pretty(&mut stdout, result)?;
                writeln!(stdout)?;
            }
            Self::Effects { result } => effects::print(result, &mut stdout)?,
            Self::Federation { result } => federation::print(result, &mut stdout)?,
            Self::Curriculum { result } => result.print(&mut stdout)?,
            Self::Development { result } => development::print(result, &mut stdout)?,
            Self::Experimentation { result } => result.print(&mut stdout)?,
            Self::Integration { .. } => {}
            Self::Resilience { result } => result.print(&mut stdout)?,
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
            Self::LessonSearch {
                report, federated, ..
            } => {
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
                for external in federated {
                    writeln!(stdout, "FEDERATED · ADVISORY · {}", external.id)?;
                    writeln!(
                        stdout,
                        "  producer: {} · context match {:.2}",
                        external.identity.origin_node, external.trust.context_compatibility.score
                    )?;
                    writeln!(
                        stdout,
                        "  This federated evidence has not been locally validated."
                    )?;
                }
            }
            Self::Why { explanation } => {
                writeln!(
                    stdout,
                    "Experience: {} · {:?}",
                    explanation.experience_id, explanation.outcome
                )?;
                for entry in &explanation.reflexes {
                    let m = &entry.matched;
                    writeln!(
                        stdout,
                        "Hardknock requested {:?} because {} matched ({}; confidence {:.2}).",
                        m.response,
                        m.reflex_id,
                        if m.test_only { "test only" } else { "active" },
                        f64::from(m.confidence)
                    )?;
                    writeln!(
                        stdout,
                        "  Consecutive failures: {}; no state change: {}; config changed: {}",
                        m.observed.consecutive_failures,
                        m.observed.no_state_change,
                        m.observed.config_changed
                    )?;
                    writeln!(
                        stdout,
                        "  Source: {} / {} / {}",
                        entry.source_campaign,
                        entry.source_trial.id,
                        entry.source_trial.experience_id
                    )?;
                    for lesson in &entry.lessons {
                        writeln!(stdout, "  Lesson {}: {}", lesson.id, lesson.claim)?;
                    }
                }
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
            Self::Lesson {
                lesson,
                hypothesis,
                provenance,
            } => {
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
                writeln!(
                    stdout,
                    "Agent evidence (cross-agent replication does not imply independence):"
                )?;
                serde_json::to_writer_pretty(&mut stdout, provenance)?;
                writeln!(stdout)?;
                serde_json::to_writer_pretty(&mut stdout, lesson)?;
                writeln!(stdout)?;
            }
            Self::Experiments {
                experiments,
                strategy_experiments,
            } => {
                for e in strategy_experiments {
                    writeln!(
                        stdout,
                        "{} · {:?} · {} · {}",
                        e.id, e.status, e.request.requested_by.kind, e.request.question
                    )?;
                }
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
            Self::Reality { reality, effects } => {
                writeln!(
                    stdout,
                    "{}\t{:?}\t{}",
                    reality.id,
                    reality.status,
                    reality.root.display()
                )?;
                if let Some(effects) = effects {
                    writeln!(
                        stdout,
                        "External Effects\n  proposed {}\n  prepared {}\n  committed {}\n  discarded {}",
                        effects["proposed"].as_u64().unwrap_or(0),
                        effects["prepared"].as_u64().unwrap_or(0),
                        effects["committed"].as_u64().unwrap_or(0),
                        effects["discarded"].as_u64().unwrap_or(0)
                    )?;
                }
            }
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

fn container_provider<'a>(
    store: &'a Store,
    image: Option<&str>,
) -> Result<crate::capability::ContainerRealityProvider<'a>> {
    let runtime = crate::capability::ContainerRuntime::detect()?;
    crate::capability::ContainerRealityProvider::with_runtime(
        store,
        runtime,
        image.unwrap_or(crate::capability::DEFAULT_CONTAINER_IMAGE),
    )
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
    if let Commands::Assurance {
        command: assurance::AssuranceCommand::Verify { file },
    } = &cli.command
    {
        return Ok(Response::Assurance {
            result: assurance::verify_artifact(file)?,
        });
    }
    if matches!(
        cli.command,
        Commands::Bridge { .. }
            | Commands::Integrate { .. }
            | Commands::IntegrationEvent { .. }
            | Commands::Agent { .. }
            | Commands::Events { .. }
    ) {
        return Ok(Response::Integration {
            result: integrations::execute(cli, &home, cancel).await?,
        });
    }
    // Validate input before creating a database or touching a repository.
    let state = if matches!(
        cli.command,
        Commands::Run(_)
            | Commands::Lesson {
                command: LessonCommand::Search { .. } | LessonCommand::Test { .. }
            }
            | Commands::Reality {
                command: RealityCommand::Create { .. }
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
    if assurance::handles(&cli.command) {
        return Ok(Response::Assurance {
            result: assurance::execute(cli, &store)?,
        });
    }
    if tools::handles(&cli.command) {
        return Ok(Response::Tools {
            result: tools::execute(cli, &store, cancel).await?,
        });
    }
    if attestation::handles(&cli.command) {
        return Ok(Response::Attestations {
            result: attestation::execute(cli, &store)?,
        });
    }
    if capability::handles(&cli.command) {
        return Ok(Response::Capability {
            result: capability::execute(cli, &store)?,
        });
    }
    if effects::handles(&cli.command) {
        return Ok(Response::Effects {
            result: effects::execute(cli, &store)?,
        });
    }
    if federation::handles(&cli.command) {
        return Ok(Response::Federation {
            result: federation::execute(cli, &store, cancel).await?,
        });
    }
    if development::handles(&cli.command) {
        return Ok(Response::Development {
            result: development::execute(cli, &store, cancel).await?,
        });
    }
    if let Commands::Experiment {
        command: ExperimentCommand::List { agent },
    } = &cli.command
    {
        return Ok(Response::Experiments {
            experiments: if agent.is_none() {
                store.experiments()?
            } else {
                vec![]
            },
            strategy_experiments: crate::store::ExperimentStore::list(&store, agent.as_deref())?,
        });
    }
    if let Commands::Experiment {
        command: ExperimentCommand::Show { id },
    } = &cli.command
        && crate::store::ExperimentStore::get(&store, id)?.is_none()
    {
        return Ok(Response::Experiment {
            experiment: Box::new(store.experiment(id)?),
        });
    }
    if experimentation::handles(&cli.command) {
        return Ok(Response::Experimentation {
            result: Box::new(experimentation::execute(cli, &store, cancel).await?),
        });
    }
    let provider = GitRealityProvider::new(&store);
    match &cli.command {
        Commands::Contract { .. } | Commands::Assurance { .. } => {
            Err(Error::InvalidInput("Assurance dispatch failed".into()))
        }
        Commands::Tool { .. } | Commands::Attestation { .. } => Err(Error::InvalidInput(
            "Tool or attestation dispatch failed".into(),
        )),
        Commands::Capability { .. } => {
            Err(Error::InvalidInput("Capability dispatch failed".into()))
        }
        Commands::Effect { .. } => Err(Error::InvalidInput("Effect dispatch failed".into())),
        Commands::Peer { .. }
        | Commands::Federate { .. }
        | Commands::Provenance { .. }
        | Commands::Conflict { .. } => {
            Err(Error::InvalidInput("Federation dispatch failed".into()))
        }
        Commands::Profile { .. }
        | Commands::Growth(_)
        | Commands::Timeline(_)
        | Commands::Revalidation { .. }
        | Commands::Episode { .. }
        | Commands::Benchmark { .. }
        | Commands::Doctor => Err(Error::InvalidInput("Development dispatch failed".into())),
        Commands::Curriculum { .. }
        | Commands::TaskFamily { .. }
        | Commands::Skill {
            command: SkillCommand::Harden { .. } | SkillCommand::Package { .. },
        } => curriculum::execute(cli, &store, cancel).await,
        Commands::Bridge { .. }
        | Commands::Integrate { .. }
        | Commands::IntegrationEvent { .. }
        | Commands::Agent { .. }
        | Commands::Events { .. } => Err(Error::InvalidInput("Integration dispatch failed".into())),
        Commands::Chaos { .. }
        | Commands::Envelope { .. }
        | Commands::Reflex { .. }
        | Commands::Recovery { .. }
        | Commands::Skill { .. } => resilience::execute(cli, &store, cancel).await,
        Commands::Why { experience, .. } => Ok(Response::Why {
            explanation: Box::new(store.explain(experience.as_ref())?),
        }),
        Commands::Status => Ok(Response::Status {
            counts: store.status_counts()?,
        }),
        Commands::Run(args) => {
            if args.provider == RealityProviderChoice::GitWorktree {
                if args.capability_profile.is_some() || args.image.is_some() {
                    return Err(Error::Intervention(
                        "Git worktrees cannot enforce capability profiles; use --provider container"
                            .into(),
                    ));
                }
                warning(cli.json)?;
            }
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
            let request = RunRequest {
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
            };
            let learning = RunLearningOptions {
                enabled: !args.no_experience
                    && (args.agent.is_some() || args.with_experience || args.retry_with_experience),
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
            };
            if args.provider == RealityProviderChoice::Container {
                if args.retry_with_experience || args.max_retries != 1 {
                    return Err(Error::Intervention(
                        "V0.9 container runs record one isolated Experience; automatic retry/reflection orchestration is not yet routed through the execution proxy"
                            .into(),
                    ));
                }
                let manifest = crate::capability::builtin_profile(
                    args.capability_profile
                        .as_deref()
                        .unwrap_or("coding-offline"),
                )?;
                let run = crate::workflow::run_in_container(
                    &store,
                    request,
                    &learning,
                    manifest,
                    args.image.as_deref(),
                    cancel,
                )
                .await?;
                return Ok(Response::RunCompleted {
                    execution: Box::new(run.execution),
                    reality: Box::new(run.reality),
                    experience: Box::new(run.experience),
                    lesson: None,
                    experiment: None,
                    retries: vec![],
                    retry_stop_reason: "Container run completed without automatic retries".into(),
                    interrupted: false,
                });
            }
            let cycle = execute_learning_run(
                &store,
                request,
                LearningRunOptions {
                    experience_budget: args.experience_budget.map(|n| {
                        crate::bridge::protocol::ExperienceBudget {
                            max_realities: n as usize,
                            max_agent_runs: n as usize,
                            max_duration_ms: None,
                            max_commands_per_reality: None,
                            ..Default::default()
                        }
                    }),
                    learning,
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
            LessonCommand::History { .. } => {
                Err(Error::InvalidInput("Development dispatch failed".into()))
            }
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
                include_federated,
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
                    federated: if *include_federated {
                        store.search_federated(Some("lesson"),None)?.into_iter().filter(|o| !matches!(o.state,crate::federation::FederatedExperienceState::Rejected|crate::federation::FederatedExperienceState::Retired|crate::federation::FederatedExperienceState::LocallyContradicted)).collect()
                    } else {
                        vec![]
                    },
                })
            }
            LessonCommand::Retire { id, reason } => {
                let lesson = store.retire_lesson(id, reason.clone())?;
                Ok(Response::Lesson {
                    provenance: store.lesson_agent_provenance(&lesson.id)?,
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
                    provenance: store.lesson_agent_provenance(&lesson.id)?,
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
                    provenance: store.lesson_agent_provenance(&lesson.id)?,
                    lesson: Box::new(lesson),
                    hypothesis: Box::new(h),
                })
            }
        },
        Commands::Experiment { command } => match command {
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
            _ => Err(Error::InvalidInput("Experiment dispatch failed".into())),
        },
        Commands::Experience { command } => match command {
            ExperienceCommand::Health(_) | ExperienceCommand::Maintain(_) => {
                Err(Error::InvalidInput("Development dispatch failed".into()))
            }
            ExperienceCommand::List => Ok(Response::Experiences {
                experiences: ExperienceStore::list(&store, ExperienceQuery::default())?,
            }),
            ExperienceCommand::Show { id } => Ok(Response::Experience {
                experience: Box::new(store.experience(id)?),
            }),
        },
        Commands::Reality { command } => match command {
            RealityCommand::Tree | RealityCommand::Export { .. } => {
                Err(Error::InvalidInput("Reality dispatch failed".into()))
            }
            RealityCommand::Create {
                provider: selected,
                profile,
                image,
            } => {
                let state = state
                    .as_ref()
                    .ok_or_else(|| Error::InvalidInput("Missing starting state".into()))?;
                let reality = match selected {
                    RealityProviderChoice::GitWorktree => {
                        if profile.is_some() || image.is_some() {
                            return Err(Error::Intervention(
                                "Git worktrees cannot enforce capability profiles; use --provider container"
                                    .into(),
                            ));
                        }
                        warning(cli.json)?;
                        provider.create(state)?
                    }
                    RealityProviderChoice::Container => {
                        let manifest = crate::capability::builtin_profile(
                            profile.as_deref().unwrap_or("coding-offline"),
                        )?;
                        let container = container_provider(&store, image.as_deref())?;
                        let mut reality =
                            crate::capability::IsolatedRealityProvider::create_with_capabilities(
                                &container, state, &manifest,
                            )?;
                        let token = match capability::issue_reality_token(&store, &reality)
                            .and_then(|token| {
                                capability::publish_reality_token(&store, &reality, &token)?;
                                Ok(token)
                            }) {
                            Ok(token) => token,
                            Err(primary) => {
                                let cleanup = container.discard(&mut reality);
                                return match cleanup {
                                    Ok(()) => Err(primary),
                                    Err(cleanup) => Err(Error::Cleanup {
                                        primary: Box::new(primary),
                                        cleanup: Box::new(cleanup),
                                    }),
                                };
                            }
                        };
                        tracing::debug!(
                            reality_id = %reality.id,
                            token_id = %token.claims.id,
                            "Issued scoped Reality capability token"
                        );
                        reality
                    }
                };
                Ok(Response::Reality {
                    reality,
                    effects: None,
                })
            }
            RealityCommand::List => Ok(Response::Realities {
                realities: store.realities()?,
            }),
            RealityCommand::Show { id } => {
                let entries = crate::store::EffectStore::effects(&store, Some(id))?;
                Ok(Response::Reality {
                    reality: store.reality(id)?,
                    effects: Some(serde_json::json!({
                        "proposed":entries.iter().filter(|effect| matches!(effect.lifecycle,crate::effects::EffectLifecycle::Proposed|crate::effects::EffectLifecycle::Classified|crate::effects::EffectLifecycle::Virtualized)).count(),
                        "prepared":entries.iter().filter(|effect| effect.lifecycle==crate::effects::EffectLifecycle::Prepared).count(),
                        "committed":entries.iter().filter(|effect| effect.lifecycle==crate::effects::EffectLifecycle::Committed).count(),
                        "discarded":entries.iter().filter(|effect| effect.lifecycle==crate::effects::EffectLifecycle::Discarded).count(),
                        "unknown":entries.iter().filter(|effect| effect.lifecycle==crate::effects::EffectLifecycle::Unknown).count(),
                    })),
                })
            }
            RealityCommand::Inspect { id } => {
                let reality = store.reality(id)?;
                let effects = crate::store::EffectStore::effects(&store, Some(id))?;
                let (manifest, events, credentials) =
                    if reality.execution_boundary.manifest_id.is_some() {
                        (
                            Some(store.effective_capability_manifest(id)?),
                            store.capability_events(Some(id))?,
                            store.issued_credentials(id)?,
                        )
                    } else {
                        (None, vec![], vec![])
                    };
                let runtime = if reality.execution_boundary.provider == "container" {
                    Some(
                        store
                            .provider_runtime::<crate::capability::ContainerRuntimeMetadata>(id)?,
                    )
                } else {
                    None
                };
                let processes = if let Some(runtime) = &runtime {
                    let provider = crate::capability::ContainerRealityProvider::with_runtime(
                        &store,
                        crate::capability::ContainerRuntime::named(&runtime.runtime)?,
                        &runtime.image,
                    )?;
                    match provider.processes(&reality) {
                        Ok(processes) => serde_json::json!({"observed":true,"listing":processes}),
                        Err(error) => {
                            serde_json::json!({"observed":false,"reason":error.to_string()})
                        }
                    }
                } else {
                    serde_json::json!({"observed":false,"reason":"provider has no isolated process namespace"})
                };
                let filesystem_diff = if reality.status != RealityStatus::Discarded {
                    let diff = if reality.execution_boundary.provider == "container" {
                        crate::capability::ContainerRealityProvider::with_runtime(
                            &store,
                            crate::capability::ContainerRuntime::named(
                                &runtime.as_ref().expect("container runtime").runtime,
                            )?,
                            &runtime.as_ref().expect("container runtime").image,
                        )?
                        .diff(&reality)
                    } else {
                        provider.diff(&reality)
                    };
                    match diff {
                        Ok(bytes) => {
                            let maximum = 256 * 1024;
                            let truncated = bytes.len() > maximum;
                            let visible = &bytes[..bytes.len().min(maximum)];
                            serde_json::json!({
                                "available":true,
                                "bytes":bytes.len(),
                                "truncated":truncated,
                                "patch":String::from_utf8_lossy(visible)
                            })
                        }
                        Err(error) => {
                            serde_json::json!({"available":false,"reason":error.to_string()})
                        }
                    }
                } else {
                    serde_json::json!({"available":false,"reason":"Reality is discarded; inspect saved diff artifacts"})
                };
                let violations: Vec<_> = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event.kind,
                            crate::capability::CapabilityEventKind::Denied
                                | crate::capability::CapabilityEventKind::ApprovalRequired
                        )
                    })
                    .collect();
                Ok(Response::Capability {
                    result: serde_json::json!({
                        "reality":reality,
                        "runtime":runtime,
                        "manifest":manifest,
                        "network_policy":manifest.as_ref().map(|manifest| &manifest.network),
                        "processes":processes,
                        "pending_effects":effects.iter().filter(|effect| effect.lifecycle == crate::effects::EffectLifecycle::Prepared).collect::<Vec<_>>(),
                        "issued_credentials":credentials,
                        "violations":violations,
                        "capability_events":events,
                        "filesystem_diff":filesystem_diff
                    }),
                })
            }
            RealityCommand::Freeze { id } => {
                let _lease = store.lock_reality(id)?;
                let mut reality = store.reality(id)?;
                if reality.execution_boundary.provider != "container" {
                    return Err(Error::Intervention(
                        "Freeze requires a container Reality".into(),
                    ));
                }
                container_provider(&store, None)?.freeze(&mut reality)?;
                Ok(Response::Reality {
                    reality,
                    effects: None,
                })
            }
            RealityCommand::Execute { id, command } => {
                let _lease = store.lock_reality(id)?;
                let reality = store.reality(id)?;
                if reality.execution_boundary.provider != "container" {
                    return Err(Error::Intervention(
                        "Capability execution proxy requires a container Reality".into(),
                    ));
                }
                let (program, args) = command
                    .split_first()
                    .ok_or_else(|| Error::InvalidInput("Command is required".into()))?;
                let token = capability::issue_reality_token(&store, &reality)?;
                capability::publish_reality_token(&store, &reality, &token)?;
                let proxy = crate::capability::CapabilityExecutionProxy::new(
                    &store,
                    crate::capability::SecretRedactor::default(),
                )?;
                let artifacts = store
                    .home
                    .join("artifacts")
                    .join(format!("capability-action-{}", uuid::Uuid::new_v4()));
                let result = crate::capability::ToolExecutionProxy::execute(
                    &proxy,
                    &reality,
                    &token,
                    &crate::capability::NormalizedAction::Shell(CommandSpec {
                        program: program.clone(),
                        args: args.to_vec(),
                        environment: EnvironmentMode::Controlled,
                        environment_overrides: Default::default(),
                    }),
                    &artifacts,
                )
                .await?;
                let crate::capability::ActionResult::Process { status, action } = result else {
                    return Err(Error::InvalidInput(
                        "Shell proxy returned a non-process result".into(),
                    ));
                };
                let stdout = String::from_utf8_lossy(&fs::read(&action.stdout.path)?).into_owned();
                let stderr = String::from_utf8_lossy(&fs::read(&action.stderr.path)?).into_owned();
                Ok(Response::Capability {
                    result: serde_json::json!({
                        "reality":id,
                        "status":status,
                        "action":action,
                        "stdout":stdout,
                        "stderr":stderr
                    }),
                })
            }
            RealityCommand::Fork { id } => {
                let _lease = store.lock_reality(id)?;
                let original = store.reality(id)?;
                let reality = if original.execution_boundary.provider == "container" {
                    let mut reality = container_provider(&store, None)?.fork(&original)?;
                    let token = capability::issue_reality_token(&store, &reality)?;
                    capability::publish_reality_token(&store, &reality, &token)?;
                    reality.parent = Some(original.id);
                    store.update_reality(&reality)?;
                    reality
                } else {
                    warning(cli.json)?;
                    provider.fork(&original)?
                };
                Ok(Response::Reality {
                    reality,
                    effects: None,
                })
            }
            RealityCommand::Diff { id } => {
                let _lease = store.lock_reality(id)?;
                let reality = store.reality(id)?;
                let patch = if reality.execution_boundary.provider == "container" {
                    container_provider(&store, None)?.diff(&reality)?
                } else {
                    provider.diff(&reality)?
                };
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
                crate::effects::EffectManager::new(&store)?.discard_reality(id)?;
                if reality.execution_boundary.provider == "container" {
                    container_provider(&store, None)?.discard(&mut reality)?;
                } else {
                    provider.discard(&mut reality)?;
                }
                Ok(Response::Reality {
                    reality,
                    effects: None,
                })
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
                    crate::effects::EffectManager::new(&store)?.discard_reality(&reality.id)?;
                    if reality.execution_boundary.provider == "container" {
                        container_provider(&store, None)?.discard(&mut reality)?;
                    } else {
                        provider.discard(&mut reality)?;
                    }
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
        Commands::Try(_) => Err(Error::InvalidInput("Experiment dispatch failed".into())),
    }
}
