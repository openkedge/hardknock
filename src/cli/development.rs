// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands, ExperienceCommand, LessonCommand, resilience::SkillCommand};
use crate::{
    Error, Result, bridge::config::Config, cancellation::Cancellation, core::*, development::*,
    dojo::capture_state, experience::ExperienceContext, store::Store,
};
use chrono::{DateTime, Duration, Utc};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

#[derive(Debug, Args, Default)]
pub struct SubjectArgs {
    #[arg(long, conflicts_with_all=["task_family","shared"])]
    pub agent: Option<String>,
    #[arg(long, requires = "agent")]
    pub agent_version: Option<String>,
    #[arg(long, requires = "agent")]
    pub model: Option<String>,
    #[arg(long, conflicts_with = "shared")]
    pub task_family: Option<String>,
    #[arg(long)]
    pub shared: bool,
}
impl SubjectArgs {
    pub fn subject(&self, cli: &Cli, store: &Store) -> Result<ExperienceSubject> {
        Ok(if let Some(kind) = &self.agent {
            ExperienceSubject::Agent(AgentSubject {
                agent_kind: kind.clone(),
                agent_version: self.agent_version.clone(),
                model: self.model.clone(),
                configuration: None,
                profile_scope: ProfileScope::LocalStore,
            })
        } else if let Some(name) = &self.task_family {
            ExperienceSubject::TaskFamily(store.task_family(name)?.id)
        } else if self.shared {
            ExperienceSubject::SharedLocal
        } else {
            ExperienceSubject::Repository(fs::canonicalize(&cli.repo)?)
        })
    }
}
#[derive(Debug, Args, Default)]
pub struct ProfileArgs {
    #[command(flatten)]
    pub subject: SubjectArgs,
    #[arg(long, conflicts_with_all=["last_days","last_experiences"])]
    pub since: Option<String>,
    #[arg(long, conflicts_with = "last_experiences")]
    pub last_days: Option<u32>,
    #[arg(long)]
    pub last_experiences: Option<u64>,
}
impl ProfileArgs {
    fn window(&self) -> Result<ProfileWindow> {
        Ok(if let Some(s) = &self.since {
            ProfileWindow::Since(parse_time(s)?)
        } else if let Some(n) = self.last_days {
            ProfileWindow::LastDays(n)
        } else if let Some(n) = self.last_experiences {
            ProfileWindow::LastExperiences(n)
        } else {
            ProfileWindow::AllTime
        })
    }
}
#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Show(ProfileArgs),
    Rebuild(ProfileArgs),
    Snapshot(ProfileArgs),
    History(SubjectArgs),
    Gaps(ProfileArgs),
    Compare {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[command(flatten)]
        subject: SubjectArgs,
    },
    Export {
        #[command(flatten)]
        args: ProfileArgs,
        #[arg(long)]
        output: PathBuf,
    },
}
#[derive(Debug, Args)]
pub struct TimelineArgs {
    #[arg(long, conflicts_with = "lesson")]
    skill: Option<String>,
    #[arg(long)]
    lesson: Option<LessonId>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u32).range(1..=20000))]
    limit: u32,
}
#[derive(Debug, Subcommand)]
pub enum RevalidationCommand {
    List,
    Run { id: RevalidationId },
}
#[derive(Debug, Subcommand)]
pub enum EpisodeCommand {
    Start {
        name: String,
        #[command(flatten)]
        subject: SubjectArgs,
    },
    Finish {
        id: DevelopmentEpisodeId,
    },
    List,
}
#[derive(Debug, Subcommand)]
pub enum BenchmarkCommand {
    Longitudinal {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    List,
}
#[derive(Debug, Subcommand)]
pub enum PackageCommand {
    History {
        name: String,
        #[arg(long, default_value = "resilience-basic")]
        profile: String,
    },
    Diff {
        name: String,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
        #[arg(long, default_value = "resilience-basic")]
        profile: String,
    },
    Export {
        name: String,
        #[arg(long)]
        revision: Option<u64>,
        #[arg(long, default_value = "resilience-basic")]
        profile: String,
        #[arg(long)]
        output: PathBuf,
    },
}
pub fn handles(c: &Commands) -> bool {
    matches!(
        c,
        Commands::Profile { .. }
            | Commands::Growth(_)
            | Commands::Timeline(_)
            | Commands::Revalidation { .. }
            | Commands::Episode { .. }
            | Commands::Benchmark { .. }
            | Commands::Doctor
            | Commands::Experience {
                command: ExperienceCommand::Health(_) | ExperienceCommand::Maintain(_)
            }
            | Commands::Lesson {
                command: LessonCommand::History { .. }
            }
            | Commands::Skill {
                command: SkillCommand::History { .. }
                    | SkillCommand::Revise { .. }
                    | SkillCommand::Package {
                        command: Some(_),
                        ..
                    }
            }
    )
}
pub fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    if let Some(n) = s.strip_suffix('d') {
        let n = n
            .parse::<i64>()
            .map_err(|_| Error::InvalidInput("Use positive Nd, YYYY-MM-DD, or RFC3339".into()))?;
        if !(1..=36500).contains(&n) {
            return Err(Error::InvalidInput("Days out of range".into()));
        }
        return Ok(Utc::now() - Duration::days(n));
    }
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_rfc3339(&format!("{s}T00:00:00Z")))
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| Error::InvalidInput("Use positive Nd, YYYY-MM-DD, or RFC3339".into()))
}
pub fn export(path: &PathBuf, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    file.sync_all()?;
    Ok(())
}
fn build(
    store: &Store,
    cfg: &Config,
    subject: &ExperienceSubject,
    window: ProfileWindow,
) -> Result<ExperienceProfile> {
    EvidenceProfileBuilder {
        store,
        config: &cfg.development,
        now: Utc::now(),
        context: None,
    }
    .build(subject, window)
}
pub async fn execute(cli: &Cli, store: &Store, cancel: &Cancellation) -> Result<Value> {
    let cfg = Config::load(&store.home)?;
    Ok(match &cli.command {
        Commands::Profile { command } => match command {
            ProfileCommand::Show(a)
            | ProfileCommand::Rebuild(a)
            | ProfileCommand::Snapshot(a)
            | ProfileCommand::Gaps(a)
            | ProfileCommand::Export { args: a, .. } => {
                let p = build(store, &cfg, &a.subject.subject(cli, store)?, a.window()?)?;
                match command {
                    ProfileCommand::Snapshot(_) => {
                        json!({"kind":"snapshot","snapshot":store.save_profile_snapshot(&p)?})
                    }
                    ProfileCommand::Gaps(_) => {
                        json!({"kind":"gaps","coverage":p.coverage,"health":p.freshness,"recommendation":"Inspect UNKNOWN conditions, then explicitly plan a bounded curriculum","auto_run":false})
                    }
                    ProfileCommand::Export { output, .. } => {
                        // Deliberately export the projection, never transcripts, commands or raw output.
                        let value = json!({"format":"hardknock-profile-v1","trust":"untrusted_when_shared","profile":p});
                        export(output, &value)?;
                        json!({"kind":"export","path":output,"format":"hardknock-profile-v1"})
                    }
                    _ => {
                        store.profile_cache(&p)?;
                        json!({"kind":"profile","profile":p})
                    }
                }
            }
            ProfileCommand::History(a) => {
                let id = stable_id("profile-", &a.subject(cli, store)?)?.parse()?;
                json!({"kind":"history","snapshots":store.profile_history(&id)?})
            }
            ProfileCommand::Compare { from, to, subject } => {
                let resolve = |value: &str| -> Result<ProfileSnapshot> {
                    if let Ok(id) = value.parse::<ProfileSnapshotId>() {
                        return store.profile_snapshot(&id);
                    }
                    let mut cutoff = parse_time(value)?;
                    if value.len() == 10 {
                        cutoff += Duration::days(1) - Duration::nanoseconds(1);
                    }
                    let id = stable_id("profile-", &subject.subject(cli, store)?)?.parse()?;
                    store
                        .profile_history(&id)?
                        .into_iter()
                        .rfind(|s| s.captured_at <= cutoff)
                        .ok_or_else(|| {
                            Error::NotFound(format!("No recorded snapshot on or before {value}"))
                        })
                };
                let r = compare_snapshots(&resolve(from)?, &resolve(to)?, &cfg.development);
                store.save_regressions(&r)?;
                json!({"kind":"growth","growth":r})
            }
        },
        Commands::Growth(a) => {
            let subject = a.subject(cli, store)?;
            let id = stable_id("profile-", &subject)?.parse()?;
            // Completed episodes offer disjoint task windows; cumulative snapshots remain history.
            let episode_ids: Vec<_> = store
                .episodes()?
                .into_iter()
                .filter(|e| e.subject == subject)
                .filter_map(|e| e.profile_after)
                .collect();
            let history = store.profile_history(&id)?;
            let comparable: Vec<_> = history
                .iter()
                .filter(|s| episode_ids.contains(&s.id))
                .collect();
            let s: Vec<_> = if comparable.len() >= 2 {
                comparable
            } else {
                history.iter().collect()
            };
            if s.len() < 2 {
                json!({"kind":"growth","status":"insufficient_evidence","note":"Record at least two snapshots or completed development episodes; missing observations are UNKNOWN"})
            } else {
                let report = compare_snapshots(s[s.len() - 2], s[s.len() - 1], &cfg.development);
                store.save_regressions(&report)?;
                json!({"kind":"growth","growth":report})
            }
        }
        Commands::Lesson {
            command: LessonCommand::History { id },
        } => json!({"kind":"lesson_history","revisions":store.lesson_versions(id)?}),
        Commands::Skill {
            command: SkillCommand::History { name },
        } => {
            json!({"kind":"skill_history","revisions":store.skill_revisions(&store.skill(name)?.id)?})
        }
        Commands::Skill {
            command: SkillCommand::Revise { name, experience },
        } => json!({"kind":"skill_revision","revision":store.revise_skill(name,experience)?}),
        Commands::Skill {
            command: SkillCommand::Package {
                command: Some(c), ..
            },
        } => {
            let (name, profile) = match c {
                PackageCommand::History { name, profile }
                | PackageCommand::Diff { name, profile, .. }
                | PackageCommand::Export { name, profile, .. } => (name, profile),
            };
            let revisions = store.package_revisions(&store.skill(name)?.id, profile)?;
            let get = |n| {
                revisions
                    .iter()
                    .find(|r| r.revision == n)
                    .ok_or_else(|| Error::NotFound(format!("Package revision {n}")))
            };
            match c {
                PackageCommand::History { .. } => {
                    json!({"kind":"package_history","revisions":revisions})
                }
                PackageCommand::Diff { from, to, .. } => {
                    let a = get(*from)?;
                    let b = get(*to)?;
                    json!({"kind":"package_diff","from":a.revision,"to":b.revision,"skill_revision":[a.skill_revision,b.skill_revision],"maturity":[a.package.maturity,b.package.maturity],"coverage":[a.package.coverage,b.package.coverage],"hashes":[a.evidence_hash,b.evidence_hash],"removed":a.items.iter().filter(|i|!b.items.contains(i)).collect::<Vec<_>>(),"added":b.items.iter().filter(|i|!a.items.contains(i)).collect::<Vec<_>>()})
                }
                PackageCommand::Export {
                    output, revision, ..
                } => {
                    let r = if let Some(n) = revision {
                        get(*n)?
                    } else {
                        revisions
                            .last()
                            .ok_or_else(|| Error::NotFound("Generate a package first".into()))?
                    };
                    let value = json!({"format":"hardknock-package-manifest-v1","trust":"untrusted_when_shared","revision":r,"note":"Local evidence reference manifest, not an executable import; referenced raw artifacts are not included"});
                    export(output, &value)?;
                    json!({"kind":"export","path":output})
                }
            }
        }
        Commands::Experience {
            command: ExperienceCommand::Health(a) | ExperienceCommand::Maintain(a),
        } => {
            let state = capture_state(&cli.repo)?;
            let context =
                ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled)?;
            let p = EvidenceProfileBuilder {
                store,
                config: &cfg.development,
                now: Utc::now(),
                context: Some(crate::retrieval::QueryContext::new(&context, "", vec![])),
            }
            .build(&a.subject(cli, store)?, ProfileWindow::AllTime)?;
            json!({"kind":"maintenance","report":maintain(store,&p,&context,matches!(&cli.command,Commands::Experience { command:ExperienceCommand::Maintain(_) }))?})
        }
        Commands::Revalidation { command } => match command {
            RevalidationCommand::List => {
                json!({"kind":"revalidation","items":store.revalidations()?})
            }
            RevalidationCommand::Run { id } => {
                let item = store
                    .revalidations()?
                    .into_iter()
                    .find(|i| i.id == *id)
                    .ok_or_else(|| Error::NotFound(id.to_string()))?;
                super::warning(cli.json)?;
                json!({"kind":"revalidation","item":run_revalidation(store,&item,cancel).await?})
            }
        },
        Commands::Episode { command } => match command {
            EpisodeCommand::List => json!({"kind":"episodes","episodes":store.episodes()?}),
            EpisodeCommand::Start { name, subject } => {
                json!({"kind":"episode","episode":start_episode(store,subject.subject(cli,store)?,name,&cfg.development)?})
            }
            EpisodeCommand::Finish { id } => {
                json!({"kind":"episode","episode":finish_episode(store,id,&cfg.development)?})
            }
        },
        Commands::Timeline(a) => {
            let since = a.since.as_deref().map(parse_time).transpose()?;
            let skill = a.skill.as_deref().map(|n| store.skill(n)).transpose()?;
            let observations: std::collections::HashMap<_, _> = store
                .development_observations()?
                .into_iter()
                .map(|e| (e.id.clone(), e))
                .collect();
            let lesson = a.lesson.as_ref().map(|id| store.lesson(id)).transpose()?;
            let events: Vec<_> = store
                .development_timeline(20000)?
                .into_iter()
                .filter(|e| since.is_none_or(|t| e.at >= t))
                .filter(|e| {
                    let observation = e.experience_id.as_ref().and_then(|id| observations.get(id));
                    a.agent
                        .as_ref()
                        .is_none_or(|agent| observation.is_some_and(|o| &o.agent.kind == agent))
                        && skill.as_ref().is_none_or(|s| {
                            e.id == s.id.to_string()
                                || observation.is_some_and(|o| s.context.matches(&o.context))
                        })
                        && lesson.as_ref().is_none_or(|l| {
                            e.id == l.id.to_string()
                                || observation.is_some_and(|o| {
                                    o.id == l.source_experience
                                        || o.applications.iter().any(|a| a.lesson_id == l.id)
                                })
                        })
                })
                .take(a.limit as usize)
                .collect();
            json!({"kind":"timeline","events":events,"scan_limit":20000})
        }
        Commands::Benchmark {
            command: BenchmarkCommand::List,
        } => json!({"kind":"benchmarks","runs":store.benchmark_runs()?}),
        Commands::Benchmark {
            command: BenchmarkCommand::Longitudinal { output },
        } => {
            super::warning(cli.json)?;
            let result = benchmark::run(store, cancel).await?;
            if let Some(path) = output {
                export(path, &serde_json::to_value(&result)?)?;
            }
            json!({"kind":"benchmark","benchmark":result})
        }
        Commands::Doctor => {
            let p = build(
                store,
                &cfg,
                &ExperienceSubject::SharedLocal,
                ProfileWindow::AllTime,
            )?;
            let database = store.database_health()?;
            json!({"kind":"doctor","snapshots":database["snapshot_count"],"database":database,"schema_version":9,"policy_hash":p.policy_hash,"health":p.freshness,"experience_count":p.experience_count,"queue_pending":store.revalidations()?.iter().filter(|i|i.status=="pending").count(),"latest_benchmark":store.benchmark_runs()?.last().map(|b|json!({"id":b.id,"status":b.status})),"auto_run":false})
        }
        _ => return Err(Error::InvalidInput("Not a development command".into())),
    })
}
pub fn print(result: &Value, out: &mut impl Write) -> Result<()> {
    match result["kind"].as_str() {
        Some("profile") => {
            let p: ExperienceProfile = serde_json::from_value(result["profile"].clone())?;
            writeln!(
                out,
                "Experience Profile · {}\nSubject: {}\nWindow: {}\nExperiences: {} · task attempts: {}\nSkills: {} · Lessons: {} · Reflexes: {} · Recoveries: {}",
                p.id,
                serde_json::to_string(&p.subject)?,
                serde_json::to_string(&p.window)?,
                p.experience_count,
                p.task_count,
                p.skills.len(),
                p.lessons.len(),
                p.reflexes.len(),
                p.recoveries.len()
            )?;
            for k in DevelopmentMetricKind::ALL {
                let m = p.metrics.metric(k);
                writeln!(
                    out,
                    "  {:?}: {} (n={})",
                    k,
                    m.value
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                    m.sample_count
                )?;
            }
            writeln!(
                out,
                "Hardened Skills: {}\nFreshness: {}\nUnknown conditions: {}\nObserved behavior in this scope; not an intelligence score.",
                p.metrics.hardened_skill_count,
                serde_json::to_string(&p.freshness)?,
                p.coverage.known_unknowns.len()
            )?;
        }
        Some("growth") if result.get("growth").is_some() => {
            let r: GrowthReport = serde_json::from_value(result["growth"].clone())?;
            writeln!(out, "Development · {} → {}", r.from, r.to)?;
            for c in r.comparisons {
                writeln!(
                    out,
                    "  {:?}: {:?} · delta {} · n={}/{}\n    {}",
                    c.metric,
                    c.trend,
                    c.delta
                        .map(|d| format!("{d:+.3}"))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                    c.previous.sample_count,
                    c.current.sample_count,
                    c.reason
                )?;
            }
            writeln!(
                out,
                "Median recovery ms: {:?} → {:?} (samples {}/{})\nHardened Skills: {:?} → {:?}",
                r.median_recovery_ms.previous,
                r.median_recovery_ms.current,
                r.median_recovery_ms.previous_samples,
                r.median_recovery_ms.current_samples,
                r.hardened_skills.previous,
                r.hardened_skills.current
            )?;
            writeln!(out, "{}\nNo curriculum was started.", r.note)?;
        }
        Some("history") => {
            writeln!(
                out,
                "DATE / SNAPSHOT                                  SUCCESS  REPEAT  RECOVERY  TRANSFER"
            )?;
            let snapshots: Vec<ProfileSnapshot> =
                serde_json::from_value(result["snapshots"].clone())?;
            let rate = |m: &MetricValue| {
                m.value
                    .map(|v| format!("{:.0}% (n={})", v * 100.0, m.sample_count))
                    .unwrap_or_else(|| "UNKNOWN".into())
            };
            for s in snapshots {
                writeln!(
                    out,
                    "{} {}\n  {}  {}  {}  {}",
                    s.captured_at,
                    s.id,
                    rate(&s.metrics.task_success_rate),
                    rate(&s.metrics.repeated_mistake_rate),
                    rate(&s.metrics.recovery_success_rate),
                    rate(&s.metrics.experience_transfer_rate)
                )?;
            }
        }
        Some("timeline") => {
            let events: Vec<TimelineEvent> = serde_json::from_value(result["events"].clone())?;
            for e in events {
                writeln!(
                    out,
                    "{}  {}  {}{}  {}",
                    e.at,
                    e.kind,
                    e.id,
                    e.revision.map(|v| format!(" v{v}")).unwrap_or_default(),
                    e.description
                )?;
            }
        }
        Some("benchmark") => {
            let b = &result["benchmark"];
            writeln!(
                out,
                "Longitudinal Development Benchmark · {}\n5 episodes · 30 tasks per arm · local deterministic fixtures",
                b["id"].as_str().unwrap_or_default()
            )?;
            for arm in ["stateless", "reflection_memory", "hardknock"] {
                writeln!(out, "\n{arm}")?;
                for metric in [
                    "task_success_rate",
                    "repeated_mistake_rate",
                    "recovery_success_rate",
                    "experience_transfer_rate",
                ] {
                    let m = &b["metrics"][arm]["aggregate"][metric];
                    writeln!(
                        out,
                        "  {metric}: {} (n={})",
                        m["value"]
                            .as_f64()
                            .map(|v| format!("{:.1}%", v * 100.0))
                            .unwrap_or_else(|| "UNKNOWN".into()),
                        m["sample_count"]
                    )?;
                }
                let m = &b["stale_rule"][arm]["task_success_rate"];
                writeln!(
                    out,
                    "  stale-rule success: {}/{}",
                    m["numerator"], m["sample_count"]
                )?;
            }
            writeln!(
                out,
                "\nPortability: {}/{} observed validated applications\nRaw results and learning curves are stored in the benchmark record. Fixture results are not general-agent performance.",
                b["portability"]["metric"]["numerator"], b["portability"]["metric"]["sample_count"]
            )?;
        }
        _ => {
            serde_json::to_writer_pretty(&mut *out, result)?;
            writeln!(out)?;
        }
    }
    Ok(())
}
