// SPDX-License-Identifier: Apache-2.0
//! Explicit, local longitudinal benchmark. Execution is implemented through the existing engines.
use super::*;
use crate::{
    Error, Result,
    application::{ExperienceRelation, RunLearningOptions},
    bridge::config::Config,
    cancellation::Cancellation,
    core::*,
    curriculum::*,
    dojo::capture_state,
    evaluation::EvaluationSpec,
    experience::{Experience, Outcome, ReplaySpec},
    lesson::{ActionPattern, HeuristicConfidence, Lesson},
    perturbation::{Perturbation, PerturbationParameters},
    reflection::{DeterministicReflection, ReflectionProvider},
    resilience::{fixture, runtime::RunResilienceOptions, *},
    store::{LessonStore, Store},
    workflow::{RunRequest, run_with_learning, run_with_resilience},
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub const VERSION: &str = "longitudinal-fixtures-v1";
fn git(root: &Path, args: &[&str]) -> Result<()> {
    let result = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Hardknock",
            "-c",
            "user.email=fixture@localhost",
        ])
        .args(args)
        .current_dir(root)
        .output()?;
    if !result.status.success() {
        return Err(Error::Intervention(format!(
            "Cannot prepare benchmark fixture: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(())
}
pub(crate) fn pnpm(store: &Store, transfer: bool) -> Result<StateRef> {
    let root = store.home.join("fixtures").join(if transfer {
        "longitudinal-transfer"
    } else {
        "longitudinal-initial"
    });
    fs::create_dir(&root)?;
    let files: Vec<(&str, &str)> = if transfer {
        vec![
            (
                "agent-script.sh",
                include_str!("../../fixtures/pnpm-workspace-transfer/agent-script.sh"),
            ),
            (
                "test.sh",
                include_str!("../../fixtures/pnpm-workspace-transfer/test.sh"),
            ),
            (
                "package.json",
                include_str!("../../fixtures/pnpm-workspace-transfer/package.json"),
            ),
            (
                "pnpm-lock.yaml",
                include_str!("../../fixtures/pnpm-workspace-transfer/pnpm-lock.yaml"),
            ),
            (
                "pnpm-workspace.yaml",
                include_str!("../../fixtures/pnpm-workspace-transfer/pnpm-workspace.yaml"),
            ),
            (
                "hardknock-fixture.json",
                include_str!("../../fixtures/pnpm-workspace-transfer/hardknock-fixture.json"),
            ),
            (
                "packages/service/package.json",
                include_str!(
                    "../../fixtures/pnpm-workspace-transfer/packages/service/package.json"
                ),
            ),
            (
                "packages/worker/package.json",
                include_str!("../../fixtures/pnpm-workspace-transfer/packages/worker/package.json"),
            ),
        ]
    } else {
        vec![
            (
                "agent-script.sh",
                include_str!("../../fixtures/pnpm-workspace-conflict/agent-script.sh"),
            ),
            (
                "test.sh",
                include_str!("../../fixtures/pnpm-workspace-conflict/test.sh"),
            ),
            (
                "package.json",
                include_str!("../../fixtures/pnpm-workspace-conflict/package.json"),
            ),
            (
                "pnpm-lock.yaml",
                include_str!("../../fixtures/pnpm-workspace-conflict/pnpm-lock.yaml"),
            ),
            (
                "pnpm-workspace.yaml",
                include_str!("../../fixtures/pnpm-workspace-conflict/pnpm-workspace.yaml"),
            ),
            (
                "hardknock-fixture.json",
                include_str!("../../fixtures/pnpm-workspace-conflict/hardknock-fixture.json"),
            ),
            (
                "packages/demo/package.json",
                include_str!("../../fixtures/pnpm-workspace-conflict/packages/demo/package.json"),
            ),
        ]
    };
    for (name, body) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent"))?;
        fs::write(&path, body)?;
        if name.ends_with(".sh") {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
    }
    git(&root, &["init", "-q"])?;
    git(&root, &["add", "."])?;
    git(&root, &["commit", "-qm", "Pinned longitudinal fixture"])?;
    capture_state(&root)
}
pub(crate) fn update_environment(state: &StateRef) -> Result<StateRef> {
    for (name, body) in [
        (
            "agent-script.sh",
            include_str!("../../fixtures/pnpm-workspace-contradiction/agent-script.sh"),
        ),
        (
            "test.sh",
            include_str!("../../fixtures/pnpm-workspace-contradiction/test.sh"),
        ),
        (
            "hardknock-fixture.json",
            include_str!("../../fixtures/pnpm-workspace-contradiction/hardknock-fixture.json"),
        ),
    ] {
        fs::write(state.repo_path.join(name), body)?;
    }
    git(&state.repo_path, &["add", "."])?;
    git(
        &state.repo_path,
        &[
            "commit",
            "-qm",
            "Environment now requires npm-compatible output",
        ],
    )?;
    capture_state(&state.repo_path)
}
pub(crate) fn request(
    state: &StateRef,
    agent: &AgentIdentity,
    credential: bool,
    script: &str,
) -> RunRequest {
    RunRequest {
        state: state.clone(),
        goal: if credential {
            "process-task-successfully"
        } else {
            "resolve workspace dependencies"
        }
        .into(),
        agent: agent.clone(),
        command: CommandSpec::shell(script, EnvironmentMode::Controlled),
        evaluation: EvaluationSpec {
            checks: vec!["/bin/sh ./test.sh".into()],
        },
        timeout_secs: 10,
        keep: false,
        replay: Some(ReplaySpec {
            script: if credential {
                "/bin/sh ./operation.sh"
            } else {
                "./agent-script.sh baseline"
            }
            .into(),
            timeout_secs: 10,
        }),
        perturbations: vec![],
        expected_fingerprint: None,
    }
}
fn config() -> Config {
    let mut cfg = Config::default();
    cfg.curriculum.profiles.insert(
        "longitudinal".into(),
        ProfileConfig {
            conditions: vec![
                "delay:500".into(),
                "env:missing".into(),
                "config:drift".into(),
                "dependency:unavailable".into(),
            ],
        },
    );
    cfg
}
/// Explicit benchmark, with a fresh Hardknock store and isolated baseline stores.
/// No external agents, network operations, random outcomes, or direct evidence inserts.
pub async fn run(store: &Store, cancel: &Cancellation) -> Result<BenchmarkResult> {
    if !store.development_observations()?.is_empty()
        || !store.benchmark_runs()?.is_empty()
        || !store.all_lessons()?.is_empty()
    {
        return Err(Error::InvalidInput("Longitudinal benchmark requires a fresh dedicated --home so prior evidence cannot leak into its initial state".into()));
    }
    let cfg = config();
    if store.home.join("config.toml").exists() {
        return Err(Error::InvalidInput("Benchmark requires a fresh unconfigured --home; existing configuration is never overwritten".into()));
    }
    fs::write(
        store.home.join("config.toml"),
        toml::to_string(&cfg).map_err(|e| Error::InvalidInput(e.to_string()))?,
    )?;
    let id = BenchmarkRunId::new();
    let root = store.home.join("fixtures").join(id.to_string());
    fs::create_dir(&root)?;
    let stateless = Store::open(&root.join("stateless"))?;
    let reflection = Store::open(&root.join("reflection-memory"))?;
    let mut result = BenchmarkResult {
        id,
        created_at: Utc::now(),
        status: "running".into(),
        metadata: json!({"hardknock_version":env!("CARGO_PKG_VERSION"),"fixture_version":VERSION,"agent_versions":["fixture-agent-a-v1","fixture-agent-b-v1"],"config":cfg,"random_seed":null,"starting_state":"empty evidence stores","arms":{"stateless":{"home":stateless.home,"retrieval":false},"reflection_memory":{"home":reflection.home,"retrieval":false,"memory":"deterministic text: prefer pnpm; retry operation after credential failure"},"hardknock":{"home":store.home,"retrieval":true}},"episodes":["initial failures","learning and recovery","related-context transfer","agent/model replacement","environment update and stale-rule challenge"],"tasks_per_arm":30,"measurement":"Task outcomes come from subprocess evaluation; training controls are not scored tasks. Baseline Experience IDs resolve in their arm's home."}),
        tasks: vec![],
        metrics: json!({}),
        stale_rule: json!({}),
        portability: json!({}),
        profiles: vec![],
        snapshots: vec![],
        stop_reason: None,
    };
    store.save_benchmark(&result, true)?;
    let outcome = run_arms(store, &stateless, &reflection, &cfg, cancel, &mut result).await;
    result.status = if outcome.is_ok() {
        "completed"
    } else if cancel.is_cancelled() {
        "cancelled"
    } else {
        "failed"
    }
    .into();
    if let Err(error) = &outcome {
        result.stop_reason = Some(error.to_string());
    }
    store.save_benchmark(&result, false)?;
    let path = store
        .home
        .join("artifacts")
        .join(format!("{}.json", result.id));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    outcome?;
    Ok(result)
}
async fn run_arms(
    main: &Store,
    stateless: &Store,
    reflection: &Store,
    cfg: &Config,
    cancel: &Cancellation,
    result: &mut BenchmarkResult,
) -> Result<()> {
    for (arm, store) in [
        ("stateless", stateless),
        ("reflection_memory", reflection),
        ("hardknock", main),
    ] {
        let full = arm == "hardknock";
        let mut initial = pnpm(store, false)?;
        let transfer = pnpm(store, true)?;
        let credential = fixture::materialize(store, FixtureKind::SkillHardening)?;
        result.metadata["arms"][arm]["initial_fixture_trees"] =
            json!([initial.tree_hash, transfer.tree_hash, credential.tree_hash]);
        let subject = ExperienceSubject::Agent(AgentSubject {
            agent_kind: "test-agent".into(),
            agent_version: None,
            model: None,
            configuration: None,
            profile_scope: ProfileScope::LocalStore,
        });
        let mut failed_actions = HashSet::new();
        let mut failed_tasks = HashSet::new();
        let mut first_failure: Option<Experience> = None;
        let mut memory_prefer_pnpm = false;
        let mut skill = None;
        let mut origin_lesson = None;
        let mut recovery = None;
        let mut reflexes = vec![];
        for episode in 1..=5 {
            if cancel.is_cancelled() {
                return Err(Error::Intervention(
                    "Benchmark cancelled before the next task".into(),
                ));
            }
            if episode == 5 {
                initial = update_environment(&initial)?;
            }
            let agent = AgentIdentity {
                kind: "test-agent".into(),
                executable: "/bin/sh".into(),
                version: Some(
                    if episode >= 4 {
                        "fixture-agent-b-v1"
                    } else {
                        "fixture-agent-a-v1"
                    }
                    .into(),
                ),
                model: Some(
                    if episode >= 4 {
                        "deterministic-b"
                    } else {
                        "deterministic-a"
                    }
                    .into(),
                ),
            };
            let ep = start_episode(
                store,
                subject.clone(),
                &format!("{arm}: episode {episode}"),
                &cfg.development,
            )?;
            for within in 0..6 {
                let is_credential = within >= 3;
                let state = if is_credential {
                    &credential
                } else if episode == 3 || episode == 4 {
                    &transfer
                } else {
                    &initial
                };
                let script = if is_credential {
                    "/bin/sh ./operation.sh"
                } else if arm == "reflection_memory" && memory_prefer_pnpm {
                    "./agent-script.sh alternative"
                } else {
                    "./agent-script.sh run"
                };
                let learning = RunLearningOptions {
                    enabled: full && episode > 1,
                    audit: true,
                    fixture: true,
                    proposed_actions: vec![ActionPattern::shell("npm install")],
                    ..Default::default()
                };
                let req = request(state, &agent, is_credential, script);
                let run = if is_credential {
                    run_with_resilience(
                        store,
                        req,
                        &learning,
                        &RunResilienceOptions {
                            fixture: Some(FixtureKind::SkillHardening),
                            perturbations: vec![Perturbation::new(
                                PerturbationParameters::EnvironmentVariable {
                                    key: "HK_TOKEN_STATE".into(),
                                    value: format!("EXPIRED_EPISODE_{episode}"),
                                },
                            )],
                            recovery: if episode > 1 { recovery.clone() } else { None },
                            reflexes: if episode > 1 {
                                reflexes.clone()
                            } else {
                                vec![]
                            },
                            ..Default::default()
                        },
                        cancel,
                    )
                    .await?
                } else {
                    run_with_learning(store, req, &learning, cancel).await?
                };
                let e = run.experience;
                if e.outcome != Outcome::Success && e.outcome != Outcome::Failure {
                    return Err(Error::Intervention(format!(
                        "Benchmark task {} was inconclusive: {:?}",
                        e.id, e.outcome
                    )));
                }
                if first_failure.is_none() && !is_credential {
                    first_failure = Some(e.clone());
                }
                let success = e.outcome == Outcome::Success;
                let task = if is_credential {
                    "credential"
                } else if episode == 5 {
                    "updated_workspace"
                } else {
                    "workspace"
                };
                let choice = if is_credential {
                    if recovery.is_some() && episode > 1 {
                        "tested_recovery"
                    } else {
                        "unchanged_retry"
                    }
                } else if e
                    .lesson_applications
                    .iter()
                    .any(|a| a.influence == crate::application::LessonInfluence::Applied)
                    || (arm == "reflection_memory" && memory_prefer_pnpm)
                {
                    "pnpm"
                } else {
                    "npm"
                };
                let key = (task.to_owned(), choice.to_owned());
                let repeated_mistake = !success && failed_actions.contains(&key);
                let repeated_failure = !success && failed_tasks.contains(task);
                if !success {
                    failed_actions.insert(key);
                    failed_tasks.insert(task.to_owned());
                }
                let observed = e.resilience.as_ref();
                let attempted = observed.is_some_and(|r| {
                    r.metrics.failed_attempts > 0
                        && (r.metrics.retries > 0
                            || r.recovery_attempt.as_ref().is_some_and(|a| a.attempted))
                });
                let recovered = attempted && success;
                let latency = observed
                    .and_then(|r| r.recovery_attempt.as_ref())
                    .filter(|r| r.succeeded)
                    .map(|r| r.time_to_recovery_ms);
                result.tasks.push(BenchmarkTask {
                    episode,
                    index: (episode - 1) * 6 + within + 1,
                    arm: arm.into(),
                    task_kind: task.into(),
                    experience_id: e.id.clone(),
                    success,
                    repeated_mistake,
                    repeated_failure,
                    recovery_attempted: attempted,
                    recovery_succeeded: recovered,
                    time_to_recovery_ms: latency,
                });
                // Revalidate only after the updated fixture actually contradicts delivered advice.
                if full && episode == 5 && within == 0 {
                    let p = EvidenceProfileBuilder {
                        store,
                        config: &cfg.development,
                        now: Utc::now(),
                        context: Some(crate::retrieval::QueryContext::new(
                            &e.context,
                            &e.goal,
                            vec![],
                        )),
                    }
                    .build(&subject, ProfileWindow::AllTime)?;
                    let maintenance = maintain(store, &p, &e.context, true)?;
                    let id = origin_lesson
                        .as_ref()
                        .ok_or_else(|| Error::NotFound("Benchmark Lesson missing".into()))?;
                    let current = store.lesson(id)?;
                    let item = maintenance
                        .revalidation
                        .into_iter()
                        .find(|i| i.item.id == id.to_string())
                        .unwrap_or_else(|| RevalidationItem {
                            id: RevalidationId::new(),
                            item: ExperienceRef {
                                kind: "lesson".into(),
                                id: id.to_string(),
                                revision: current.version as u64,
                            },
                            reason: RevalidationReason::EnvironmentChanged,
                            explanation: "Updated fixture contradicts prior advice".into(),
                            context: e.context.clone(),
                            created_at: Utc::now(),
                            status: "pending".into(),
                            experiment_id: None,
                        });
                    store.enqueue_revalidation(&item)?;
                    let done = run_revalidation(store, &item, cancel).await?;
                    result.stale_rule["revalidation"] = json!(done);
                    result.stale_rule["lesson_after"] = json!(store.lesson(id)?.status);
                }
                main.save_benchmark(result, false)?;
            }
            if episode == 1 && arm == "reflection_memory" {
                let failure = first_failure.as_ref().expect("workspace task");
                let summaries = DeterministicReflection.reflect(failure)?;
                memory_prefer_pnpm = summaries
                    .iter()
                    .any(|h| h.prefer.shell_script() == Some("./agent-script.sh alternative"));
                let saved = json!({"source_experience":failure.id,"summary":summaries.iter().map(|h|&h.claim).collect::<Vec<_>>(),"preferred_action":if memory_prefer_pnpm {Some("./agent-script.sh alternative")}else{None},"tested":false,"policy":"Persist naive preference across tasks and environment changes; credential failures receive unchanged retries"});
                fs::write(
                    store.home.join("artifacts").join("reflection-memory.json"),
                    serde_json::to_vec_pretty(&saved)?,
                )?;
                result.metadata["arms"][arm]["memory"] = saved;
            }
            if episode == 1 && full {
                let failure = first_failure.as_ref().expect("workspace task");
                let h = DeterministicReflection
                    .reflect(failure)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Fixture did not generate a supported hypothesis".into(),
                        )
                    })?;
                store.insert_hypothesis(&h)?;
                let l = Lesson::candidate(&h, &HeuristicConfidence);
                LessonStore::insert(store, &l)?;
                crate::experiment::ExperimentEngine { store }
                    .execute(&l.id, cancel)
                    .await?;
                origin_lesson = Some(l.id);
                let seed = run_with_resilience(
                    store,
                    request(&credential, &agent, true, "/bin/sh ./operation.sh"),
                    &RunLearningOptions {
                        relations: vec![ExperienceRelation::CounterfactualOf(failure.id.clone())],
                        ..Default::default()
                    },
                    &RunResilienceOptions {
                        fixture: Some(FixtureKind::SkillHardening),
                        ..Default::default()
                    },
                    cancel,
                )
                .await?;
                let s = store.register_skill("longitudinal-process-task", &seed.experience.id)?;
                let engine = CurriculumExecutor { store, config: cfg };
                for budget in [4, 3] {
                    let c = engine.plan(
                        CurriculumTarget::Skill(s.id.clone()),
                        "longitudinal",
                        &cfg.curriculum.budget(budget)?,
                    )?;
                    let c = engine.run(&c.id, cancel).await?;
                    if c.status != CurriculumStatus::Completed {
                        return Err(Error::Intervention(format!(
                            "Benchmark curriculum incomplete: {:?}",
                            c.stop_reason
                        )));
                    }
                }
                let p = skill_package(store, &s.name, "longitudinal", &cfg.curriculum)?;
                recovery = p
                    .recoveries
                    .iter()
                    .map(|id| store.recovery(id))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .find(|r| {
                        r.failure_signature.signature == "stale_credential"
                            && matches!(
                                r.status,
                                RecoveryStatus::Supported | RecoveryStatus::Validated
                            )
                    });
                if recovery.is_none() {
                    return Err(Error::NotFound(
                        "Curriculum did not produce a tested credential recovery".into(),
                    ));
                }
                reflexes = p
                    .reflexes
                    .iter()
                    .map(|id| store.reflex(id))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|r| matches!(r.status, ReflexStatus::Supported | ReflexStatus::Active))
                    .take(cfg.development.max_reflexes)
                    .collect();
                skill = Some(s.id);
            }
            let closed = finish_episode(store, &ep.id, &cfg.development)?;
            let episode_profile = EvidenceProfileBuilder {
                store,
                config: &cfg.development,
                now: Utc::now(),
                context: None,
            }
            .build(&subject, ProfileWindow::Since(ep.started_at))?;
            result.metadata["episode_metrics"][arm][episode.to_string()] =
                extra_metrics(&episode_profile);
            if full {
                if let Some(id) = closed.profile_after {
                    result.snapshots.push(id);
                }
                if episode == 4 {
                    let p = EvidenceProfileBuilder {
                        store,
                        config: &cfg.development,
                        now: Utc::now(),
                        context: None,
                    }
                    .build(&subject, ProfileWindow::Since(ep.started_at))?;
                    let id = origin_lesson.as_ref().expect("learned Lesson");
                    let lesson = store.lesson(id)?;
                    result.portability = json!({"metric":p.metrics.experience_portability_rate,"lesson_id":id,"origin_agent":store.experience(&lesson.source_experience)?.agent,"contributors":store.lesson_agent_provenance(id)?,"new_agent":agent,"new_experiences":closed.experiences,"independent_replication":false});
                }
            }
        }
        let p = EvidenceProfileBuilder {
            store,
            config: &cfg.development,
            now: Utc::now(),
            context: None,
        }
        .build(&subject, ProfileWindow::AllTime)?;
        store.profile_cache(&p)?;
        result.metadata["arm_metrics"][arm] = extra_metrics(&p);
        if full {
            result.profiles.push(p.id.clone());
            result.metadata["hardknock_final_metrics"] = json!(p.metrics);
            result.metadata["skill_id"] = json!(skill);
            result.metadata["efficiency"] = json!(p.efficiency);
        }
    }
    let mut metrics = serde_json::Map::new();
    for arm in ["stateless", "reflection_memory", "hardknock"] {
        let tasks: Vec<_> = result.tasks.iter().filter(|t| t.arm == arm).collect();
        let augment = |mut measured: Value, extra: &Value| {
            if let Some(extra) = extra.as_object() {
                measured
                    .as_object_mut()
                    .expect("metrics")
                    .extend(extra.clone());
            }
            measured
        };
        let aggregate = augment(summarize(&tasks), &result.metadata["arm_metrics"][arm]);
        for episode in 0..=5 {
            let subset: Vec<_> = tasks
                .iter()
                .copied()
                .filter(|t| episode == 0 || t.episode == episode)
                .collect();
            let extra = if episode == 0 {
                &result.metadata["arm_metrics"][arm]
            } else {
                &result.metadata["episode_metrics"][arm][episode.to_string()]
            };
            let summary = augment(summarize(&subset), extra);
            for (name, m) in summary.as_object().expect("summary object") {
                if let Some(n) = m["sample_count"].as_u64() {
                    main.save_benchmark_metric(
                        &result.id,
                        arm,
                        episode,
                        name,
                        n,
                        m["value"].as_f64(),
                    )?;
                }
            }
        }
        let episodes:Vec<_>=(1..=5).map(|e|json!({"episode":e,"metrics":augment(summarize(&tasks.iter().copied().filter(|t|t.episode==e).collect::<Vec<_>>()),&result.metadata["episode_metrics"][arm][e.to_string()])})).collect();
        metrics.insert(arm.into(),json!({"aggregate":aggregate,"episodes":episodes,"learning_curve":tasks.iter().map(|t|json!({"task":t.index,"success":t.success,"experience_id":t.experience_id})).collect::<Vec<_>>()}));
        let stale: Vec<_> = tasks
            .iter()
            .copied()
            .filter(|t| t.task_kind == "updated_workspace")
            .collect();
        result.stale_rule[arm] = summarize(&stale);
    }
    result.metrics = Value::Object(metrics);
    let value = |arm: &str, metric: &str| {
        result.metrics[arm]["aggregate"][metric]["value"]
            .as_f64()
            .unwrap_or(-1.0)
    };
    if value("hardknock", "task_success_rate") <= value("stateless", "task_success_rate")
        || value("hardknock", "repeated_mistake_rate")
            >= value("stateless", "repeated_mistake_rate")
        || value("hardknock", "recovery_success_rate")
            <= value("stateless", "recovery_success_rate")
        || result.stale_rule["hardknock"]["task_success_rate"]["value"].as_f64()
            <= result.stale_rule["reflection_memory"]["task_success_rate"]["value"].as_f64()
        || result.portability["metric"]["value"]
            .as_f64()
            .unwrap_or(0.0)
            <= 0.0
    {
        return Err(Error::Intervention(
            "Measured longitudinal acceptance criteria failed; inspect persisted results".into(),
        ));
    }
    Ok(())
}
fn extra_metrics(p: &ExperienceProfile) -> Value {
    let validated: Vec<_> = p
        .efficiency
        .iter()
        .filter_map(|e| e.experiences_to_validation)
        .collect();
    json!({"experience_transfer_rate":p.metrics.experience_transfer_rate,"experience_portability_rate":p.metrics.experience_portability_rate,"reflex_false_positive_rate":p.metrics.reflex_false_positive_rate,"lesson_precision":p.metrics.lesson_precision,"experiment_success_rate":p.metrics.experiment_success_rate,"curriculum_yield":p.metrics.curriculum_yield,"lesson_contradiction_rate":MetricValue::ratio(p.lessons.iter().filter(|a|a.state==EvidenceState::Contradicted).count() as u64,p.lessons.len() as u64,&p.window,"Contradicted current Lessons / known Lessons at episode end; inventory, not a task rate"),"skill_hardening_rate":MetricValue::ratio(p.metrics.hardened_skill_count,p.skills.len() as u64,&p.window,"Currently Hardened Skills / registered Skills at episode end; scoped catalog,"),"experiences_to_validation":MetricValue::ratio(validated.iter().sum(),validated.len() as u64,&p.window,"Mean unique linked Experiences by first validated Lesson revision; UNKNOWN until an artifact validates")})
}
fn summarize(tasks: &[&BenchmarkTask]) -> Value {
    let ratio = |n, d, definition| MetricValue::ratio(n, d, &ProfileWindow::AllTime, definition);
    let successes = tasks.iter().filter(|t| t.success).count() as u64;
    let recovery: Vec<_> = tasks.iter().filter(|t| t.recovery_attempted).collect();
    let mut latencies: Vec<_> = tasks.iter().filter_map(|t| t.time_to_recovery_ms).collect();
    latencies.sort();
    json!({"task_success_rate":ratio(successes,tasks.len() as u64,"Successful evaluated scheduled tasks / scheduled tasks; training executions excluded"),"repeated_mistake_rate":ratio(tasks.iter().filter(|t|t.repeated_mistake).count() as u64,tasks.len() as u64,"Failed reuse of a previously failed observed strategy in this task family/version / scheduled tasks; harness audit shared across arms, never delivered to stateless"),"repeated_failure_rate":ratio(tasks.iter().filter(|t|t.repeated_failure).count() as u64,tasks.len() as u64,"Unresolved task after a prior failure in its task family/version / scheduled tasks"),"recovery_success_rate":ratio(recovery.iter().filter(|t|t.recovery_succeeded).count() as u64,recovery.len() as u64,"Successful recovery / credential faults with observed retries or typed recovery after failure; unchanged retries count as attempted recovery"),"median_time_to_recovery_ms":{"value":median(&latencies),"sample_count":latencies.len(),"definition":"Successful typed recovery latency only; unsuccessful recovery time is not imputed"}})
}
pub fn median(values: &[u64]) -> Option<u64> {
    let n = values.len();
    if n == 0 {
        None
    } else if n.is_multiple_of(2) {
        let (a, b) = (values[n / 2 - 1], values[n / 2]);
        Some(a + (b - a) / 2)
    } else {
        Some(values[n / 2])
    }
}
