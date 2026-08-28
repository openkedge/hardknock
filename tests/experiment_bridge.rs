// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::{
        protocol::{self, AgentEvent},
        transport::{self, BridgeClient},
    },
    budget::ExperienceBudget,
    cancellation::Cancellation,
    core::{CandidateId, ExperimentId, ExperimentRequestId},
    evaluation::EvaluationSpec,
    experimentation::*,
    store::{ExperimentStore, Store},
};
use std::{collections::BTreeSet, time::Duration};
use support::Fixture;

async fn connect(
    f: &Fixture,
) -> (
    BridgeClient,
    Cancellation,
    tokio::task::JoinHandle<hardknock::Result<()>>,
) {
    let home = f.home.clone();
    let cancel = Cancellation::default();
    let shutdown = cancel.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &shutdown).await });
    let mut client = BridgeClient::new(&f.home);
    client.timeout = Duration::from_secs(5);
    for _ in 0..200 {
        if client.request(AgentEvent::Status).await.is_ok() {
            return (client, cancel, server);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Bridge did not become ready");
}
async fn start(client: &BridgeClient, f: &Fixture, agent: &str) -> String {
    let result = client
        .request(AgentEvent::SessionStarted(protocol::SessionStarted {
            session_id: format!("session-{agent}"),
            agent: protocol::AgentIdentity::new(agent),
            cwd: f.repo.to_string_lossy().into(),
            repository: None,
            task: Some("Choose an upgrade strategy".into()),
            environment: Default::default(),
        }))
        .await
        .unwrap();
    assert!(
        result["context_document"]
            .as_str()
            .unwrap()
            .contains("hardknock try --session")
    );
    result["hardknock_session_id"].as_str().unwrap().into()
}
fn request(session: &str) -> protocol::ExperimentRequested {
    protocol::ExperimentRequested {
        hardknock_session_id: session.into(),
        request_id: ExperimentRequestId::new(),
        question: "Which upgrade works?".into(),
        hypothesis: None,
        candidates: ["direct", "staged"]
            .into_iter()
            .map(|name| ExperimentCandidate {
                id: CandidateId::new(),
                name: name.into(),
                description: String::new(),
                execution: CandidateExecution::AgentTask {
                    prompt: format!("{name}-upgrade"),
                    agent: Some(hardknock::core::AgentIdentity {
                        kind: "test-agent".into(),
                        executable: "unused".into(),
                        version: None,
                        model: None,
                    }),
                },
                expected_outcome: None,
            })
            .collect(),
        evaluator: EvaluationSpec {
            checks: vec!["./test.sh".into()],
        },
        budget: ExperienceBudget::default(),
        criteria: ComparisonCriteria::default(),
        capabilities: ExperimentCapabilities::default(),
        intent: ExperimentIntent::CompareStrategies,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fake_integrated_agent_receives_request_progress_result_and_session_spending_is_bounded() {
    let f = Fixture::from_fixture("strategy-choice");
    let (client, shutdown, server) = connect(&f).await;
    let session = start(&client, &f, "claude").await;
    let wire = request(&session);
    let accepted = client
        .request(AgentEvent::ExperimentRequested(wire.clone()))
        .await
        .unwrap();
    assert_eq!(accepted["event"], "experiment_accepted");
    assert!(
        accepted["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("Session snapshot unavailable"))
    );
    let duplicate = client
        .request(AgentEvent::ExperimentRequested(wire))
        .await
        .unwrap();
    assert_eq!(accepted["experiment_id"], duplicate["experiment_id"]);
    let id: ExperimentId = serde_json::from_value(accepted["experiment_id"].clone()).unwrap();
    let mut after = 0;
    let mut phases = BTreeSet::new();
    let mut complete = None;
    for _ in 0..300 {
        let result = client
            .request(AgentEvent::ExperimentProgress {
                hardknock_session_id: session.clone(),
                experiment_id: id.clone(),
                after,
            })
            .await
            .unwrap();
        for entry in result["progress"].as_array().unwrap() {
            let sequence = entry[0].as_u64().unwrap();
            assert!(sequence > after);
            after = sequence;
            phases.insert(entry[1]["phase"].as_str().unwrap().to_owned());
        }
        if result["status"] == "completed" {
            complete = Some(result);
            break;
        }
        assert!(client.request(AgentEvent::Status).await.is_ok());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let complete = complete.expect("experiment did not finish");
    assert_eq!(complete["event"], "experiment_completed");
    assert_eq!(complete["result"]["quality"], "controlled");
    assert_eq!(
        complete["result"]["created_experience"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for phase in [
        "preparing",
        "executing",
        "evaluating",
        "comparing",
        "learning",
        "completed",
    ] {
        assert!(phases.contains(phase), "missing {phase}");
    }
    let exhausted = client
        .request(AgentEvent::ExperimentRequested(request(&session)))
        .await
        .unwrap();
    assert_eq!(exhausted["event"], "experiment_rejected");
    assert!(
        exhausted["reason"]
            .as_str()
            .unwrap()
            .contains("session Reality budget")
    );
    let codex_session = start(&client, &f, "codex").await;
    assert!(
        client
            .request(AgentEvent::ExperimentProgress {
                hardknock_session_id: codex_session.clone(),
                experiment_id: id,
                after: 0
            })
            .await
            .is_err()
    );
    let codex = client
        .request(AgentEvent::ExperimentRequested(request(&codex_session)))
        .await
        .unwrap();
    let codex_id: ExperimentId = serde_json::from_value(codex["experiment_id"].clone()).unwrap();
    for _ in 0..300 {
        let progress = client
            .request(AgentEvent::ExperimentProgress {
                hardknock_session_id: codex_session.clone(),
                experiment_id: codex_id.clone(),
                after: 0,
            })
            .await
            .unwrap();
        if progress["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        Store::open(&f.home)
            .unwrap()
            .strategy_experiment(&codex_id)
            .unwrap()
            .status,
        ExperimentStatus::Completed
    );
    client
        .request(AgentEvent::SessionEnded(protocol::SessionEnded {
            hardknock_session_id: session,
        }))
        .await
        .unwrap();
    shutdown.cancel();
    server.await.unwrap().unwrap();
    f.assert_source_unchanged();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_end_cancels_native_origin_candidate_and_retains_partial_evidence() {
    let f = Fixture::from_fixture("strategy-choice");
    let (client, shutdown, server) = connect(&f).await;
    let session = start(&client, &f, "codex").await;
    let mut wire = request(&session);
    for c in &mut wire.candidates {
        c.execution = CandidateExecution::Shell {
            commands: vec!["printf started > started; sleep 60".into()],
        };
    }
    let accepted = client
        .request(AgentEvent::ExperimentRequested(wire))
        .await
        .unwrap();
    let id: ExperimentId = serde_json::from_value(accepted["experiment_id"].clone()).unwrap();
    let store = Store::open(&f.home).unwrap();
    let mut running = false;
    for _ in 0..200 {
        if store
            .realities()
            .unwrap()
            .iter()
            .any(|r| r.root.join("started").exists())
        {
            running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(running);
    client
        .request(AgentEvent::SessionEnded(protocol::SessionEnded {
            hardknock_session_id: session.clone(),
        }))
        .await
        .unwrap();
    for _ in 0..200 {
        let e = client
            .request(AgentEvent::ExperimentProgress {
                hardknock_session_id: session.clone(),
                experiment_id: id.clone(),
                after: 0,
            })
            .await
            .unwrap();
        if e["status"] == "cancelled" {
            assert_eq!(e["event"], "experiment_cancelled");
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let e = store.strategy_experiment(&id).unwrap();
    assert_eq!(e.status, ExperimentStatus::Cancelled);
    let result = e.result.unwrap();
    assert!(result.recommendation.is_none());
    assert!(!result.created_experience.is_empty());
    for id in result.created_experience {
        assert_eq!(
            store.experience(&id).unwrap().outcome,
            hardknock::experience::Outcome::Interrupted
        );
    }
    assert!(
        client
            .request(AgentEvent::ExperimentRequested(request(&session)))
            .await
            .is_err()
    );
    shutdown.cancel();
    server.await.unwrap().unwrap();
    f.assert_source_unchanged();
}

#[test]
fn bridge_configuration_can_disable_agent_requests_independently() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    std::fs::write(
        f.home.join("config.toml"),
        "[experiments.agent_requests]\nenabled = false\n",
    )
    .unwrap();
    let (bridge, worker) = hardknock::bridge::Bridge::open(&f.home).unwrap();
    let started = bridge
        .handle(AgentEvent::SessionStarted(protocol::SessionStarted {
            session_id: "disabled".into(),
            agent: protocol::AgentIdentity::new("claude"),
            cwd: f.repo.to_string_lossy().into(),
            repository: None,
            task: None,
            environment: Default::default(),
        }))
        .unwrap();
    let session = started["hardknock_session_id"].as_str().unwrap();
    let result = bridge
        .handle(AgentEvent::ExperimentRequested(request(session)))
        .unwrap();
    assert_eq!(result["event"], "experiment_rejected");
    assert!(store.realities().unwrap().is_empty());
    assert_eq!(ExperimentStore::list(&store, None).unwrap().len(), 1);
    bridge.flush().unwrap();
    drop(bridge);
    worker.join().unwrap();
}
