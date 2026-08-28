// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Response, warning};
use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::*,
    dojo::capture_state,
    evaluation::EvaluationSpec,
    experience::{EnvironmentContext, fixture_metadata},
    perturbation::{Perturbation, PerturbationParameters},
    resilience::{campaign, fixture, testing, *},
    store::Store,
};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::io::Write;

#[derive(Debug, Subcommand)]
pub enum ChaosCommand {
    Run(ChaosArgs),
    List,
    Show { id: ChaosCampaignId },
    Report { id: ChaosCampaignId },
}
#[derive(Debug, Subcommand)]
pub enum EnvelopeCommand {
    List,
    Show { id: OperatingEnvelopeId },
}
#[derive(Debug, Subcommand)]
pub enum ReflexCommand {
    List,
    Show {
        id: ReflexId,
    },
    Test {
        id: ReflexId,
        #[arg(long = "perturb")]
        perturbations: Vec<String>,
    },
    Enable {
        id: ReflexId,
    },
    Disable {
        id: ReflexId,
    },
}
#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    List,
    Show { id: RecoveryId },
    Test { id: RecoveryId },
}
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    List,
    Show {
        name: String,
    },
    Register {
        name: String,
        #[arg(long)]
        experience: ExperienceId,
    },
}

#[derive(Debug, Args)]
pub struct ChaosArgs {
    #[arg(long, value_enum, conflicts_with = "skill")]
    fixture: Option<FixtureKind>,
    #[arg(long, conflicts_with_all=["fixture","command","agent"])]
    skill: Option<String>,
    #[arg(long, value_parser=["test-agent"], conflicts_with="command")]
    agent: Option<String>,
    #[arg(long, alias="script", conflicts_with_all=["fixture","agent","skill"], help="Trusted shell script to perturb as a top-level Command")]
    command: Option<String>,
    #[arg(long = "check")]
    checks: Vec<String>,
    #[arg(long = "perturb")]
    perturbations: Vec<String>,
    #[arg(long, help = "Explicit delay points, e.g. delay=0,100,500,1000,2000")]
    perturb_sweep: Option<String>,
    #[arg(long, value_parser=["latency","command-failure","config-drift","credential"])]
    profile: Option<String>,
    #[arg(
        long,
        default_value_t = 10,
        help = "Maximum perturbed trials; control is one additional run"
    )]
    trials: usize,
    #[arg(
        long,
        default_value_t = 300,
        help = "Campaign dispatch deadline in seconds; current bounded run may finish"
    )]
    max_duration: u64,
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
    #[arg(default_value = "exercise known-good behavior")]
    task: String,
}
pub fn parse_perturbation(value: &str) -> Result<Perturbation> {
    let (kind, value) = value.split_once(':').ok_or_else(|| {
        Error::InvalidInput(
            "Use delay:100ms, command-failure:once|N, env:KEY=VALUE, or file:relative-path=content"
                .into(),
        )
    })?;
    let number = |v: &str| {
        v.parse::<u64>().map_err(|_| {
            Error::InvalidInput("Perturbation value must be a nonnegative integer".into())
        })
    };
    let parameters = match kind {
        "delay" => PerturbationParameters::CommandDelay {
            milliseconds: number(value.strip_suffix("ms").unwrap_or(value))?,
        },
        "command-failure" => PerturbationParameters::CommandFailure {
            failures: match value {
                "once" => 1,
                "always" => 6,
                _ => u32::try_from(number(value)?)
                    .map_err(|_| Error::InvalidInput("Too many failures".into()))?,
            },
            exit_code: 17,
        },
        "env" => {
            let (key, value) = value.split_once('=').ok_or_else(|| {
                Error::InvalidInput("Environment perturbation requires KEY=VALUE".into())
            })?;
            PerturbationParameters::EnvironmentVariable {
                key: key.into(),
                value: value.into(),
            }
        }
        "file" => {
            let (path, content) = value.split_once('=').ok_or_else(|| {
                Error::InvalidInput("File perturbation requires relative-path=content".into())
            })?;
            PerturbationParameters::FileMutation {
                path: path.into(),
                content: content.into(),
            }
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "Unsupported local perturbation: {kind}"
            )));
        }
    };
    let p = Perturbation::new(parameters);
    p.validate()?;
    Ok(p)
}
fn conditions(args: &ChaosArgs) -> Result<Vec<Vec<Perturbation>>> {
    let mut values = args.perturbations.clone();
    if let Some(profile) = &args.profile {
        values.extend(
            match profile.as_str() {
                "latency" => vec![
                    "delay:0ms",
                    "delay:100ms",
                    "delay:500ms",
                    "delay:1000ms",
                    "delay:2000ms",
                ],
                "command-failure" => vec![
                    "command-failure:once",
                    "command-failure:3",
                    "command-failure:always",
                ],
                "config-drift" => vec!["file:generation=2"],
                "credential" => vec!["env:HK_TOKEN_STATE=STALE_TOKEN"],
                _ => unreachable!(),
            }
            .into_iter()
            .map(str::to_owned),
        );
    }
    if let Some(sweep) = &args.perturb_sweep {
        let numbers = sweep.strip_prefix("delay=").ok_or_else(|| {
            Error::InvalidInput("Only explicit delay=... sweeps are supported".into())
        })?;
        values.extend(numbers.split(',').map(|v| format!("delay:{v}ms")));
    }
    if values.is_empty() {
        return Err(Error::InvalidInput(
            "Select --profile, --perturb, or --perturb-sweep".into(),
        ));
    }
    values
        .iter()
        .map(|v| Ok(vec![parse_perturbation(v)?]))
        .collect()
}
fn detect_fixture(root: &std::path::Path) -> Result<FixtureKind> {
    let marker = fixture_metadata(root)?;
    match marker["kind"].as_str() {
        Some("retry-resilience") => Ok(FixtureKind::RetryResilience),
        Some("stale-credential") => Ok(FixtureKind::StaleCredential),
        Some("config-drift") => Ok(FixtureKind::ConfigDrift),
        _ => Err(Error::InvalidInput(
            "test-agent requires an initialized V0.2 resilience fixture".into(),
        )),
    }
}
async fn run(
    cli: &Cli,
    store: &Store,
    args: &ChaosArgs,
    cancel: &Cancellation,
) -> Result<ResilienceResponse> {
    let perturbations = conditions(args)?;
    let (state, fixture, target, command, goal) = if let Some(name) = &args.skill {
        let skill = store.skill(name)?;
        if !matches!(
            skill.status,
            SkillStatus::Supported | SkillStatus::Validated
        ) || skill.procedure.len() != 1
        {
            return Err(Error::Intervention(
                "Chaos requires a supported single-script Skill".into(),
            ));
        }
        let source = store.experience(&skill.source_experience)?;
        let fixture = if source.agent.kind == "test-agent" {
            Some(detect_fixture(&source.starting_state.repo_path)?)
        } else {
            None
        };
        let script = skill.procedure[0]
            .shell_script()
            .ok_or_else(|| Error::InvalidInput("Skill must contain a shell procedure".into()))?;
        (
            source.starting_state,
            fixture,
            ChaosTarget::Skill(skill.id),
            CommandSpec::shell(script, EnvironmentMode::Controlled),
            source.goal,
        )
    } else {
        let state = if let Some(kind) = args.fixture {
            fixture::materialize(store, kind)?
        } else {
            capture_state(&cli.repo)?
        };
        let fixture = if let Some(kind) = args.fixture {
            Some(kind)
        } else if args.agent.is_some() {
            Some(detect_fixture(&state.repo_path)?)
        } else {
            None
        };
        let script = args
            .command
            .clone()
            .unwrap_or_else(|| "/bin/sh ./operation.sh".into());
        if fixture.is_none() && args.command.is_none() {
            return Err(Error::InvalidInput(
                "Choose --fixture, --skill, --agent test-agent, or --command".into(),
            ));
        }
        let command = CommandSpec::shell(&script, EnvironmentMode::Controlled);
        let target = if args.command.is_some() {
            ChaosTarget::Command(command.clone())
        } else {
            ChaosTarget::Task(args.task.clone())
        };
        (state, fixture, target, command, args.task.clone())
    };
    if store.home.starts_with(&state.repo_path) {
        return Err(Error::Intervention(
            "Hardknock home must be outside the source repository".into(),
        ));
    }
    let checks = if args.checks.is_empty() {
        if let ChaosTarget::Skill(id) = &target {
            store
                .experience(&store.skill(&id.to_string())?.source_experience)?
                .evaluation
                .spec
                .checks
        } else if fixture.is_some() {
            vec!["/bin/sh ./test.sh".into()]
        } else {
            vec![]
        }
    } else {
        args.checks.clone()
    };
    let environment = EnvironmentContext::capture(&state.repo_path, EnvironmentMode::Controlled)?;
    let active_reflexes = if fixture.is_some() {
        store
            .reflexes()?
            .into_iter()
            .filter(|r| r.status == ReflexStatus::Active)
            .collect()
    } else {
        vec![]
    };
    let plan = CampaignPlan {
        target,
        starting_state: state,
        goal,
        command,
        evaluation: EvaluationSpec { checks },
        agent: AgentIdentity {
            kind: if fixture.is_some() {
                "test-agent"
            } else {
                "script"
            }
            .into(),
            executable: "/bin/sh".into(),
            version: Some(fixture::RUNTIME_VERSION.into()),
            model: None,
        },
        fixture,
        perturbations,
        trial_budget: args.trials,
        timeout_secs: args.timeout_secs,
        max_duration_secs: args.max_duration,
        environment,
        hardknock_version: env!("CARGO_PKG_VERSION").into(),
        runtime_version: fixture::RUNTIME_VERSION.into(),
        fixture_version: fixture.map(|_| "1".into()),
        active_reflexes,
    };
    campaign::validate(&plan)?;
    warning(cli.json)?;
    let progress = |event: &campaign::CampaignEvent| -> Result<()> {
        if cli.json {
            let mut stderr = std::io::stderr().lock();
            serde_json::to_writer(&mut stderr, event)?;
            writeln!(stderr)?;
        }
        Ok(())
    };
    Ok(ResilienceResponse::Campaign {
        campaign: Box::new(campaign::run_observed(store, plan, cancel, Some(&progress)).await?),
    })
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResilienceResponse {
    Campaign {
        campaign: Box<ChaosCampaign>,
    },
    Campaigns {
        campaigns: Vec<ChaosCampaign>,
    },
    Report {
        campaign_id: ChaosCampaignId,
        metrics: serde_json::Value,
    },
    Envelope {
        envelope: Box<OperatingEnvelope>,
    },
    Envelopes {
        envelopes: Vec<OperatingEnvelope>,
    },
    Reflex {
        reflex: Box<Reflex>,
        tests: Vec<ResilienceTest>,
    },
    Reflexes {
        reflexes: Vec<Reflex>,
    },
    Recovery {
        recovery: Box<Recovery>,
        tests: Vec<ResilienceTest>,
    },
    Recoveries {
        recoveries: Vec<Recovery>,
    },
    Test {
        test: Box<ResilienceTest>,
    },
    Skill {
        skill: Box<Skill>,
    },
    Skills {
        skills: Vec<Skill>,
    },
}
pub async fn execute(cli: &Cli, store: &Store, cancel: &Cancellation) -> Result<Response> {
    use super::Commands;
    let result =
        match &cli.command {
            Commands::Chaos { command } => match command {
                ChaosCommand::Run(args) => run(cli, store, args, cancel).await?,
                ChaosCommand::List => ResilienceResponse::Campaigns {
                    campaigns: store.campaigns()?,
                },
                ChaosCommand::Show { id } => ResilienceResponse::Campaign {
                    campaign: Box::new(store.campaign(id)?),
                },
                ChaosCommand::Report { id } => ResilienceResponse::Report {
                    campaign_id: id.clone(),
                    metrics: report(store, &store.campaign(id)?)?,
                },
            },
            Commands::Envelope { command } => match command {
                EnvelopeCommand::List => ResilienceResponse::Envelopes {
                    envelopes: store.envelopes()?,
                },
                EnvelopeCommand::Show { id } => ResilienceResponse::Envelope {
                    envelope: Box::new(store.envelope(id)?),
                },
            },
            Commands::Reflex { command } => match command {
                ReflexCommand::List => ResilienceResponse::Reflexes {
                    reflexes: store.reflexes()?,
                },
                ReflexCommand::Show { id } => ResilienceResponse::Reflex {
                    reflex: Box::new(store.reflex(id)?),
                    tests: store
                        .resilience_tests()?
                        .into_iter()
                        .filter(|t| t.reflex_id.as_ref() == Some(id))
                        .collect(),
                },
                ReflexCommand::Enable { id } | ReflexCommand::Disable { id } => {
                    ResilienceResponse::Reflex {
                        reflex: Box::new(store.set_reflex_enabled(
                            id,
                            matches!(command, ReflexCommand::Enable { .. }),
                        )?),
                        tests: vec![],
                    }
                }
                ReflexCommand::Test { id, perturbations } => {
                    warning(cli.json)?;
                    let conditions = if perturbations.is_empty() {
                        None
                    } else {
                        Some(
                            perturbations
                                .iter()
                                .map(|p| parse_perturbation(p))
                                .collect::<Result<_>>()?,
                        )
                    };
                    ResilienceResponse::Test {
                        test: Box::new(testing::test_reflex(store, id, conditions, cancel).await?),
                    }
                }
            },
            Commands::Recovery { command } => match command {
                RecoveryCommand::List => ResilienceResponse::Recoveries {
                    recoveries: store.recoveries()?,
                },
                RecoveryCommand::Show { id } => ResilienceResponse::Recovery {
                    recovery: Box::new(store.recovery(id)?),
                    tests: store
                        .resilience_tests()?
                        .into_iter()
                        .filter(|t| t.recovery_id.as_ref() == Some(id))
                        .collect(),
                },
                RecoveryCommand::Test { id } => {
                    warning(cli.json)?;
                    ResilienceResponse::Test {
                        test: Box::new(testing::test_recovery(store, id, cancel).await?),
                    }
                }
            },
            Commands::Skill { command } => match command {
                SkillCommand::List => ResilienceResponse::Skills {
                    skills: store.skills()?,
                },
                SkillCommand::Show { name } => ResilienceResponse::Skill {
                    skill: Box::new(store.skill(name)?),
                },
                SkillCommand::Register { name, experience } => ResilienceResponse::Skill {
                    skill: Box::new(store.register_skill(name, experience)?),
                },
            },
            _ => return Err(Error::InvalidInput("Not a resilience command".into())),
        };
    Ok(Response::Resilience {
        result: Box::new(result),
    })
}
pub fn report(store: &Store, campaign: &ChaosCampaign) -> Result<serde_json::Value> {
    let count = |outcome| {
        campaign
            .trials
            .iter()
            .filter(|t| t.outcome == outcome)
            .count()
    };
    let ids = campaign.trials.iter().map(|t| &t.id).collect::<Vec<_>>();
    let tests = store
        .resilience_tests()?
        .into_iter()
        .filter(|t| ids.contains(&&t.source_trial))
        .collect::<Vec<_>>();
    let fired = tests.iter().filter(|t| t.false_positive.is_some()).count();
    let false_positives = tests
        .iter()
        .filter(|t| t.false_positive == Some(true))
        .count();
    let recovery_attempts = tests
        .iter()
        .filter_map(|t| t.with.as_ref())
        .map(|id| store.experience(id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|e| e.resilience.and_then(|r| r.recovery_attempt))
        .filter(|a| a.attempted)
        .collect::<Vec<_>>();
    let recovered = recovery_attempts.iter().filter(|a| a.succeeded).count();
    let ratio = |n: usize, d: usize| {
        if d == 0 {
            None
        } else {
            Some(n as f64 / d as f64)
        }
    };
    Ok(
        json!({"status":campaign.result,"trials":campaign.trials.len(),"pass":count(ChaosTrialOutcome::Pass),"degraded":count(ChaosTrialOutcome::Degraded),"fail":count(ChaosTrialOutcome::Fail),"inconclusive":count(ChaosTrialOutcome::Inconclusive),"task_success_rate":ratio(count(ChaosTrialOutcome::Pass)+count(ChaosTrialOutcome::Degraded),campaign.trials.len()),"repeated_mistake_rate":null,"lesson_audited_trials":0,"retry_count":campaign.trials.iter().map(|t|t.metrics.retries as u64).sum::<u64>(),"failed_attempts":campaign.trials.iter().map(|t|t.metrics.failed_attempts as u64).sum::<u64>(),"failure_detection_clock":if campaign.plan.fixture.is_some(){"simulated"}else{"wall"},"failure_signatures":campaign.trials.iter().flat_map(|t|t.failure_signatures.clone()).collect::<Vec<_>>(),"failure_detection_ms":campaign.trials.iter().filter_map(|t|t.metrics.failure_detection_ms).collect::<Vec<_>>(),"new_lessons":campaign.trials.iter().map(|t|t.lessons.len()).sum::<usize>(),"reflex_candidates_created":campaign.trials.iter().map(|t|t.reflexes.len()).sum::<usize>(),"recovery_candidates_created":campaign.trials.iter().map(|t|t.recoveries.len()).sum::<usize>(),"false_positive_reflexes":false_positives,"paired_reflex_firings":fired,"false_positive_reflex_rate":ratio(false_positives,fired),"recovery_attempts":recovery_attempts.len(),"recovery_successes":recovered,"recovery_success_rate":ratio(recovered,recovery_attempts.len()),"envelope_tested_points":campaign.trials.len(),"envelope_total_space_coverage":null,"unknown":"all untested conditions"}),
    )
}
impl ResilienceResponse {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Campaign { campaign } => match campaign.result {
                CampaignStatus::Completed => 0,
                CampaignStatus::Interrupted => 5,
                CampaignStatus::Failed => 2,
                _ => 3,
            },
            Self::Test { test } => match test.status {
                ResilienceTestStatus::Supported
                | ResilienceTestStatus::FalsePositive
                | ResilienceTestStatus::Contradicted => 0,
                ResilienceTestStatus::Failed => 2,
                _ => 3,
            },
            _ => 0,
        }
    }
    pub fn print(&self, out: &mut impl Write) -> Result<()> {
        match self {
            Self::Campaign { campaign } => {
                writeln!(out, "Chaos {} · {:?}", campaign.id, campaign.result)?;
                if let Some(control) = &campaign.control {
                    writeln!(
                        out,
                        "Control  {:?} · {}",
                        control.outcome, control.experience_id
                    )?;
                }
                for trial in &campaign.trials {
                    writeln!(
                        out,
                        "  {}  {:?} · {}",
                        conditions_label(&trial.perturbations),
                        trial.outcome,
                        trial.experience_id
                    )?;
                    if !trial.failure_signatures.is_empty() {
                        writeln!(
                            out,
                            "    signatures: {}",
                            trial.failure_signatures.join(", ")
                        )?;
                    }
                    for id in &trial.lessons {
                        writeln!(out, "    Candidate Lesson {id}")?;
                    }
                    for id in &trial.reflexes {
                        writeln!(out, "    Candidate Reflex {id}")?;
                    }
                    for id in &trial.recoveries {
                        writeln!(out, "    Candidate Recovery {id}")?;
                    }
                }
                if let Some(id) = &campaign.envelope_id {
                    writeln!(out, "Operating envelope: {id}")?;
                }
                if let Some(reason) = &campaign.stop_reason {
                    writeln!(out, "{reason}")?;
                }
            }
            Self::Envelope { envelope } => {
                writeln!(out, "{} · campaign {}", envelope.id, envelope.campaign_id)?;
                for c in &envelope.tested_conditions {
                    writeln!(
                        out,
                        "  {}  {:?}",
                        conditions_label(&c.perturbations),
                        c.outcome
                    )?;
                }
                writeln!(
                    out,
                    "Unknown: all untested conditions. No interpolation or extrapolation."
                )?;
            }
            Self::Test { test } => {
                writeln!(out, "{} · {:?}\n{}", test.id, test.status, test.reason)?;
                writeln!(out, "Without: {:?}\nWith: {:?}", test.without, test.with)?;
            }
            Self::Reflex { reflex, tests } => {
                writeln!(
                    out,
                    "{} · {:?} · confidence {:.2}\nTrigger: {}\nResponse: {:?}\nSource Lessons: {:?}\nSource trial: {}\nPaired tests: {}",
                    reflex.id,
                    reflex.status,
                    f64::from(reflex.confidence),
                    serde_json::to_string(&reflex.trigger)?,
                    reflex.response,
                    reflex.source_lessons,
                    reflex.source_trial,
                    tests.len()
                )?;
            }
            Self::Recovery { recovery, tests } => {
                writeln!(
                    out,
                    "{} · {:?} · confidence {:.2}\nFailure: {}\nScope: {}\nSource trial: {}\nPaired tests: {}",
                    recovery.id,
                    recovery.status,
                    f64::from(recovery.confidence),
                    recovery.failure_signature.signature,
                    serde_json::to_string(&recovery.context)?,
                    recovery.source_trial,
                    tests.len()
                )?;
                for (i, step) in recovery.steps.iter().enumerate() {
                    writeln!(out, "  {}. {}", i + 1, serde_json::to_string(step)?)?;
                }
            }
            Self::Campaigns { campaigns } => {
                for c in campaigns {
                    writeln!(out, "{} · {:?} · {} trials", c.id, c.result, c.trials.len())?;
                }
            }
            Self::Envelopes { envelopes } => {
                for e in envelopes {
                    writeln!(
                        out,
                        "{} · {} tested points · {}",
                        e.id,
                        e.tested_conditions.len(),
                        e.campaign_id
                    )?;
                }
            }
            Self::Reflexes { reflexes } => {
                for r in reflexes {
                    writeln!(out, "{} · {:?} · {:?}", r.id, r.status, r.response)?;
                }
            }
            Self::Recoveries { recoveries } => {
                for r in recoveries {
                    writeln!(
                        out,
                        "{} · {:?} · {}",
                        r.id, r.status, r.failure_signature.signature
                    )?;
                }
            }
            Self::Skills { skills } => {
                for s in skills {
                    writeln!(out, "{} · {} · {:?}", s.id, s.name, s.status)?;
                }
            }
            Self::Skill { skill } => writeln!(
                out,
                "{} · {} · {:?}\nProcedure: {:?}",
                skill.id, skill.name, skill.status, skill.procedure
            )?,
            Self::Report {
                campaign_id,
                metrics,
            } => writeln!(
                out,
                "Chaos report {campaign_id}\n{}",
                serde_json::to_string_pretty(metrics)?
            )?,
        }
        Ok(())
    }
}
fn conditions_label(conditions: &[Perturbation]) -> String {
    conditions
        .iter()
        .map(|p| match &p.parameters {
            PerturbationParameters::CommandDelay { milliseconds } => {
                format!("delay={milliseconds}ms")
            }
            PerturbationParameters::CommandFailure { failures, .. } => {
                format!("command-failure={failures}")
            }
            PerturbationParameters::EnvironmentVariable { key, .. } => format!("env:{key}"),
            PerturbationParameters::FileMutation { path, .. } => format!("file:{}", path.display()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
