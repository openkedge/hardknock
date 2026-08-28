// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands, Response, warning};
use crate::{
    Error, Result,
    bridge::config::Config,
    cancellation::Cancellation,
    core::*,
    curriculum::*,
    resilience::Skill,
    store::{CurriculumStore, Store},
};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Write;
#[derive(Debug, Args)]
pub struct HardenArgs {
    #[arg(
        long,
        help = "Deliberately repeat catalog conditions even when already observed"
    )]
    pub replicate: bool,
    #[arg(long, default_value = "resilience-basic")]
    pub profile: String,
    #[arg(
        long,
        default_value_t = 5,
        help = "Maximum curriculum trials, including paired response tests; controls consume additional Reality/agent slots"
    )]
    pub budget: usize,
}
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("target").required(true).args(["skill","task_family"])))]
pub struct PlanArgs {
    #[arg(long)]
    pub skill: Option<String>,
    #[arg(long)]
    pub task_family: Option<String>,
    #[command(flatten)]
    pub limits: HardenArgs,
}
#[derive(Debug, Subcommand)]
pub enum CurriculumCommand {
    Plan(PlanArgs),
    Run { id: CurriculumId },
    List,
    Show { id: CurriculumId },
    Why { id: CurriculumId },
    Report { id: CurriculumId },
    Cancel { id: CurriculumId },
}
#[derive(Debug, Subcommand)]
pub enum TaskFamilyCommand {
    Register {
        name: String,
        #[arg(long = "experience", required = true)]
        examples: Vec<ExperienceId>,
    },
    List,
    Show {
        name: String,
    },
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CurriculumResponse {
    Curriculum {
        curriculum: Box<Curriculum>,
        recommendation: Box<CurriculumRecommendation>,
        events: Vec<(u64, CurriculumEvent)>,
        report: Value,
        reviews: Vec<Value>,
    },
    List {
        curricula: Vec<Value>,
    },
    Package {
        skill: Box<Skill>,
        package: Box<ExperiencePackage>,
    },
    Families {
        families: Vec<TaskFamily>,
    },
    Cancel {
        curriculum_id: CurriculumId,
        cancellation_requested: bool,
    },
}
pub fn report(c: &Curriculum) -> Value {
    let count = |o| {
        c.trials
            .iter()
            .filter(|t| t.result.as_ref().and_then(|r| r.outcome) == Some(o))
            .count()
    };
    let outcomes: Vec<_> = c
        .trials
        .iter()
        .filter_map(|t| t.learning_outcome.as_ref())
        .collect();
    let lessons: Vec<_> = outcomes.iter().flat_map(|o| &o.lessons_created).collect();
    let reflexes: Vec<_> = outcomes.iter().flat_map(|o| &o.reflexes_created).collect();
    let recoveries: Vec<_> = outcomes
        .iter()
        .flat_map(|o| &o.recoveries_created)
        .collect();
    let experiences: Vec<_> = outcomes.iter().flat_map(|o| &o.new_experiences).collect();
    let artifacts = lessons.len() + reflexes.len() + recoveries.len();
    json!({"curriculum_id":c.id,"status":c.status,"target":c.target,"profile":c.profile,"summary":{"trials":c.trials_executed,"pass":count(crate::resilience::ChaosTrialOutcome::Pass),"degraded":count(crate::resilience::ChaosTrialOutcome::Degraded),"fail":count(crate::resilience::ChaosTrialOutcome::Fail),"inconclusive":c.trials.iter().filter(|t|t.status==GoalStatus::Inconclusive).count()},"budget":c.budget,"usage":c.usage,"reserved":c.reserved,"new_experiences":experiences,"new_lessons":lessons,"new_reflexes":reflexes,"new_recoveries":recoveries,"experience_yield":{"artifacts":artifacts,"trials":c.trials_executed,"ratio":(c.trials_executed>0).then(||artifacts as f64/c.trials_executed as f64)},"coverage":c.before.iter().map(|b|{let a=c.after.iter().find(|a|a.skill==b.skill).unwrap_or(b);json!({"skill":b.skill,"before":b.coverage.profile_coverage,"after":a.coverage.profile_coverage,"maturity_before":b.maturity,"maturity_after":a.maturity,"remaining_unknown":a.coverage.dimensions.iter().flat_map(|d|&d.unknown).collect::<Vec<_>>(),"recovery_gaps":a.evidence.high_failure_recovery_gaps,"reflex_check_gaps":a.evidence.reflex_check_gaps})}).collect::<Vec<_>>(),"stop_reason":c.stop_reason})
}
pub fn recommendation(c: &Curriculum) -> CurriculumRecommendation {
    CurriculumRecommendation {target:c.target.clone(),gaps:c.goals.iter().filter(|g|g.status!=GoalStatus::Completed).map(|g|g.evidence_gap.clone()).collect(),rationale:"Configured profile has concrete evidence gaps; inspect this bounded plan and explicitly start it".into(),auto_run:false}
}
fn response(store: &Store, c: Curriculum) -> Result<CurriculumResponse> {
    Ok(CurriculumResponse::Curriculum {
        recommendation: Box::new(recommendation(&c)),
        events: store.curriculum_events(&c.id, 0)?,
        report: report(&c),
        reviews: store.curriculum_reviews()?,
        curriculum: Box::new(c),
    })
}
pub async fn execute(cli: &Cli, store: &Store, cancel: &Cancellation) -> Result<Response> {
    let config = Config::load(&store.home)?;
    let engine = CurriculumExecutor {
        store,
        config: &config,
    };
    let result=match &cli.command {
        Commands::Curriculum {command}=>match command {
            CurriculumCommand::Plan(args)=>{
                let target=if let Some(name)=&args.skill {CurriculumTarget::Skill(store.skill(name)?.id)} else {CurriculumTarget::TaskFamily(store.task_family(args.task_family.as_deref().unwrap_or_default())?.id)};
                let c=if args.limits.replicate {engine.plan_replication(target,&args.limits.profile,&config.curriculum.budget(args.limits.budget)?)?} else {engine.plan(target,&args.limits.profile,&config.curriculum.budget(args.limits.budget)?)?};
                response(store,c)?
            },
            CurriculumCommand::Run {id}=>{warning(cli.json)?;response(store,engine.run(id,cancel).await?)?},
            CurriculumCommand::Show {id}|CurriculumCommand::Why {id}|CurriculumCommand::Report {id}=>response(store,store.curriculum(id)?)?,
            CurriculumCommand::List=>CurriculumResponse::List {curricula:CurriculumStore::list(store,CurriculumQuery::default())?.iter().map(|c|json!({"id":c.id,"target":c.target,"status":c.status,"trials":c.trials.len(),"created_at":c.created_at})).collect()},
            CurriculumCommand::Cancel {id}=>CurriculumResponse::Cancel {curriculum_id:id.clone(),cancellation_requested:store.cancel_curriculum(id)?},
        },
        Commands::Skill {command:super::resilience::SkillCommand::Harden {name,limits}}=>{
            warning(cli.json)?;
            let target=CurriculumTarget::Skill(store.skill(name)?.id);
            let c=if limits.replicate {engine.plan_replication(target,&limits.profile,&config.curriculum.budget(limits.budget)?)?} else {engine.plan(target,&limits.profile,&config.curriculum.budget(limits.budget)?)?};
            if !cli.quiet && !cli.json {eprintln!("{}Hardening {name} · {} · {} selected trials",if cli.no_emoji {""} else {"🌸 "},c.id,c.trials.len());}
            response(store,engine.run(&c.id,cancel).await?)?
        },
        Commands::Skill {command:super::resilience::SkillCommand::Package {name,profile,..}}=>{
            let name=name.as_deref().ok_or_else(||Error::InvalidInput("Specify a Skill name or package subcommand".into()))?;
            let package=skill_package(store,name,profile,&config.curriculum)?;
            store.save_skill_package(&package)?;
            let mut skill=store.skill(name)?;skill.maturity=package.maturity;skill.coverage=package.coverage.clone();
            CurriculumResponse::Package {skill:Box::new(skill),package:Box::new(package)}
        },
        Commands::TaskFamily {command}=>CurriculumResponse::Families {families:match command {TaskFamilyCommand::Register {name,examples}=>vec![store.register_task_family(name,examples.clone())?],TaskFamilyCommand::List=>store.task_families()?,TaskFamilyCommand::Show {name}=>vec![store.task_family(name)?]}},
        _=>return Err(Error::InvalidInput("Not a curriculum command".into())),
    };
    Ok(Response::Curriculum {
        result: Box::new(result),
    })
}
pub fn print_package(out: &mut impl Write, skill: &Skill, p: &ExperiencePackage) -> Result<()> {
    writeln!(
        out,
        "Experience Package · {} · {:?}\nSkill: {}\nEvidence: {} matching executions, {} failures, {} current base successes\nLessons: {} · Reflexes: {} · Recoveries: {}\nOperating envelopes: {}",
        skill.name,
        p.maturity,
        skill.id,
        p.evidence.usage.execution_count,
        p.evidence.usage.failure_count,
        p.evidence.base_successes,
        p.lessons.len(),
        p.reflexes.len(),
        p.recoveries.len(),
        p.operating_envelopes.len()
    )?;
    writeln!(
        out,
        "Procedure: {}\nScope: {}",
        serde_json::to_string(&skill.procedure)?,
        serde_json::to_string(&skill.context)?
    )?;
    for id in &p.operating_envelopes {
        writeln!(out, "  Envelope: {id}")?;
    }
    writeln!(
        out,
        "Provenance · local records retain their original scope and confidence"
    )?;
    for item in &p.provenance {
        writeln!(
            out,
            "  {} {}{}\n    Evidence: {}",
            item.kind,
            item.id,
            item.version.map(|v| format!(" v{v}")).unwrap_or_default(),
            serde_json::to_string(&item.evidence)?
        )?;
    }
    writeln!(
        out,
        "Profile Coverage · {}: {}/{} ({:.0}%)",
        p.coverage.profile.as_deref().unwrap_or("none"),
        p.coverage.tested_conditions,
        p.coverage.configured_conditions,
        p.coverage.profile_coverage.unwrap_or(0.0) * 100.0
    )?;
    for d in &p.coverage.dimensions {
        for o in &d.tested {
            writeln!(
                out,
                "  {}  {:?}  {}",
                o.condition, o.outcome, o.experience_id
            )?;
        }
        for u in &d.unknown {
            writeln!(out, "  {u}  UNKNOWN")?;
        }
    }
    if p.evidence.freshness.stale {
        writeln!(
            out,
            "Revalidation recommended: {}",
            p.evidence.freshness.reasons.join("; ")
        )?;
    }
    for g in &p.evidence.high_failure_recovery_gaps {
        writeln!(out, "Hardening gap: tested Recovery for {g}")?;
    }
    for g in &p.evidence.reflex_check_gaps {
        writeln!(out, "Hardening gap: negative-control check for {g}")?;
    }
    writeln!(
        out,
        "Hardened applies only to the configured evidence policy and tested conditions."
    )?;
    Ok(())
}
impl CurriculumResponse {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Curriculum { curriculum, .. } => match curriculum.status {
                CurriculumStatus::Cancelled => 5,
                CurriculumStatus::PartiallyCompleted => 3,
                _ => 0,
            },
            _ => 0,
        }
    }
    pub fn print(&self, out: &mut impl Write) -> Result<()> {
        match self {
            Self::Curriculum {
                curriculum: c,
                report,
                ..
            } => {
                writeln!(
                    out,
                    "Curriculum {} · {:?}\nTarget: {:?}\nProfile: {} · rounds {}/{}",
                    c.id, c.status, c.target, c.profile, c.rounds, c.max_rounds
                )?;
                for g in &c.goals {
                    writeln!(
                        out,
                        "  {:?} {:?}: {}\n    Why: {}\n    Policy: {:?} · {}",
                        g.priority,
                        g.status,
                        g.description,
                        g.evidence_gap.rationale,
                        g.decision,
                        g.reason
                    )?;
                }
                writeln!(
                    out,
                    "Trials: {} selected, {} started",
                    c.trials.len(),
                    c.trials_executed
                )?;
                for t in &c.trials {
                    writeln!(
                        out,
                        "  {}  {:?} · {} · cost {} Realities / {} agent runs",
                        t.condition,
                        t.result.as_ref().and_then(|r| r.outcome),
                        t.id,
                        t.estimated_budget.realities,
                        t.estimated_budget.agent_runs
                    )?;
                }
                writeln!(
                    out,
                    "Results: {}\nExperience gained: {} Experiences · {} Lessons · {} Reflexes · {} Recoveries",
                    report["summary"],
                    report["new_experiences"].as_array().map_or(0, Vec::len),
                    report["new_lessons"].as_array().map_or(0, Vec::len),
                    report["new_reflexes"].as_array().map_or(0, Vec::len),
                    report["new_recoveries"].as_array().map_or(0, Vec::len)
                )?;
                for b in &c.before {
                    let a = c.after.iter().find(|a| a.skill == b.skill).unwrap_or(b);
                    writeln!(
                        out,
                        "Profile Coverage {}: {:.0}% -> {:.0}%\nSkill maturity: {:?} -> {:?}",
                        c.profile,
                        b.coverage.profile_coverage.unwrap_or(0.0) * 100.0,
                        a.coverage.profile_coverage.unwrap_or(0.0) * 100.0,
                        b.maturity,
                        a.maturity
                    )?;
                }
                writeln!(
                    out,
                    "Budget: {}/{} trials · {}/{} reserved Realities · {} recorded Realities · {}ms\nUnknown conditions remain unknown. No background curriculum is running.",
                    c.trials_executed,
                    c.budget.max_curriculum_trials.unwrap_or(0),
                    c.reserved.realities,
                    c.budget.max_realities,
                    c.usage.realities,
                    c.usage.duration_ms
                )?;
                if let Some(reason) = &c.stop_reason {
                    writeln!(out, "{reason}")?;
                }
            }
            Self::Package { skill, package } => print_package(out, skill, package)?,
            Self::List { curricula } => {
                for c in curricula {
                    writeln!(
                        out,
                        "{} · {} · {} trials",
                        c["id"], c["status"], c["trials"]
                    )?;
                }
            }
            Self::Families { families } => {
                for f in families {
                    writeln!(out, "{} · {} · {} examples", f.id, f.name, f.examples.len())?;
                }
            }
            Self::Cancel {
                curriculum_id,
                cancellation_requested,
            } => writeln!(
                out,
                "{curriculum_id}: cancellation requested={cancellation_requested}; inspect show for terminal cleanup confirmation"
            )?,
        }
        Ok(())
    }
}
