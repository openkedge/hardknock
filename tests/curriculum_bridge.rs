// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::{
        protocol::{self, AgentEvent},
        transport::{self, BridgeClient},
    },
    cancellation::Cancellation,
    core::*,
    curriculum::CurriculumStatus,
    store::Store,
};
use std::{fs, time::Duration};
use support::Fixture;
fn seed(f: &Fixture) {
    let r = f.cli(
        &[
            "chaos",
            "run",
            "--agent",
            "test-agent",
            "--perturb",
            "delay:0",
        ],
        0,
    );
    let id = r["result"]["campaign"]["control"]["experience_id"]
        .as_str()
        .unwrap();
    f.cli(
        &[
            "skill",
            "register",
            "process-task-successfully",
            "--experience",
            id,
        ],
        0,
    );
}
async fn connect(
    f: &Fixture,
) -> (
    BridgeClient,
    Cancellation,
    tokio::task::JoinHandle<hardknock::Result<()>>,
) {
    let home = f.home.clone();
    let shutdown = Cancellation::default();
    let cancel = shutdown.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &cancel).await });
    let mut client = BridgeClient::new(&f.home);
    client.timeout = Duration::from_secs(5);
    for _ in 0..200 {
        if client.request(AgentEvent::Status).await.is_ok() {
            return (client, shutdown, server);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("Bridge not ready")
}
async fn session(client: &BridgeClient, f: &Fixture, agent: &str) -> String {
    let r = client
        .request(AgentEvent::SessionStarted(protocol::SessionStarted {
            session_id: format!("curriculum-{agent}"),
            agent: protocol::AgentIdentity::new(agent),
            cwd: f.repo.to_string_lossy().into(),
            repository: None,
            task: Some("harden skill".into()),
            environment: Default::default(),
        }))
        .await
        .unwrap();
    r["hardknock_session_id"].as_str().unwrap().into()
}
fn request(session: &str, trials: usize) -> protocol::CurriculumRequested {
    protocol::CurriculumRequested {
        hardknock_session_id: session.into(),
        request_id: CurriculumId::new(),
        target: protocol::CurriculumRequestTarget::Skill {
            skill: "process-task-successfully".into(),
        },
        profile: "resilience-basic".into(),
        budget: protocol::CurriculumRequestBudget { max_trials: trials },
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_and_codex_use_explicit_plan_start_progress_with_session_budgets() {
    for agent in ["claude", "codex"] {
        let f = Fixture::from_fixture("skill-hardening");
        seed(&f);
        fs::write(
            f.home.join("config.toml"),
            "[curriculum]\nagent_requests=true\nmax_agent_session_trials=2\n",
        )
        .unwrap();
        let (client, shutdown, server) = connect(&f).await;
        let s = session(&client, &f, agent).await;
        let wire = request(&s, 2);
        let id = wire.request_id.clone();
        let before = Store::open(&f.home).unwrap().realities().unwrap().len();
        let planned = client
            .request(AgentEvent::CurriculumRequested(wire.clone()))
            .await
            .unwrap();
        assert_eq!(planned["event"], "curriculum_planned");
        assert_eq!(planned["requires_start"], true);
        assert_eq!(
            Store::open(&f.home).unwrap().realities().unwrap().len(),
            before
        );
        let again = client
            .request(AgentEvent::CurriculumRequested(wire))
            .await
            .unwrap();
        assert_eq!(again["curriculum_id"], id.to_string());
        assert!(
            client
                .request(AgentEvent::CurriculumRequested(request(&s, 1)))
                .await
                .is_err()
        );
        let foreign = session(
            &client,
            &f,
            if agent == "claude" { "codex" } else { "claude" },
        )
        .await;
        assert!(
            client
                .request(AgentEvent::CurriculumStarted {
                    hardknock_session_id: foreign,
                    curriculum_id: id.clone()
                })
                .await
                .is_err()
        );
        client
            .request(AgentEvent::CurriculumStarted {
                hardknock_session_id: s.clone(),
                curriculum_id: id.clone(),
            })
            .await
            .unwrap();
        let mut complete = None;
        let mut after = 0;
        let mut events = vec![];
        for _ in 0..400 {
            let r = client
                .request(AgentEvent::CurriculumProgress {
                    hardknock_session_id: s.clone(),
                    curriculum_id: id.clone(),
                    after,
                })
                .await
                .unwrap();
            assert!(serde_json::to_vec(&r).unwrap().len() < 65536);
            for e in r["progress"].as_array().unwrap() {
                after = e[0].as_u64().unwrap();
                events.push(e[1]["event"].as_str().unwrap().to_owned());
            }
            if r["event"] == "curriculum_completed" {
                complete = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let r = complete.expect("Curriculum did not finish");
        assert_eq!(r["status"], "completed", "{r}");
        assert_eq!(r["result"]["summary"]["trials"], 2);
        assert!(events.iter().any(|e| e == "curriculum_trial_completed"));
        let package = client
            .request(AgentEvent::SkillPackageRequested {
                hardknock_session_id: s,
                skill: "process-task-successfully".into(),
                profile: "resilience-basic".into(),
            })
            .await
            .unwrap();
        assert!(package["profile_coverage"].is_object());
        assert_ne!(package["maturity"], "hardened");
        shutdown.cancel();
        server.await.unwrap().unwrap();
        f.assert_source_unchanged();
        assert!(
            Store::open(&f.home)
                .unwrap()
                .realities()
                .unwrap()
                .iter()
                .all(|r| r.status == RealityStatus::Discarded)
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn curriculum_requests_default_off_and_session_end_cancels_started_work() {
    let f = Fixture::from_fixture("skill-hardening");
    seed(&f);
    let (client, shutdown, server) = connect(&f).await;
    let s = session(&client, &f, "claude").await;
    assert!(
        client
            .request(AgentEvent::CurriculumRequested(request(&s, 2)))
            .await
            .is_err()
    );
    shutdown.cancel();
    server.await.unwrap().unwrap();
    fs::write(
        f.home.join("config.toml"),
        "[curriculum]\nagent_requests=true\n",
    )
    .unwrap();
    let (client, shutdown, server) = connect(&f).await;
    let s = session(&client, &f, "codex").await;
    let wire = request(&s, 2);
    let id = wire.request_id.clone();
    client
        .request(AgentEvent::CurriculumRequested(wire))
        .await
        .unwrap();
    client
        .request(AgentEvent::CurriculumStarted {
            hardknock_session_id: s.clone(),
            curriculum_id: id.clone(),
        })
        .await
        .unwrap();
    client
        .request(AgentEvent::SessionEnded(protocol::SessionEnded {
            hardknock_session_id: s.clone(),
        }))
        .await
        .unwrap();
    for _ in 0..300 {
        if Store::open(&f.home)
            .unwrap()
            .curriculum(&id)
            .unwrap()
            .status
            .terminal()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let c = Store::open(&f.home).unwrap().curriculum(&id).unwrap();
    assert_eq!(c.status, CurriculumStatus::Cancelled);
    assert!(
        client
            .request(AgentEvent::CurriculumStarted {
                hardknock_session_id: s,
                curriculum_id: id
            })
            .await
            .is_err()
    );
    let unstarted_session = session(&client, &f, "unstarted").await;
    let wire = request(&unstarted_session, 1);
    let planned_id = wire.request_id.clone();
    client
        .request(AgentEvent::CurriculumRequested(wire))
        .await
        .unwrap();
    client
        .request(AgentEvent::SessionEnded(protocol::SessionEnded {
            hardknock_session_id: unstarted_session,
        }))
        .await
        .unwrap();
    let planned = Store::open(&f.home)
        .unwrap()
        .curriculum(&planned_id)
        .unwrap();
    assert_eq!(planned.status, CurriculumStatus::Cancelled);
    assert_eq!(planned.trials_executed, 0);
    shutdown.cancel();
    server.await.unwrap().unwrap();
    f.assert_source_unchanged();
}
