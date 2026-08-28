// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::config::Config,
    budget::ExperienceBudget,
    cancellation::Cancellation,
    core::{AgentIdentity, CandidateId, ExperimentRequestId},
    dojo::capture_state,
    experimentation::*,
    store::{LessonQuery, LessonStore, Store},
};
use support::Fixture;

fn identity(name: &str) -> AgentIdentity {
    AgentIdentity {
        kind: name.into(),
        executable: name.into(),
        version: None,
        model: None,
    }
}
fn request(f: &Fixture) -> ExperimentRequest {
    ExperimentRequest {
        id: ExperimentRequestId::new(),
        session_id: "test-session".into(),
        question: "Which upgrade is compatible?".into(),
        hypothesis: None,
        candidates: ["direct", "staged"]
            .into_iter()
            .map(|name| ExperimentCandidate {
                id: CandidateId::new(),
                name: name.into(),
                description: String::new(),
                execution: CandidateExecution::AgentTask {
                    prompt: format!("{name}-upgrade"),
                    agent: None,
                },
                expected_outcome: None,
            })
            .collect(),
        starting_state: ExperimentStartingState {
            state_ref: capture_state(&f.repo).unwrap(),
            expected_fingerprint: None,
            parent_reality: None,
            source: SnapshotSource::RepositoryCommit,
        },
        evaluator: hardknock::evaluation::EvaluationSpec {
            checks: vec!["./test.sh".into()],
        },
        budget: ExperienceBudget::default(),
        requested_by: identity("test-agent"),
        created_at: chrono::Utc::now(),
        criteria: ComparisonCriteria::default(),
        origin: ExperimentOrigin::User,
        intent: ExperimentIntent::CompareStrategies,
        capabilities: ExperimentCapabilities::default(),
    }
}

#[tokio::test]
async fn controlled_fixture_records_equivalent_experiences_and_candidate_lesson() {
    let f = Fixture::from_fixture("strategy-choice");
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let experiment = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(request(&f), &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(
        experiment.status,
        ExperimentStatus::Completed,
        "{:?}",
        experiment.failure
    );
    let result = experiment.result.unwrap();
    assert_eq!(result.quality, ExperimentQuality::Controlled);
    assert_eq!(
        result.recommendation,
        Some(result.candidates[1].candidate_id.clone())
    );
    assert_eq!(
        result.candidates[0].starting_fingerprint,
        result.candidates[1].starting_fingerprint
    );
    assert!(!result.candidates[0].evaluation.success);
    assert!(result.candidates[1].evaluation.success);
    assert_eq!(result.created_experience.len(), 2);
    assert_eq!(result.candidate_lessons.len(), 1);
    assert_eq!(result.usage.realities, 2);
    for candidate in &result.candidates {
        let experience = store.experience(&candidate.experience_id).unwrap();
        assert_eq!(
            experience.experiment.unwrap().experiment_id,
            result.experiment_id
        );
        assert_eq!(
            store.reality(&candidate.reality_id).unwrap().status,
            hardknock::core::RealityStatus::Discarded
        );
    }
    let lessons = LessonStore::list(&store, LessonQuery::default()).unwrap();
    assert!(
        lessons
            .iter()
            .all(|l| l.status == hardknock::lesson::LessonStatus::Candidate)
    );
    f.assert_source_unchanged();
}

fn shell_request(f: &Fixture, scripts: &[&str]) -> ExperimentRequest {
    let mut r = request(f);
    r.candidates = scripts
        .iter()
        .enumerate()
        .map(|(i, s)| ExperimentCandidate {
            id: CandidateId::new(),
            name: format!("candidate-{i}"),
            description: String::new(),
            execution: CandidateExecution::Shell {
                commands: vec![(*s).into()],
            },
            expected_outcome: None,
        })
        .collect();
    r.evaluator.checks = vec!["test -f tracked.txt".into()];
    r
}

#[tokio::test]
async fn different_agents_and_strategies_are_confounded_without_causal_lesson() {
    let f = Fixture::from_fixture("confounded-comparison");
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let mut r = request(&f);
    for (c, suffix) in r.candidates.iter_mut().zip(["A", "B"]) {
        c.execution = CandidateExecution::AgentTask {
            prompt: format!("strategy-{suffix}"),
            agent: Some(identity(&format!("fake-agent-{suffix}"))),
        };
    }
    let e = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(r, &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(e.status, ExperimentStatus::Completed, "{:?}", e.failure);
    let result = e.result.unwrap();
    assert_eq!(result.quality, ExperimentQuality::Confounded);
    assert_eq!(
        result.recommendation,
        Some(result.candidates[1].candidate_id.clone())
    );
    assert!(result.changed_variables.iter().any(|v| v.name == "agent"));
    assert!(
        result
            .changed_variables
            .iter()
            .any(|v| v.name == "strategy")
    );
    assert!(result.candidate_lessons.is_empty());
    assert!(
        LessonStore::list(&store, LessonQuery::default())
            .unwrap()
            .is_empty()
    );
    f.assert_source_unchanged();
}

#[tokio::test]
async fn budgets_and_provider_capacity_reject_before_creating_realities() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut config = Config::default();
    let mut r = shell_request(&f, &["true"; 5]);
    r.budget.max_realities = 2;
    let e = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(r, &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(e.status, ExperimentStatus::Rejected);
    assert!(e.failure.unwrap().contains("Reality budget"));
    assert!(store.realities().unwrap().is_empty());
    let mut r = request(&f);
    r.budget.max_agent_runs = 1;
    assert_eq!(
        ExperimentOrchestrator {
            store: &store,
            config: &config
        }
        .submit(r)
        .unwrap()
        .status,
        ExperimentStatus::Rejected
    );
    config.experiments.provider_capacity = 1;
    let e = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(shell_request(&f, &["true"; 2]), &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(e.status, ExperimentStatus::Rejected);
    assert!(e.failure.unwrap().contains("capacity"));
    assert!(store.realities().unwrap().is_empty());
}

#[tokio::test]
async fn command_budget_counts_evaluators_and_rejects_unobservable_agent_tool_caps() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    let mut r = shell_request(&f, &["true"; 2]);
    r.budget.max_commands_per_reality = Some(1);
    assert_eq!(
        orchestrator.submit(r).unwrap().status,
        ExperimentStatus::Rejected
    );
    let mut r = request(&f);
    r.budget.max_commands_per_reality = Some(50);
    assert_eq!(
        orchestrator.submit(r).unwrap().status,
        ExperimentStatus::Rejected
    );
    assert!(store.realities().unwrap().is_empty());
}

#[tokio::test]
async fn equivalent_state_drift_is_refused_without_any_execution() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    let mut r = shell_request(&f, &["true"; 2]);
    r.starting_state.expected_fingerprint =
        Some(orchestrator.starting_proof(&r).unwrap().fingerprint);
    std::fs::write(f.repo.join("tracked.txt"), "drifted input\n").unwrap();
    support::git(&f.repo, &["add", "."]);
    support::git(&f.repo, &["commit", "-m", "input drift"]);
    r.starting_state.state_ref = capture_state(&f.repo).unwrap();
    let e = orchestrator.run(r, &Cancellation::default()).await.unwrap();
    assert_eq!(e.status, ExperimentStatus::Rejected);
    assert!(e.failure.unwrap().contains("fingerprint drift"));
    assert!(store.executions().unwrap().is_empty());
    assert!(store.realities().unwrap().is_empty());
    assert_eq!(e.result.unwrap().quality, ExperimentQuality::Invalid);
}

#[tokio::test]
async fn ties_are_not_broken_without_explicit_secondary_criteria() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    let mut r = shell_request(&f, &["printf 'extra\n' > extra.txt", "true"]);
    let tied = orchestrator
        .run(r.clone(), &Cancellation::default())
        .await
        .unwrap()
        .result
        .unwrap();
    assert!(tied.recommendation.is_none());
    assert!(tied.candidates.iter().all(|c| c.evaluation.success));
    r.id = ExperimentRequestId::new();
    for c in &mut r.candidates {
        c.id = CandidateId::new();
    }
    r.criteria.minimize_diff_size = true;
    let result = orchestrator
        .run(r, &Cancellation::default())
        .await
        .unwrap()
        .result
        .unwrap();
    assert_eq!(
        result.recommendation,
        Some(result.candidates[1].candidate_id.clone())
    );
    f.assert_source_unchanged();
}

#[tokio::test]
async fn bounded_candidates_run_concurrently_and_sequential_limit_is_respected() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut config = Config::default();
    for parallel in [2, 1] {
        config.experiments.max_parallel_realities = parallel;
        let r = shell_request(&f, &["sleep 0.3", "sleep 0.3"]);
        let result = ExperimentOrchestrator {
            store: &store,
            config: &config,
        }
        .run(r, &Cancellation::default())
        .await
        .unwrap()
        .result
        .unwrap();
        let a = store
            .experience(&result.candidates[0].experience_id)
            .unwrap()
            .actions[0]
            .clone();
        let b = store
            .experience(&result.candidates[1].experience_id)
            .unwrap()
            .actions[0]
            .clone();
        let overlap = a.started_at
            < b.started_at + chrono::Duration::milliseconds(b.duration_ms as i64)
            && b.started_at < a.started_at + chrono::Duration::milliseconds(a.duration_ms as i64);
        assert_eq!(overlap, parallel == 2);
    }
    f.assert_source_unchanged();
}

#[tokio::test]
async fn cancellation_kills_child_discards_realities_and_retains_interrupted_experience() {
    use std::time::Duration;
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut config = Config::default();
    config.experiments.max_parallel_realities = 1;
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    let r = shell_request(
        &f,
        &[
            "sleep 60 & child=$!; printf '%s' \"$child\" > child.pid; wait",
            "true",
        ],
    );
    let accepted = orchestrator.submit(r).unwrap();
    let cancel = Cancellation::default();
    let monitor = async {
        for _ in 0..200 {
            for reality in store.realities().unwrap() {
                if let Ok(pid) = std::fs::read_to_string(reality.root.join("child.pid"))
                    && let Ok(pid) = pid.parse::<i32>()
                {
                    store.cancel_experiment(&accepted.id).unwrap();
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("candidate child did not start");
    };
    let (e, pid) = tokio::join!(orchestrator.execute(&accepted.id, &cancel), monitor);
    let e = e.unwrap();
    assert_eq!(e.status, ExperimentStatus::Cancelled, "{:?}", e.failure);
    let result = e.result.unwrap();
    assert!(result.recommendation.is_none());
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(
        store
            .experience(&result.candidates[0].experience_id)
            .unwrap()
            .outcome,
        hardknock::experience::Outcome::Interrupted
    );
    let process = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&process.stdout);
    assert!(
        status.trim().is_empty() || status.trim().starts_with('Z'),
        "child still active: {status}"
    );
    f.assert_source_unchanged();
}

#[tokio::test]
async fn duration_budget_cancels_remaining_work_and_cleanup_completes() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let mut r = shell_request(&f, &["sleep 60"; 2]);
    r.budget.max_duration_ms = Some(1500);
    let start = std::time::Instant::now();
    let e = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(r, &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(e.status, ExperimentStatus::Cancelled);
    assert!(e.failure.unwrap().contains("duration budget"));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    f.assert_source_unchanged();
}

#[test]
fn external_effects_policy_and_request_id_conflicts_are_rejected() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut config = Config::default();
    config
        .bridge
        .policy
        .approval_shell_commands
        .push("needs-approval".into());
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    for script in [
        "git push origin main",
        "send email to someone",
        "aws s3 rm bucket",
        "needs-approval",
    ] {
        assert_eq!(
            orchestrator
                .submit(shell_request(&f, &[script, "true"]))
                .unwrap()
                .status,
            ExperimentStatus::Rejected
        );
    }
    let mut r = shell_request(&f, &["true"; 2]);
    r.capabilities.allow_external_mutations = true;
    assert_eq!(
        orchestrator.submit(r).unwrap().status,
        ExperimentStatus::Rejected
    );
    let mut r = shell_request(&f, &["true"; 2]);
    let first = orchestrator.submit(r.clone()).unwrap();
    assert_eq!(orchestrator.submit(r.clone()).unwrap().id, first.id);
    r.question = "changed question".into();
    assert!(orchestrator.submit(r).is_err());
    assert!(store.realities().unwrap().is_empty());
}

#[test]
fn a_drifted_candidate_worktree_fails_the_all_candidates_barrier() {
    use hardknock::dojo::{GitRealityProvider, RealityProvider};
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let orchestrator = ExperimentOrchestrator {
        store: &store,
        config: &config,
    };
    let request = shell_request(&f, &["true"; 2]);
    let proof = orchestrator.starting_proof(&request).unwrap();
    let provider = GitRealityProvider::new(&store);
    let mut realities = vec![
        provider.create(&proof.state_ref).unwrap(),
        provider.create(&proof.state_ref).unwrap(),
    ];
    std::fs::write(realities[1].root.join("tracked.txt"), "drift after fork").unwrap();
    assert!(
        orchestrator
            .verify_equivalent_realities(&request, &proof, &realities)
            .unwrap_err()
            .to_string()
            .contains("equivalent starting state")
    );
    assert!(store.executions().unwrap().is_empty());
    for reality in &mut realities {
        provider.discard(reality).unwrap();
    }
    f.assert_source_unchanged();
}

#[tokio::test]
async fn configured_agent_tasks_are_literal_arguments_and_not_claimed_fully_controlled() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let mut config = Config::default();
    config.experiments.agents.insert(
        "echo-agent".into(),
        ExperimentAgentConfig {
            command: "/usr/bin/printf %s {task}".into(),
            environment: hardknock::core::EnvironmentMode::Controlled,
            version: None,
            model: None,
        },
    );
    let mut r = shell_request(&f, &["true"; 2]);
    for (c, prompt) in r
        .candidates
        .iter_mut()
        .zip(["$(touch escaped)", "literal ; touch escaped"])
    {
        c.execution = CandidateExecution::AgentTask {
            prompt: prompt.into(),
            agent: Some(identity("echo-agent")),
        };
    }
    r.evaluator.checks = vec!["test ! -e escaped".into()];
    let e = ExperimentOrchestrator {
        store: &store,
        config: &config,
    }
    .run(r, &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(e.status, ExperimentStatus::Completed, "{:?}", e.failure);
    let result = e.result.unwrap();
    assert_eq!(result.quality, ExperimentQuality::PartiallyControlled);
    assert!(result.candidates.iter().all(|c| c.evaluation.success));
    assert!(result.recommendation.is_none());
    f.assert_source_unchanged();
}

#[test]
fn cli_try_show_why_replay_fork_tree_export_and_immutable_storage() {
    let f = Fixture::from_fixture("strategy-choice");
    let value = f.cli(
        &[
            "try",
            "--agent",
            "test-agent",
            "--candidate",
            "direct=direct-upgrade",
            "--candidate",
            "staged=staged-upgrade",
            "--check",
            "./test.sh",
        ],
        0,
    );
    let experiment = &value["result"]["experiment"];
    let id = experiment["id"].as_str().unwrap();
    assert_eq!(experiment["result"]["quality"], "controlled");
    assert_eq!(
        f.cli(&["experiment", "show", id], 0)["result"]["experiment"]["id"],
        id
    );
    assert_eq!(
        f.cli(&["why", "--experiment", id], 0)["result"]["experiment"]["id"],
        id
    );
    assert_eq!(
        f.cli(&["experiment", "list", "--agent", "test-agent"], 0)["strategy_experiments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let replay = f.cli(&["experiment", "replay", id, "--candidate", "staged"], 0);
    assert_ne!(replay["result"]["experiment"]["id"], id);
    assert!(replay["result"]["experiment"]["result"]["recommendation"].is_null());
    assert_eq!(replay["result"]["relations"][0]["relation"], "replay");
    let fork = f.cli(
        &[
            "experiment",
            "fork",
            id,
            "--candidate",
            "third=staged-upgrade",
        ],
        0,
    );
    assert_eq!(
        fork["result"]["experiment"]["result"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(fork["result"]["relations"][0]["relation"], "extension");
    let reality = experiment["result"]["candidates"][1]["reality_id"]
        .as_str()
        .unwrap();
    let patch = f.temp.path().join("staged.patch");
    f.cli(
        &[
            "reality",
            "export",
            reality,
            "--patch",
            patch.to_str().unwrap(),
        ],
        0,
    );
    assert!(
        std::fs::read_to_string(&patch)
            .unwrap()
            .contains("consumer-version")
    );
    support::git(&f.repo, &["apply", "--check", patch.to_str().unwrap()]);
    let before = std::fs::read(&patch).unwrap();
    assert!(
        !f.command()
            .args([
                "reality",
                "export",
                reality,
                "--patch",
                patch.to_str().unwrap()
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read(&patch).unwrap(), before);
    assert!(
        !f.cli(&["reality", "tree"], 0)["result"]["realities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute(
            "UPDATE experiment_requests SET status='running' WHERE id=?1",
            [id]
        )
        .is_err()
    );
    assert!(
        db.execute(
            "UPDATE experiment_candidates SET result='{}' WHERE experiment_id=?1",
            [id]
        )
        .is_err()
    );
    assert!(
        db.execute(
            "DELETE FROM experiment_candidates WHERE experiment_id=?1",
            [id]
        )
        .is_err()
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        f.cli(&["experiment", "show", id], 0)["result"]["experiment"],
        *experiment
    );
    f.assert_source_unchanged();
}

#[test]
fn diff_counts_content_lines_that_resemble_file_headers() {
    let summary = summarize_diff(
        b"diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n---old\n+++new\n",
    );
    assert_eq!(
        (summary.files_changed, summary.insertions, summary.deletions),
        (1, 1, 1)
    );
}
