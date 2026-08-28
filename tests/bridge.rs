// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::{
        Bridge,
        config::Config,
        protocol::*,
        transport::{self, BridgeClient},
    },
    cancellation::Cancellation,
    experience::Outcome,
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use support::Fixture;
struct Runtime {
    bridge: Option<Arc<Bridge>>,
    worker: Option<JoinHandle<()>>,
}
impl Runtime {
    fn new(home: &Path) -> Self {
        let (bridge, worker) = Bridge::open(home).unwrap();
        Self {
            bridge: Some(bridge),
            worker: Some(worker),
        }
    }
    fn b(&self) -> &Bridge {
        self.bridge.as_ref().unwrap()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(b) = self.bridge.take() {
            let _ = b.flush();
            drop(b);
        }
        if let Some(w) = self.worker.take() {
            w.join().unwrap();
        }
    }
}
fn start(b: &Bridge, f: &Fixture, agent: &str, external: &str) -> String {
    b.handle(AgentEvent::SessionStarted(SessionStarted {
        session_id: external.into(),
        agent: AgentIdentity::new(agent),
        cwd: f.repo.to_string_lossy().into(),
        repository: None,
        task: Some("Evaluate tracked state".into()),
        environment: Default::default(),
    }))
    .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .into()
}
fn action(f: &Fixture) -> NormalizedAction {
    NormalizedAction::Shell {
        command: "printf hello".into(),
        cwd: f.repo.to_string_lossy().into(),
    }
}
fn propose(id: &str, action_id: &str, action: NormalizedAction) -> AgentEvent {
    AgentEvent::ActionProposed(ActionProposed {
        hardknock_session_id: id.into(),
        action_id: action_id.into(),
        action,
        context: Default::default(),
    })
}
fn complete(id: &str, action_id: &str, action: NormalizedAction) -> AgentEvent {
    AgentEvent::ActionCompleted(ActionCompleted {
        hardknock_session_id: id.into(),
        action_id: action_id.into(),
        action,
        result: ActionResult {
            success: true,
            exit_code: Some(0),
            error_class: None,
            output_summary: Some(
                "OPENAI_API_KEY=secret-value Authorization: Bearer secret-credential AWS_SECRET_ACCESS_KEY='quoted secret value'".into(),
            ),
            artifacts: vec![],
        },
        duration_ms: 2,
    })
}
fn finish(b: &Bridge, id: &str) -> Value {
    b.handle(AgentEvent::RunCompleted(RunCompleted {
        termination: RunTermination::Completed,
        hardknock_session_id: id.into(),
        run_id: "turn-1".into(),
        success: Some(true),
        final_message: Some("FULL PRIVATE OUTPUT MUST NOT BE SAVED".into()),
        duration_ms: 3,
        external_metadata: json!({"OPENAI_API_KEY":"should-never-persist"}),
    }))
    .unwrap()
}
fn config(f: &Fixture, checks: Vec<String>) {
    fs::create_dir_all(&f.home).unwrap();
    let mut config = Config::default();
    config
        .bridge
        .evaluators
        .insert(f.repo.canonicalize().unwrap().display().to_string(), checks);
    fs::write(
        f.home.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
}
#[test]
fn lifecycle_persists_evaluated_experience_idempotently_and_redacts() {
    let f = Fixture::new();
    config(&f, vec!["test -f tracked.txt".into()]);
    let r = Runtime::new(&f.home);
    let id = start(r.b(), &f, "test-adapter", "one");
    let p = propose(&id, "a", action(&f));
    assert_eq!(r.b().handle(p.clone()).unwrap()["decision"], "continue");
    r.b().handle(p).unwrap();
    r.b().handle(complete(&id, "a", action(&f))).unwrap();
    r.b().handle(complete(&id, "a", action(&f))).unwrap();
    let recorded = finish(r.b(), &id);
    assert_eq!(
        recorded["experience_id"],
        finish(r.b(), &id)["experience_id"]
    );
    r.b().flush().unwrap();
    let run = r
        .b()
        .handle(AgentEvent::RunStatus {
            hardknock_session_id: id.clone(),
            run_id: "turn-1".into(),
        })
        .unwrap();
    assert_eq!(run["status"], "completed", "{run}");
    let store = Store::open(&f.home).unwrap();
    let exp = store
        .experience(&recorded["experience_id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert_eq!(exp.outcome, Outcome::Success);
    assert_eq!(
        store.realities().unwrap()[0].status,
        hardknock::core::RealityStatus::Observed
    );
    let bytes = fs::read(f.home.join("hardknock.db-wal")).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    for secret in [
        "secret-value",
        "secret-credential",
        "quoted secret value",
        "FULL PRIVATE OUTPUT",
        "should-never-persist",
    ] {
        assert!(!text.contains(secret));
    }
    let trace = fs::read_to_string(&exp.observed_actions[0].artifact.path).unwrap();
    assert!(!trace.contains("secret-value"));
    assert!(trace.contains("[REDACTED]"));
    f.assert_source_unchanged();
}
#[test]
fn success_claim_without_evaluator_remains_unknown_and_failure_check_wins() {
    for (checks, expected) in [
        (vec![], Outcome::Inconclusive),
        (vec!["exit 1".into()], Outcome::Failure),
    ] {
        let f = Fixture::new();
        config(&f, checks);
        let r = Runtime::new(&f.home);
        let id = start(r.b(), &f, "test-adapter", "one");
        let run = finish(r.b(), &id);
        r.b().flush().unwrap();
        let exp = Store::open(&f.home)
            .unwrap()
            .experience(&run["experience_id"].as_str().unwrap().parse().unwrap())
            .unwrap();
        assert_eq!(exp.outcome, expected);
    }
}
#[test]
fn invalid_lifecycle_cannot_replace_existing_evidence() {
    let f = Fixture::new();
    let r = Runtime::new(&f.home);
    let id = start(r.b(), &f, "claude", "same");
    let codex = start(r.b(), &f, "codex", "same");
    assert_ne!(id, codex);
    assert!(r.b().handle(complete(&id, "missing", action(&f))).is_err());
    r.b().handle(propose(&id, "a", action(&f))).unwrap();
    let mut other = action(&f);
    if let NormalizedAction::Shell { command, .. } = &mut other {
        *command = "different".into();
    }
    assert!(r.b().handle(propose(&id, "a", other.clone())).is_err());
    assert!(r.b().handle(complete(&id, "a", other)).is_err());
    let invalid = json!({"event":"action_proposed","data":{"hardknock_session_id":id,"action_id":"a","action":{"type":"shell","command":"x","cwd":"/tmp","unexpected":"x"}}});
    assert!(serde_json::from_value::<AgentEvent>(invalid).is_err());
    assert!(r.b().handle(propose("unknown", "a", action(&f))).is_err());
}
#[test]
fn action_path_does_not_wait_for_evaluator_and_p95_is_bounded() {
    let f = Fixture::new();
    config(&f, vec!["sleep 0.5".into()]);
    let r = Runtime::new(&f.home);
    let id = start(r.b(), &f, "test-adapter", "bench");
    finish(r.b(), &id);
    let mut timings = Vec::new();
    for n in 0..200 {
        let now = Instant::now();
        r.b()
            .handle(propose(&id, &format!("a{n}"), action(&f)))
            .unwrap();
        timings.push(now.elapsed().as_micros());
    }
    timings.sort_unstable();
    let p95 = timings[189];
    println!(
        "BRIDGE_PRE_ACTION_P95_US={p95} N=200 (including enqueue; background evaluator running)"
    );
    assert!(p95 < 25000, "p95 {p95}us exceeded 25ms");
}
#[test]
fn independent_user_policy_is_the_only_block_source() {
    let f = Fixture::new();
    fs::create_dir_all(&f.home).unwrap();
    let mut c = Config::default();
    c.bridge
        .policy
        .blocked_shell_commands
        .push("printf hello".into());
    fs::write(f.home.join("config.toml"), toml::to_string(&c).unwrap()).unwrap();
    let r = Runtime::new(&f.home);
    let id = start(r.b(), &f, "claude", "one");
    let d = r.b().handle(propose(&id, "a", action(&f))).unwrap();
    assert_eq!(d["decision"], "block");
    assert_eq!(d["authority"], "user_policy");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_unix_and_tcp_transport_and_cleanup() {
    for tcp in [None, Some(0)] {
        let f = Fixture::new();
        let cancel = Cancellation::default();
        let c = cancel.clone();
        let home = f.home.clone();
        let server = tokio::spawn(async move { transport::serve(&home, tcp, &c).await });
        let mut client = BridgeClient::new(&f.home);
        client.timeout = Duration::from_secs(2);
        for _ in 0..100 {
            if f.home.join("run/bridge-endpoint.json").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            client.request(AgentEvent::Status).await.unwrap()["status"],
            "running"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(f.home.join("run/bridge-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let token = fs::read_to_string(f.home.join("run/bridge-token")).unwrap();
        fs::write(f.home.join("run/bridge-token"), "incorrect").unwrap();
        assert!(
            client
                .request(AgentEvent::Status)
                .await
                .unwrap_err()
                .to_string()
                .contains("unauthorized")
        );
        fs::write(f.home.join("run/bridge-token"), token).unwrap();
        client.request(AgentEvent::Shutdown).await.unwrap();
        server.await.unwrap().unwrap();
        assert!(!f.home.join("run/bridge-token").exists());
        assert!(!f.home.join("run/hardknock.sock").exists());
    }
}
#[test]
fn persisted_session_survives_bridge_restart() {
    let f = Fixture::new();
    let id;
    {
        let r = Runtime::new(&f.home);
        id = start(r.b(), &f, "claude", "one");
        r.b().handle(propose(&id, "a", action(&f))).unwrap();
        r.b().flush().unwrap();
    }
    let r = Runtime::new(&f.home);
    let resumed = start(r.b(), &f, "claude", "one");
    assert_eq!(id, resumed);
    assert_eq!(
        r.b()
            .handle(AgentEvent::Inspect {
                hardknock_session_id: id
            })
            .unwrap()["session"]["actions"],
        1
    );
}

#[test]
fn codex_fixture_failure_transfers_to_claude_after_controlled_support() {
    use hardknock::{
        experiment::ExperimentEngine,
        integrations::{claude, codex},
        lesson::{HeuristicConfidence, Lesson, LessonStatus},
        reflection::{CandidateHypothesis, ManualReflection, ReflectionProvider},
        store::LessonStore,
    };
    let a = Fixture::pnpm();
    config(&a, vec!["./test.sh".into()]);
    let r = Runtime::new(&a.home);
    let id = start(r.b(), &a, "codex", "codex-protocol-fixture");
    let mut native: Value = include_str!("../integrations/codex/fixtures/lifecycle.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .nth(4)
        .unwrap()["params"]["item"]
        .clone();
    native["cwd"] = json!(a.repo);
    let baseline = codex::normalize_item(&native).unwrap().unwrap();
    r.b()
        .handle(propose(&id, "codex-tool", baseline.clone()))
        .unwrap();
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", "./agent-script.sh baseline"])
        .current_dir(&a.repo)
        .status()
        .unwrap();
    assert!(status.success());
    r.b().handle(complete(&id, "codex-tool", baseline)).unwrap();
    let first = finish(r.b(), &id);
    r.b().flush().unwrap();
    let store = Store::open(&a.home).unwrap();
    let source = store
        .experience(&first["experience_id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert_eq!(source.outcome, Outcome::Failure);
    assert_eq!(source.agent.kind, "codex");
    // Explicit fixture hypothesis, not a claim that a live model inferred it.
    let mut h: CandidateHypothesis = ManualReflection {
        claim: "The fixture package-manager mismatch creates conflicting state".into(),
        avoid: "./agent-script.sh baseline".into(),
        prefer: "./agent-script.sh alternative".into(),
    }
    .reflect(&source)
    .unwrap()
    .remove(0);
    h.generated_by = source.agent.clone();
    h.context_match.repository = None;
    h.context_match.required_markers = vec![
        "pnpm-workspace.yaml".into(),
        "hardknock-fixture.json".into(),
    ];
    h.context_match.tags = vec!["fixture-family:pnpm-workspace-v2".into()];
    store.insert_hypothesis(&h).unwrap();
    let candidate = Lesson::candidate(&h, &HeuristicConfidence);
    LessonStore::insert(&store, &candidate).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let experiment = rt
        .block_on(
            ExperimentEngine { store: &store }.execute(&candidate.id, &Cancellation::default()),
        )
        .unwrap();
    assert!(experiment.plan.external_reconstruction);
    assert_eq!(
        store.lesson(&candidate.id).unwrap().status,
        LessonStatus::CounterfactuallySupported
    );
    let mut b = Fixture::from_fixture("pnpm-workspace-transfer");
    b.home = a.home.clone();
    // A separate Bridge load picks up explicit evaluator configuration for fixture B.
    drop(r);
    let mut c = Config::load(&a.home).unwrap();
    c.bridge.evaluators.insert(
        b.repo.canonicalize().unwrap().display().to_string(),
        vec!["./test.sh".into()],
    );
    fs::write(a.home.join("config.toml"), toml::to_string(&c).unwrap()).unwrap();
    let r = Runtime::new(&a.home);
    let id = start(r.b(), &b, "claude", "claude-hook-fixture");
    let context = r
        .b()
        .handle(AgentEvent::ContextRequested(ContextRequested {
            hardknock_session_id: id.clone(),
            task: Some("Upgrade the different service and worker packages".into()),
        }))
        .unwrap();
    assert_eq!(
        context["relevant_experience"][0]["id"],
        candidate.id.to_string()
    );
    let bad = claude::normalize(
        "Bash",
        &json!({"command":"./agent-script.sh baseline"}),
        b.repo.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        r.b()
            .handle(AgentEvent::ActionProposed(ActionProposed {
                hardknock_session_id: id.clone(),
                action_id: "bad".into(),
                action: bad,
                context: ActionContext {
                    can_intercept: true,
                    ..Default::default()
                },
            }))
            .unwrap()["decision"],
        "advise"
    );
    let good = claude::normalize(
        "Bash",
        &json!({"command":"./agent-script.sh alternative"}),
        b.repo.to_str().unwrap(),
    )
    .unwrap();
    r.b().handle(propose(&id, "good", good.clone())).unwrap();
    assert!(
        std::process::Command::new("/bin/sh")
            .args(["-c", "./agent-script.sh alternative"])
            .current_dir(&b.repo)
            .status()
            .unwrap()
            .success()
    );
    r.b().handle(complete(&id, "good", good)).unwrap();
    let transfer = finish(r.b(), &id);
    r.b().flush().unwrap();
    let exp = store
        .experience(&transfer["experience_id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert_eq!(exp.outcome, Outcome::Success);
    assert_eq!(exp.agent.kind, "claude");
    assert_eq!(
        exp.lesson_applications[0].verification,
        hardknock::application::ApplicationVerification::Observed
    );
    assert_eq!(
        store.lesson(&candidate.id).unwrap().status,
        LessonStatus::Validated
    );
    let provenance = store.lesson_agent_provenance(&candidate.id).unwrap();
    assert!(
        provenance["contributions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["role"] == "successful_transfer" && c["agent"]["kind"] == "claude")
    );
    for (external, reason) in [
        ("reject-1", RejectionReason::ContextMismatch),
        ("reject-2", RejectionReason::EnvironmentChanged),
    ] {
        let id = start(r.b(), &b, "claude", external);
        r.b()
            .handle(AgentEvent::LessonRejected(LessonFeedback {
                hardknock_session_id: id,
                lesson_id: candidate.id.to_string(),
                reason,
                detail: Some("Fixture-only rejection".into()),
            }))
            .unwrap();
    }
    r.b().flush().unwrap();
    assert_eq!(
        store.lesson_agent_provenance(&candidate.id).unwrap()["needs_revalidation"],
        true
    );
    assert_eq!(
        store.lesson(&candidate.id).unwrap().status,
        LessonStatus::Validated
    );
}

#[test]
fn runner_shared_experience_budget_limits_trials_and_retries() {
    let f = Fixture::pnpm();
    let result = f.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "--experience-budget",
            "2",
            "upgrade dependencies",
        ],
        1,
    );
    assert_eq!(result["experiment"]["trials"].as_array().unwrap().len(), 2);
    assert!(result["retries"].as_array().unwrap().is_empty());
    assert_eq!(
        result["retry_stop_reason"],
        "Shared experience budget exhausted"
    );
}

#[test]
fn cache_reflex_mapping_scope_and_thousand_rule_latency() {
    use hardknock::{
        core::{ChaosTrialId, EnvironmentMode, ReflexId},
        experience::ExperienceContext,
        lesson::{ActionPattern, ContextSelector},
        resilience::{Reflex, ReflexResponse, ReflexStatus, TriggerPattern},
    };
    let f = Fixture::new();
    let r = Runtime::new(&f.home);
    let id = start(r.b(), &f, "claude", "latency");
    let context = ExperienceContext::capture(
        &hardknock::dojo::capture_state(&f.repo).unwrap(),
        &f.repo.canonicalize().unwrap(),
        EnvironmentMode::Inherited,
    )
    .unwrap();
    let reflex = Reflex {
        id: ReflexId::new(),
        version: 1,
        source_lessons: vec![],
        source_trial: ChaosTrialId::new(),
        trigger: TriggerPattern {
            context: ContextSelector::from_context(&context),
            proposed_action: ActionPattern::shell("printf hello"),
            repeated_failures: None,
            no_state_change: false,
            config_changed: false,
        },
        response: ReflexResponse::Warn,
        confidence: 0.8.try_into().unwrap(),
        status: ReflexStatus::Supported,
        evidence: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    r.b().cache.write().unwrap().reflexes = vec![reflex.clone()];
    // Synthetic in-memory rules must carry the same freshness metadata as loaded rules.
    let basis = hardknock::development::FreshnessBasis {
        origin_context: None,
        last_supported_at: chrono::Utc::now(),
        context: context.clone(),
        agent: hardknock::core::AgentIdentity {
            kind: "benchmark".into(),
            executable: "none".into(),
            version: None,
            model: None,
        },
        contradicted: false,
    };
    r.b()
        .cache
        .write()
        .unwrap()
        .reflex_freshness
        .insert(reflex.id.clone(), basis.clone());
    assert_eq!(
        r.b().handle(propose(&id, "supported", action(&f))).unwrap()["decision"],
        "warn"
    );
    let mut active = reflex;
    active.status = ReflexStatus::Active;
    r.b().cache.write().unwrap().reflexes = vec![active.clone()];
    assert_eq!(
        r.b().handle(propose(&id, "active", action(&f))).unwrap()["decision"],
        "replan"
    );
    active.trigger.context.os = Some("unmatched-os".into());
    r.b().cache.write().unwrap().reflexes = vec![active.clone(); 1000];
    assert_eq!(
        r.b()
            .handle(propose(&id, "different-scope", action(&f)))
            .unwrap()["decision"],
        "continue"
    );
    // All 1,000 lessons match the context/action; all reflexes need a scope check.
    // This deliberately exercises ranking and cloning, not only a fast empty-cache path.
    use hardknock::{
        core::{ExperienceId, HypothesisId, LessonId},
        lesson::{HeuristicConfidence, Lesson, LessonStatus},
        reflection::CandidateHypothesis,
    };
    let hypothesis = CandidateHypothesis {
        id: HypothesisId::new(),
        source_experience: ExperienceId::new(),
        created_at: chrono::Utc::now(),
        claim: "Benchmark-only evidence".into(),
        rationale: "Synthetic cache workload, never persisted as evidence".into(),
        context_match: ContextSelector::from_context(&context),
        avoid: ActionPattern::shell("printf hello"),
        prefer: ActionPattern::shell("printf safe"),
        generated_by: hardknock::core::AgentIdentity {
            kind: "benchmark".into(),
            executable: "none".into(),
            version: None,
            model: None,
        },
    };
    let mut lesson = Lesson::candidate(&hypothesis, &HeuristicConfidence);
    lesson.status = LessonStatus::CounterfactuallySupported;
    r.b().cache.write().unwrap().lessons = (0..1000)
        .map(|_| {
            let mut l = lesson.clone();
            l.id = LessonId::new();
            l
        })
        .collect();
    let mut timings = Vec::new();
    {
        let mut cache = r.b().cache.write().unwrap();
        cache.freshness = cache
            .lessons
            .iter()
            .map(|l| (l.id.clone(), basis.clone()))
            .collect();
    }
    for index in 0..200 {
        let started = Instant::now();
        let decision = r
            .b()
            .handle(propose(&id, &format!("bench-{index}"), action(&f)))
            .unwrap();
        timings.push(started.elapsed().as_micros());
        assert_eq!(decision["decision"], "advise");
    }
    timings.sort_unstable();
    println!(
        "BRIDGE_1000_LESSONS_1000_REFLEXES_P95_US={} N=200 (full action handler; debug build)",
        timings[189]
    );
    assert!(timings[189] < 25000, "actual P95={}us", timings[189]);
    let mut retrieved = r.b().cache.read().unwrap().retrieve(
        &context,
        "",
        vec![ActionPattern::shell("printf hello")],
    );
    retrieved[0].lesson.claim = "\"quoted 日本語\" ".repeat(500);
    let config = hardknock::bridge::config::BridgeConfig {
        max_context_bytes: 4096,
        ..Default::default()
    };
    let response = hardknock::bridge::cache::context_response(&id, &retrieved, &config);
    assert!(serde_json::to_vec(&response).unwrap().len() <= config.max_context_bytes);
}

#[tokio::test]
async fn client_deadline_bounds_a_stalled_local_peer() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    Store::open(&f.home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    for (name, text) in [
        ("bridge-token", "private-test-token".into()),
        (
            "bridge-endpoint.json",
            json!({"transport":"tcp","address":listener.local_addr().unwrap().to_string()})
                .to_string(),
        ),
    ] {
        let path = f.home.join("run").join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let stalled = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let mut client = BridgeClient::new(&f.home);
    client.timeout = Duration::from_millis(30);
    let started = Instant::now();
    let result = client.request(AgentEvent::Status).await;
    stalled.abort();
    let _ = stalled.await;
    assert!(result.unwrap_err().to_string().contains("timeout"));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_shutdown_cancels_and_reaps_a_running_evaluator() {
    let f = Fixture::new();
    config(&f, vec!["echo $$ > evaluator.pid; sleep 30".into()]);
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &c).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut client = BridgeClient::new(&f.home);
    client.timeout = Duration::from_secs(2);
    let session = client
        .request(AgentEvent::SessionStarted(SessionStarted {
            session_id: "shutdown-test".into(),
            agent: AgentIdentity::new("test-adapter"),
            cwd: f.repo.display().to_string(),
            repository: None,
            task: None,
            environment: Default::default(),
        }))
        .await
        .unwrap();
    let run = client
        .request(AgentEvent::RunCompleted(RunCompleted {
            hardknock_session_id: session["hardknock_session_id"].as_str().unwrap().into(),
            run_id: "run".into(),
            success: Some(true),
            final_message: None,
            duration_ms: 0,
            termination: RunTermination::Completed,
            external_metadata: Value::Null,
        }))
        .await
        .unwrap();
    for _ in 0..100 {
        if f.repo.join("evaluator.pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = Instant::now();
    cancel.cancel();
    server.await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid: i32 = fs::read_to_string(f.repo.join("evaluator.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    let store = Store::open(&f.home).unwrap();
    assert_eq!(
        store
            .experience(&run["experience_id"].as_str().unwrap().parse().unwrap())
            .unwrap()
            .outcome,
        Outcome::Interrupted
    );
    assert!(!f.home.join("run/bridge-token").exists());
}

#[tokio::test]
async fn runtime_refuses_foreign_paths_without_deleting_them() {
    let f = Fixture::new();
    Store::open(&f.home).unwrap();
    let path = f.home.join("run/hardknock.sock");
    fs::write(&path, "user data").unwrap();
    assert!(
        transport::serve(&f.home, None, &Cancellation::default())
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "user data");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_rejects_bad_versions_unknown_fields_and_oversized_frames() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };
    let f = Fixture::new();
    let c = Cancellation::default();
    let cancel = c.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, Some(0), &cancel).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let endpoint: Value =
        serde_json::from_slice(&fs::read(f.home.join("run/bridge-endpoint.json")).unwrap())
            .unwrap();
    let address = endpoint["address"].as_str().unwrap();
    let token = fs::read_to_string(f.home.join("run/bridge-token")).unwrap();
    for (version, payload, expected) in [
        ("unknown", json!({"event":"status"}), "unsupported_protocol"),
        (
            PROTOCOL_VERSION,
            json!({"event":"session_started","data":{"secret":"never returned"}}),
            "invalid_event",
        ),
    ] {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let body = format!(
            "{}\n",
            json!({"protocol_version":version,"request_id":"r","token":token,"payload":payload})
        );
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["error"]["code"], expected);
        assert!(!String::from_utf8_lossy(&bytes).contains("never returned"));
    }
    let mut stream = TcpStream::connect(address).await.unwrap();
    let _ = stream.write_all(&vec![b'x'; MAX_EVENT_BYTES + 1]).await;
    let mut bytes = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut bytes))
        .await
        .unwrap();
    assert!(bytes.is_empty());
    c.cancel();
    server.await.unwrap().unwrap();
}
