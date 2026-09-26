// SPDX-License-Identifier: Apache-2.0
mod support;

use chrono::{Duration as ChronoDuration, Utc};
use hardknock::{
    bridge::{
        Bridge,
        config::Config,
        engine::session_key,
        protocol::{
            ActionContext, ActionProposed, AgentEvent, AgentIdentity, AgentMessage,
            ContextRequested, NormalizedAction, RunCompleted, SessionEnded, SessionStarted,
        },
    },
    core::{AgentIdentity as CoreAgentIdentity, HardknockSessionId},
    store::{RuntimeStore, Store},
    team::{
        AgentRole, AgentTeam, AgentTeamMember, BuiltInAgentRole, RoleAssignment, TeamRuntimeContext,
    },
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread::JoinHandle,
};
use support::Fixture;

fn database(home: &Path) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(home.join("hardknock.db")).unwrap();
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    connection
}

struct Runtime {
    bridge: Option<Arc<Bridge>>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    fn open(home: &Path) -> Self {
        let (bridge, worker) = Bridge::open(home).unwrap();
        Self {
            bridge: Some(bridge),
            worker: Some(worker),
        }
    }

    fn bridge(&self) -> &Bridge {
        self.bridge.as_ref().unwrap()
    }

    fn bridge_arc(&self) -> Arc<Bridge> {
        Arc::clone(self.bridge.as_ref().unwrap())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            let _ = bridge.flush();
            drop(bridge);
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn configure(fixture: &Fixture, max_sessions: usize) {
    fs::create_dir_all(&fixture.home).unwrap();
    let mut config = Config::default();
    config.bridge.max_sessions = max_sessions;
    fs::write(
        fixture.home.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
}

fn start_event(repo: &Path, external_id: impl Into<String>) -> AgentEvent {
    AgentEvent::SessionStarted(SessionStarted {
        session_id: external_id.into(),
        agent: AgentIdentity::new("capacity-test"),
        cwd: repo.to_string_lossy().into_owned(),
        repository: None,
        task: Some("Verify Bridge session admission".into()),
        environment: Default::default(),
    })
}

fn start_and_end(runtime: &Runtime, fixture: &Fixture, external_id: &str) -> String {
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, external_id))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap();
    session_id
}

fn assert_historical_session(runtime: &Runtime, session_id: &str) {
    let inspected = runtime
        .bridge()
        .handle(AgentEvent::Inspect {
            hardknock_session_id: session_id.into(),
        })
        .unwrap();
    assert_eq!(inspected["session"]["ended"], true);
}

fn saturate_writer_queue(runtime: &Runtime, home: &Path, session_id: &str) -> rusqlite::Connection {
    let connection = database(home);
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    for index in 0..8192 {
        match runtime
            .bridge()
            .handle(AgentEvent::AgentMessage(AgentMessage {
                hardknock_session_id: session_id.into(),
                summary: format!("queue-pressure-{index}"),
            })) {
            Ok(_) => {}
            Err(error) if error.to_string().contains("queue full") => return connection,
            Err(error) => panic!("unexpected queue saturation failure: {error}"),
        }
    }
    panic!("Bridge persistence queue did not saturate");
}

fn flush_after_pressure(runtime: &Runtime) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match runtime.bridge().flush() {
            Ok(()) => return,
            Err(error)
                if error.to_string().contains("queue full")
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            Err(error) => panic!("Bridge persistence queue did not drain: {error}"),
        }
    }
}

#[test]
fn ended_sessions_release_capacity_before_and_after_engine_reload() {
    const MAX_SESSIONS: usize = 2;
    let fixture = Fixture::new();
    configure(&fixture, MAX_SESSIONS);

    let first_generation = {
        let runtime = Runtime::open(&fixture.home);
        let sessions = (0..=MAX_SESSIONS)
            .map(|index| start_and_end(&runtime, &fixture, &format!("before-reload-{index}")))
            .collect::<Vec<_>>();
        for session_id in &sessions {
            assert_historical_session(&runtime, session_id);
        }
        assert_eq!(
            runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
            0
        );
        runtime.bridge().flush().unwrap();
        sessions
    };

    let second_generation = {
        let runtime = Runtime::open(&fixture.home);
        for session_id in &first_generation {
            assert_historical_session(&runtime, session_id);
        }
        let sessions = (0..=MAX_SESSIONS)
            .map(|index| start_and_end(&runtime, &fixture, &format!("after-reload-{index}")))
            .collect::<Vec<_>>();
        for session_id in first_generation.iter().chain(&sessions) {
            assert_historical_session(&runtime, session_id);
        }
        assert_eq!(
            runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
            0
        );
        runtime.bridge().flush().unwrap();
        sessions
    };

    let persisted = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap();
    assert_eq!(
        persisted.len(),
        first_generation.len() + second_generation.len()
    );
    assert!(persisted.iter().all(|session| session.ended));
}

#[test]
fn ended_session_churn_keeps_live_state_bounded_and_history_inspectable() {
    const LIVE_CHURN: usize = 64;
    const DURABLE_HISTORY: usize = 2048;
    let fixture = Fixture::new();
    configure(&fixture, 1);

    let mut sampled = {
        let runtime = Runtime::open(&fixture.home);
        let mut sampled = Vec::new();
        for index in 0..LIVE_CHURN {
            let session_id = start_and_end(&runtime, &fixture, &format!("churn-session-{index}"));
            if matches!(index, 0 | 31 | 63) {
                sampled.push(session_id);
            }
            assert_eq!(
                runtime.bridge().handle(AgentEvent::Sessions).unwrap()["sessions"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
        }
        for session_id in &sampled {
            assert_historical_session(&runtime, session_id);
        }
        sampled
    };

    let store = Store::open(&fixture.home).unwrap();
    let template = store.bridge_sessions().unwrap().into_iter().next().unwrap();
    for index in LIVE_CHURN..DURABLE_HISTORY {
        let mut historical = template.clone();
        historical.id = format!("seeded-ended-session-{index}");
        historical.external_id = format!("seeded-external-{index}");
        historical.revision = u64::try_from(index).unwrap().saturating_add(1);
        historical.trajectory_id = None;
        store.save_bridge_session(&historical).unwrap();
    }
    sampled.push(format!(
        "seeded-ended-session-{}",
        DURABLE_HISTORY.saturating_sub(1)
    ));
    drop(store);

    let runtime = Runtime::open(&fixture.home);
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Sessions).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    for session_id in &sampled {
        assert_historical_session(&runtime, session_id);
    }
    drop(runtime);

    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .bridge_sessions()
            .unwrap()
            .len(),
        DURABLE_HISTORY
    );
}

#[test]
fn concurrent_starts_reserve_capacity_before_creating_trajectories() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let barrier = Arc::new(Barrier::new(2));
    let repo = fixture.repo.clone();

    let workers = (0..2)
        .map(|index| {
            let bridge = runtime.bridge_arc();
            let barrier = Arc::clone(&barrier);
            let repo = repo.clone();
            std::thread::spawn(move || {
                barrier.wait();
                bridge.handle(start_event(&repo, format!("concurrent-{index}")))
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    let successes = results.iter().filter(|result| result.is_ok()).count();
    let failures = results.iter().filter(|result| result.is_err()).count();
    assert_eq!((successes, failures), (1, 1));
    let failure = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap()
        .to_string();
    assert!(failure.contains("session budget exhausted"), "{failure}");

    let winner = results.into_iter().find_map(Result::ok).unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Sessions).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    runtime.bridge().flush().unwrap();
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectories()
            .unwrap()
            .len(),
        1
    );

    runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: winner,
        }))
        .unwrap();
}

#[test]
fn full_writer_queue_rejects_action_before_any_durable_side_effect() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "action-queue-pressure"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let trajectory_id = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap()
        .trajectory_id
        .unwrap();
    let connection = saturate_writer_queue(&runtime, &fixture.home, &session_id);
    let trajectory_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM trajectory_events WHERE trajectory_id=?1",
            [trajectory_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let runtime_decisions: i64 = connection
        .query_row("SELECT COUNT(*) FROM runtime_decisions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let action_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events WHERE kind='action_proposed'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let started = std::time::Instant::now();
    let error = runtime
        .bridge()
        .handle(AgentEvent::ActionProposed(ActionProposed {
            hardknock_session_id: session_id.clone(),
            action_id: "rejected-before-persistence".into(),
            action: NormalizedAction::Shell {
                command: "printf bridge".into(),
                cwd: fixture.repo.display().to_string(),
            },
            context: Default::default(),
        }))
        .unwrap_err();

    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    assert!(error.to_string().contains("queue full"), "{error}");
    let live_actions = runtime
        .bridge()
        .handle(AgentEvent::Inspect {
            hardknock_session_id: session_id.clone(),
        })
        .unwrap()["session"]["actions"]
        .as_u64()
        .unwrap();
    let persisted_trajectory_events = connection
        .query_row(
            "SELECT COUNT(*) FROM trajectory_events WHERE trajectory_id=?1",
            [trajectory_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let persisted_runtime_decisions = connection
        .query_row("SELECT COUNT(*) FROM runtime_decisions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let persisted_action_events = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events WHERE kind='action_proposed'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    connection.execute_batch("ROLLBACK;").unwrap();
    flush_after_pressure(&runtime);

    assert_eq!(live_actions, 0);
    assert_eq!(persisted_trajectory_events, trajectory_events);
    assert_eq!(persisted_runtime_decisions, runtime_decisions);
    assert_eq!(persisted_action_events, action_events);
    let durable = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert!(durable.actions.is_empty());
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectory_events(&trajectory_id)
            .unwrap()
            .len(),
        usize::try_from(trajectory_events).unwrap()
    );
}

#[test]
fn sticky_writer_failure_is_propagated_by_the_next_public_lifecycle_barrier() {
    let fixture = Fixture::new();
    configure(&fixture, 2);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "sticky-barrier-source"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(
            "CREATE TRIGGER fail_async_bridge_event
             BEFORE INSERT ON bridge_events
             WHEN NEW.kind = 'agent_message'
             BEGIN
               SELECT RAISE(ABORT, 'forced sticky writer failure');
             END;",
        )
        .unwrap();

    runtime
        .bridge()
        .handle(AgentEvent::AgentMessage(AgentMessage {
            hardknock_session_id: session_id,
            summary: "force an asynchronous writer failure".into(),
        }))
        .unwrap();
    let flush_error = runtime.bridge().flush().unwrap_err();
    assert!(
        flush_error
            .to_string()
            .contains("forced sticky writer failure"),
        "{flush_error}"
    );

    let barrier_error = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "sticky-barrier-rejected"))
        .unwrap_err();
    assert!(
        barrier_error
            .to_string()
            .contains("forced sticky writer failure"),
        "{barrier_error}"
    );
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
        1
    );
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .bridge_sessions()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn action_transaction_rolls_back_every_row_when_final_event_insert_fails() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let mut runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "atomic-action-rollback"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let durable_before = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_id = durable_before.trajectory_id.clone().unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(
            "CREATE TRIGGER fail_action_bridge_event
             BEFORE INSERT ON bridge_events
             WHEN NEW.kind = 'action_proposed'
             BEGIN
               SELECT RAISE(ABORT, 'forced action event failure');
             END;",
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::ActionProposed(ActionProposed {
            hardknock_session_id: session_id.clone(),
            action_id: "atomic-action".into(),
            action: NormalizedAction::Shell {
                command: "printf bridge".into(),
                cwd: fixture.repo.display().to_string(),
            },
            context: Default::default(),
        }))
        .unwrap_err();
    assert!(
        error.to_string().contains("forced action event failure"),
        "{error}"
    );
    let flush_error = runtime.bridge().flush().unwrap_err();
    assert!(
        flush_error
            .to_string()
            .contains("forced action event failure"),
        "{flush_error}"
    );
    let inspected = runtime
        .bridge()
        .handle(AgentEvent::Inspect {
            hardknock_session_id: session_id.clone(),
        })
        .unwrap();
    assert_eq!(inspected["session"]["actions"], 0);

    let store = Store::open(&fixture.home).unwrap();
    let durable_after = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable_after.revision, durable_before.revision);
    assert!(durable_after.actions.is_empty());
    assert!(store.trajectory_events(&trajectory_id).unwrap().is_empty());
    assert!(store.runtime_decisions().unwrap().is_empty());
    let action_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events WHERE session_id=?1 AND kind='action_proposed'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(action_events, 0);

    connection
        .execute_batch("DROP TRIGGER fail_action_bridge_event;")
        .unwrap();
    let bridge = runtime.bridge.take().unwrap();
    drop(bridge);
    runtime.worker.take().unwrap().join().unwrap();
}

#[test]
fn stale_session_revision_rejects_action_without_side_rows_and_reconciles_live_state() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-action-revision"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let store = Store::open(&fixture.home).unwrap();
    let mut advanced = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_id = advanced.trajectory_id.clone().unwrap();
    advanced.revision = 100;
    let connection = database(&fixture.home);
    connection
        .execute(
            "UPDATE bridge_sessions SET revision=?2,data=?3 WHERE id=?1",
            rusqlite::params![
                session_id,
                i64::try_from(advanced.revision).unwrap(),
                serde_json::to_string(&advanced).unwrap()
            ],
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::ActionProposed(ActionProposed {
            hardknock_session_id: session_id.clone(),
            action_id: "stale-action".into(),
            action: NormalizedAction::Shell {
                command: "printf bridge".into(),
                cwd: fixture.repo.display().to_string(),
            },
            context: Default::default(),
        }))
        .unwrap_err();

    assert!(error.to_string().contains("revision changed"), "{error}");
    assert_eq!(
        runtime
            .bridge()
            .handle(AgentEvent::Inspect {
                hardknock_session_id: session_id.clone(),
            })
            .unwrap()["session"]["actions"],
        0
    );
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["status"],
        "stopping"
    );
    let store = Store::open(&fixture.home).unwrap();
    let durable = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable.revision, 100);
    assert!(durable.actions.is_empty());
    assert!(store.trajectory_events(&trajectory_id).unwrap().is_empty());
    assert!(store.runtime_decisions().unwrap().is_empty());
    let action_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events
             WHERE session_id=?1 AND kind='action_proposed'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(action_events, 0);
}

#[test]
fn stale_session_revision_rejects_resumption_without_lifecycle_side_rows_and_reconciles() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-admission-revision"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let store = Store::open(&fixture.home).unwrap();
    let mut advanced = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_count = store.trajectories().unwrap().len();
    drop(store);
    advanced.revision = 100;
    let connection = database(&fixture.home);
    connection
        .execute(
            "UPDATE bridge_sessions SET revision=?2,data=?3 WHERE id=?1",
            rusqlite::params![
                session_id,
                i64::try_from(advanced.revision).unwrap(),
                serde_json::to_string(&advanced).unwrap()
            ],
        )
        .unwrap();
    let resumed_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events
             WHERE session_id=?1 AND kind='session_resumed'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-admission-revision"))
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("session revision changed before durable commit"),
        "{error}"
    );
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectories()
            .unwrap()
            .len(),
        trajectory_count
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM bridge_events
                 WHERE session_id=?1 AND kind='session_resumed'",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        resumed_events
    );

    runtime
        .bridge()
        .handle(AgentEvent::ContextRequested(ContextRequested {
            hardknock_session_id: session_id.clone(),
            task: Some("prove admission conflict reconciled live revision".into()),
        }))
        .unwrap();
    let reconciled = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(reconciled.revision, 101);
}

#[test]
fn stale_session_revision_rejects_end_without_trajectory_or_event_side_rows() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-end-revision"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let store = Store::open(&fixture.home).unwrap();
    let mut advanced = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_id = advanced.trajectory_id.clone().unwrap();
    drop(store);
    advanced.revision = 100;
    let connection = database(&fixture.home);
    connection
        .execute(
            "UPDATE bridge_sessions SET revision=?2,data=?3 WHERE id=?1",
            rusqlite::params![
                session_id,
                i64::try_from(advanced.revision).unwrap(),
                serde_json::to_string(&advanced).unwrap()
            ],
        )
        .unwrap();
    let resolved_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM predictive_events
             WHERE subject=?1 AND kind='trajectory_resolved'",
            [trajectory_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let ended_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events
             WHERE session_id=?1 AND kind='session_ended'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("session revision changed before durable commit"),
        "{error}"
    );
    let durable = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable.revision, 100);
    assert!(!durable.ended);
    assert!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectory(&trajectory_id)
            .unwrap()
            .ended_at
            .is_none()
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM predictive_events
                 WHERE subject=?1 AND kind='trajectory_resolved'",
                [trajectory_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        resolved_events
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM bridge_events
                 WHERE session_id=?1 AND kind='session_ended'",
                [&session_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        ended_events
    );
}

#[test]
fn commit_time_trajectory_change_rolls_back_session_and_every_action_side_row() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-trajectory"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let durable_before = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_id = durable_before.trajectory_id.clone().unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER end_trajectory_during_action
             AFTER UPDATE ON bridge_sessions
             WHEN NEW.id = '{session_id}' AND NEW.revision > OLD.revision
             BEGIN
               UPDATE execution_trajectories
               SET ended_at=CURRENT_TIMESTAMP
               WHERE id='{trajectory_id}';
             END;"
        ))
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::ActionProposed(ActionProposed {
            hardknock_session_id: session_id.clone(),
            action_id: "stale-trajectory-action".into(),
            action: NormalizedAction::Shell {
                command: "printf bridge".into(),
                cwd: fixture.repo.display().to_string(),
            },
            context: Default::default(),
        }))
        .unwrap_err();

    assert!(error.to_string().contains("Trajectory changed"), "{error}");
    let store = Store::open(&fixture.home).unwrap();
    let durable_after = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable_after.revision, durable_before.revision);
    assert!(durable_after.actions.is_empty());
    assert!(store.trajectory_events(&trajectory_id).unwrap().is_empty());
    assert!(store.runtime_decisions().unwrap().is_empty());
    let ended: Option<String> = connection
        .query_row(
            "SELECT ended_at FROM execution_trajectories WHERE id=?1",
            [trajectory_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended.is_none());
}

#[test]
fn commit_time_team_authority_change_rejects_action_and_rolls_back_all_rows() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "stale-team-authority"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let role = AgentRole::builtin(BuiltInAgentRole::Executor);
    let member = AgentTeamMember {
        id: hardknock::core::TeamMemberId::new(),
        agent: CoreAgentIdentity {
            kind: "capacity-test".into(),
            executable: "bridge:capacity-test".into(),
            version: None,
            model: None,
        },
        session: HardknockSessionId::from_external(&session_id),
    };
    let now = Utc::now();
    let assignment = RoleAssignment {
        id: hardknock::core::RoleAssignmentId::new(),
        member: member.id.clone(),
        role: role.id.clone(),
        scope: Default::default(),
        valid_from: now - ChronoDuration::minutes(1),
        valid_until: now + ChronoDuration::hours(1),
    };
    let team = AgentTeam {
        id: hardknock::core::AgentTeamId::new(),
        revision: 1,
        members: vec![member.clone()],
        roles: vec![role.clone()],
        role_assignments: vec![assignment.clone()],
        authority: role.authority(),
        max_delegation_depth: 1,
        created_at: now,
    };
    let store = Store::open(&fixture.home).unwrap();
    store.save_agent_team(&team).unwrap();
    let trajectory_id = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap()
        .trajectory_id
        .unwrap();
    let mut expired = team.clone();
    expired.role_assignments[0].valid_from = now - ChronoDuration::hours(2);
    expired.role_assignments[0].valid_until = now - ChronoDuration::hours(1);
    let expired_json = serde_json::to_string(&expired).unwrap().replace('\'', "''");
    let connection = database(&fixture.home);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER expire_team_during_action
             AFTER UPDATE ON bridge_sessions
             WHEN NEW.id = '{session_id}' AND NEW.revision > OLD.revision
             BEGIN
               UPDATE agent_teams SET data='{expired_json}' WHERE id='{}';
             END;",
            team.id
        ))
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::ActionProposed(ActionProposed {
            hardknock_session_id: session_id.clone(),
            action_id: "stale-authority-action".into(),
            action: NormalizedAction::Shell {
                command: "printf bridge".into(),
                cwd: fixture.repo.display().to_string(),
            },
            context: ActionContext {
                team: Some(TeamRuntimeContext {
                    review: None,
                    team: team.id.clone(),
                    revision: team.revision,
                    member: member.id,
                    assignment: assignment.id,
                    delegation: None,
                    assessment: None,
                }),
                ..Default::default()
            },
        }))
        .unwrap_err();

    assert!(
        error.to_string().contains("expired") || error.to_string().contains("authority changed"),
        "{error}"
    );
    let store = Store::open(&fixture.home).unwrap();
    let durable = store
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert!(durable.actions.is_empty());
    assert!(store.trajectory_events(&trajectory_id).unwrap().is_empty());
    assert!(store.runtime_decisions().unwrap().is_empty());
    let team_records: i64 = connection
        .query_row("SELECT COUNT(*) FROM team_records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(team_records, 0);
    assert!(
        Store::open(&fixture.home)
            .unwrap()
            .agent_team(&team.id)
            .unwrap()
            .role_assignments[0]
            .valid_until
            > Utc::now()
    );
}

#[test]
fn session_update_and_event_failure_are_atomic_and_leave_live_candidate_unpublished() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "atomic-context-update"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let durable_before = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(
            "CREATE TRIGGER fail_context_event
             BEFORE INSERT ON bridge_events
             WHEN NEW.kind='experience_injected'
             BEGIN
               SELECT RAISE(ABORT, 'forced context event failure');
             END;",
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::ContextRequested(ContextRequested {
            hardknock_session_id: session_id.clone(),
            task: Some("candidate task must roll back".into()),
        }))
        .unwrap_err();

    assert!(
        error.to_string().contains("forced context event failure"),
        "{error}"
    );
    let durable_after = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable_after.revision, durable_before.revision);
    assert_eq!(durable_after.task, durable_before.task);
    let events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM bridge_events
             WHERE session_id=?1 AND kind='experience_injected'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 1);
}

#[test]
fn recording_queue_transition_is_atomic_and_external_work_does_not_start_on_failure() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "atomic-run-queue"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let durable_before = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(
            "CREATE TRIGGER fail_run_queue_event
             BEFORE INSERT ON bridge_events
             WHEN NEW.kind='run_queued'
             BEGIN
               SELECT RAISE(ABORT, 'forced run queue failure');
             END;",
        )
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::RunCompleted(RunCompleted {
            hardknock_session_id: session_id.clone(),
            run_id: "atomic-run".into(),
            success: Some(true),
            final_message: None,
            duration_ms: 1,
            termination: Default::default(),
            external_metadata: serde_json::Value::Null,
        }))
        .unwrap_err();

    assert!(
        error.to_string().contains("forced run queue failure"),
        "{error}"
    );
    let durable_after = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(durable_after.revision, durable_before.revision);
    assert!(durable_after.runs.is_empty());
    let run_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM bridge_runs", [], |row| row.get(0))
        .unwrap();
    let experience_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM experiences", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_rows, 0);
    assert_eq!(experience_rows, 0);
}

#[test]
fn shutdown_response_is_emitted_only_after_bridge_writer_completion() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let mut runtime = Runtime::open(&fixture.home);
    let response = runtime.bridge().handle(AgentEvent::Shutdown).unwrap();
    assert_eq!(response["shutdown_complete"], true);
    assert!(
        runtime
            .bridge()
            .wait_for_shutdown_complete(std::time::Duration::from_millis(10))
    );
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["status"],
        "stopped"
    );
    let bridge = runtime.bridge.take().unwrap();
    drop(bridge);
    runtime.worker.take().unwrap().join().unwrap();
}

#[test]
fn failed_session_start_persistence_never_consumes_live_or_durable_capacity() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let id = session_key("capacity-test", "failed-start");
    let connection = database(&fixture.home);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_bridge_start
             BEFORE INSERT ON bridge_sessions
             WHEN NEW.id = '{id}'
             BEGIN
               SELECT RAISE(ABORT, 'forced session start failure');
             END;"
        ))
        .unwrap();

    let error = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "failed-start"))
        .unwrap_err();

    assert!(error.to_string().contains("forced session start failure"));
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
        0
    );
    assert!(
        Store::open(&fixture.home)
            .unwrap()
            .bridge_sessions()
            .unwrap()
            .is_empty()
    );
    assert!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectories()
            .unwrap()
            .is_empty()
    );

    connection
        .execute_batch("DROP TRIGGER fail_bridge_start;")
        .unwrap();
    let sticky = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "failed-start"))
        .unwrap_err();
    assert!(
        sticky.to_string().contains("forced session start failure"),
        "{sticky}"
    );
    drop(runtime);

    let recovered_runtime = Runtime::open(&fixture.home);
    let recovered = recovered_runtime
        .bridge()
        .handle(start_event(&fixture.repo, "failed-start"))
        .unwrap();
    assert_eq!(recovered["hardknock_session_id"], id);
}

#[test]
fn failed_session_end_persistence_keeps_live_and_durable_capacity_active() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "failed-end"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_bridge_end
             BEFORE UPDATE OF data ON bridge_sessions
             WHEN NEW.id = '{session_id}'
              AND CAST(json_extract(NEW.data, '$.ended') AS INTEGER) = 1
             BEGIN
               SELECT RAISE(ABORT, 'forced session end failure');
             END;"
        ))
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap_err();

    assert!(error.to_string().contains("forced session end failure"));
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
        1
    );
    let durable = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert!(!durable.ended);
    let trajectory = Store::open(&fixture.home)
        .unwrap()
        .trajectory(durable.trajectory_id.as_ref().unwrap())
        .unwrap();
    assert!(trajectory.ended_at.is_none());
    let capacity_error = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "capacity-still-held"))
        .unwrap_err();
    assert!(
        capacity_error
            .to_string()
            .contains("session budget exhausted")
    );

    connection
        .execute_batch("DROP TRIGGER fail_bridge_end;")
        .unwrap();
    let sticky = runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap_err();
    assert!(
        sticky.to_string().contains("forced session end failure"),
        "{sticky}"
    );
    drop(runtime);

    let recovered_runtime = Runtime::open(&fixture.home);
    recovered_runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id,
        }))
        .unwrap();
}

#[test]
fn failed_trajectory_finish_rolls_session_end_back_to_active_state() {
    let fixture = Fixture::new();
    configure(&fixture, 1);
    let runtime = Runtime::open(&fixture.home);
    let session_id = runtime
        .bridge()
        .handle(start_event(&fixture.repo, "failed-trajectory-end"))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    runtime.bridge().flush().unwrap();
    let durable = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    let trajectory_id = durable.trajectory_id.unwrap();
    let connection = database(&fixture.home);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_trajectory_finish
             BEFORE UPDATE OF ended_at ON execution_trajectories
             WHEN NEW.id = '{trajectory_id}' AND NEW.ended_at IS NOT NULL
             BEGIN
               SELECT RAISE(ABORT, 'forced trajectory finish failure');
             END;"
        ))
        .unwrap();

    let error = runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("forced trajectory finish failure")
    );
    assert_eq!(
        runtime.bridge().handle(AgentEvent::Status).unwrap()["sessions"],
        1
    );
    let durable = Store::open(&fixture.home)
        .unwrap()
        .bridge_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert!(!durable.ended);
    assert!(
        Store::open(&fixture.home)
            .unwrap()
            .trajectory(&trajectory_id)
            .unwrap()
            .ended_at
            .is_none()
    );

    connection
        .execute_batch("DROP TRIGGER fail_trajectory_finish;")
        .unwrap();
    let sticky = runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id.clone(),
        }))
        .unwrap_err();
    assert!(
        sticky
            .to_string()
            .contains("forced trajectory finish failure"),
        "{sticky}"
    );
    drop(runtime);

    let recovered_runtime = Runtime::open(&fixture.home);
    recovered_runtime
        .bridge()
        .handle(AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session_id,
        }))
        .unwrap();
}
