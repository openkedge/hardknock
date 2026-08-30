// SPDX-License-Identifier: Apache-2.0
mod support;

use hardknock::{
    bridge::{
        Bridge,
        config::Config,
        protocol::{AgentEvent, AgentIdentity, EffectProposal, SessionStarted},
    },
    budget::ExperienceBudget,
    cancellation::Cancellation,
    core::{CandidateId, ExperimentRequestId},
    dojo::capture_state,
    effects::*,
    experimentation::{
        CandidateExecution, ComparisonCriteria, ExperimentCandidate, ExperimentCapabilities,
        ExperimentIntent, ExperimentOrchestrator, ExperimentOrigin, ExperimentRequest,
        ExperimentStartingState, ExperimentStatus, SnapshotSource,
    },
    store::{EffectStore, Store},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use support::Fixture;
use tempfile::TempDir;

fn setup() -> (TempDir, Store, MockExternalSystem) {
    let home = TempDir::new().unwrap();
    let store = Store::open(home.path()).unwrap();
    let external = MockExternalSystem::new(home.path()).unwrap();
    (home, store, external)
}

fn request(target: &str, payload: Value) -> EffectRequest {
    EffectRequest {
        session_id: "test-session".into(),
        reality_id: None,
        source_action: ActionRef {
            id: format!("action-{target}"),
            kind: "test".into(),
        },
        kind: EffectKind::HttpApi,
        target: EffectTarget { uri: target.into() },
        operation: EffectOperation::Update,
        payload,
        adapter: None,
        evidence: vec!["experiment-test".into()],
        fault: EffectFault::None,
    }
}

fn prepare(manager: &EffectManager<'_>, request: EffectRequest) -> Effect {
    let (effect, prepared) = manager
        .propose_and_prepare(request, &EffectManager::user_context())
        .unwrap();
    assert_eq!(prepared.effect_id, effect.id);
    assert_eq!(effect.lifecycle, EffectLifecycle::Prepared);
    effect
}

#[test]
fn two_transactional_realities_do_not_mutate_authoritative_state_and_discard_cleanly() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/service-a";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let first = prepare(&manager, request(target, json!({"version":2})));
    let second = prepare(&manager, request(target, json!({"version":3})));
    let state = external.resource("mock-http", target).unwrap();
    assert_eq!(state.state, json!({"version":1}));
    assert_eq!(state.mutation_count, 0);
    assert_eq!(external.prepared_count("mock-http", target).unwrap(), 2);

    manager
        .discard(&first.id, &EffectManager::user_context())
        .unwrap();
    manager
        .discard(&second.id, &EffectManager::user_context())
        .unwrap();
    let state = external.resource("mock-http", target).unwrap();
    assert_eq!(state.state, json!({"version":1}));
    assert_eq!(state.mutation_count, 0);
    assert_eq!(external.prepared_count("mock-http", target).unwrap(), 0);
}

#[test]
fn explicit_authorized_commit_changes_state_once_and_persists_receipt_and_events() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/service-a";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let effect = prepare(&manager, request(target, json!({"version":3})));
    assert_eq!(external.resource("mock-http", target).unwrap().version, 1);
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    let outcome = manager
        .commit(
            &effect.id,
            Some(&authorization),
            &EffectManager::user_context(),
        )
        .unwrap();
    let CommitOutcome::Committed { receipt } = outcome else {
        panic!("expected commit")
    };
    assert_eq!(receipt.effect_id, effect.id);
    let state = external.resource("mock-http", target).unwrap();
    assert_eq!(state.state, json!({"version":3}));
    assert_eq!(state.mutation_count, 1);
    assert!(
        store
            .commit_receipt_for_effect(&effect.id)
            .unwrap()
            .is_some()
    );
    let commit_experiences = store
        .effect_experience_links(&effect.id, Some("commit"))
        .unwrap();
    assert_eq!(commit_experiences.len(), 1);
    assert_eq!(
        store.experience(&commit_experiences[0]).unwrap().outcome,
        hardknock::experience::Outcome::Success
    );
    let events = store.effect_events(&effect.id).unwrap();
    assert_eq!(
        events.first().unwrap().event_type,
        EffectEventType::Proposed
    );
    assert_eq!(
        events.last().unwrap().event_type,
        EffectEventType::Committed
    );
}

#[test]
fn stale_prepared_effect_is_rejected_without_overwriting_external_drift() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/service-a";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let effect = prepare(&manager, request(target, json!({"version":3})));
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    external
        .mutate_outside("mock-http", target, &json!({"version":2}))
        .unwrap();
    let outcome = manager
        .commit(
            &effect.id,
            Some(&authorization),
            &EffectManager::user_context(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        CommitOutcome::Rejected {
            reprepare: true,
            ..
        }
    ));
    assert_eq!(
        external.resource("mock-http", target).unwrap().state,
        json!({"version":2})
    );
    assert_eq!(
        store.effect(&effect.id).unwrap().lifecycle,
        EffectLifecycle::Prepared
    );
}

#[test]
fn mock_database_uses_optimistic_version_checks_and_deterministic_invariants() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock-db://inventory/widget";
    external
        .seed("mock-db", target, &json!({"quantity":10,"balance":10}))
        .unwrap();
    let mut database = request(target, json!({"quantity":9,"balance":9}));
    database.kind = EffectKind::Database;
    let effect = prepare(&manager, database);
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    external
        .mutate_outside("mock-db", target, &json!({"quantity":8,"balance":8}))
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Rejected {
            reprepare: true,
            ..
        }
    ));
    let mut invalid = request("mock-db://inventory/invalid", json!({"balance":-1}));
    invalid.kind = EffectKind::Database;
    assert!(
        manager
            .propose(invalid, &EffectManager::user_context())
            .unwrap_err()
            .to_string()
            .contains("negative balance")
    );
}

#[test]
fn prepare_discard_and_expiration_failures_preserve_exact_safe_state() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let prepare_target = "mock://fault/prepare";
    external
        .seed("mock-http", prepare_target, &json!({"value":1}))
        .unwrap();
    let mut prepare_request = request(prepare_target, json!({"value":2}));
    prepare_request.fault = EffectFault::PrepareFailure;
    let proposed = manager
        .propose(prepare_request, &EffectManager::user_context())
        .unwrap();
    assert!(
        manager
            .prepare(&proposed.id, &EffectManager::user_context())
            .is_err()
    );
    assert_eq!(
        store.effect(&proposed.id).unwrap().lifecycle,
        EffectLifecycle::Failed
    );

    let discard_target = "mock://fault/discard";
    external
        .seed("mock-http", discard_target, &json!({"value":1}))
        .unwrap();
    let mut discard_request = request(discard_target, json!({"value":2}));
    discard_request.fault = EffectFault::DiscardFailure;
    let discard_effect = prepare(&manager, discard_request);
    assert!(
        manager
            .discard(&discard_effect.id, &EffectManager::user_context())
            .is_err()
    );
    assert_eq!(
        store.effect(&discard_effect.id).unwrap().lifecycle,
        EffectLifecycle::Prepared
    );
    assert_eq!(
        store
            .effect_events(&discard_effect.id)
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        EffectEventType::CleanupFailed
    );

    let expiry_target = "mock://fault/expiry";
    external
        .seed("mock-http", expiry_target, &json!({"value":1}))
        .unwrap();
    let mut expiry_request = request(expiry_target, json!({"value":2}));
    expiry_request.fault = EffectFault::ReservationExpiry;
    let expiry_effect = prepare(&manager, expiry_request);
    let authorization = manager
        .authorize(
            CommitAuthority::User,
            std::slice::from_ref(&expiry_effect.id),
        )
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &expiry_effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Rejected {
            reprepare: true,
            ..
        }
    ));
    for target in [prepare_target, discard_target, expiry_target] {
        assert_eq!(
            external
                .resource("mock-http", target)
                .unwrap()
                .mutation_count,
            0
        );
    }
}

#[test]
fn expired_authorization_and_failed_reconciliation_fail_closed() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let authorization_target = "mock://fault/authorization-expiry";
    external
        .seed("mock-http", authorization_target, &json!({"value":1}))
        .unwrap();
    let effect = prepare(&manager, request(authorization_target, json!({"value":2})));
    let mut authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    authorization.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
    assert!(
        manager
            .commit(
                &effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    assert_eq!(
        external
            .resource("mock-http", authorization_target)
            .unwrap()
            .mutation_count,
        0
    );

    let reconciliation_target = "mock://fault/reconciliation";
    external
        .seed("mock-http", reconciliation_target, &json!({"value":1}))
        .unwrap();
    let mut reconciliation_request = request(reconciliation_target, json!({"value":2}));
    reconciliation_request.fault = EffectFault::ResponseLossWithReconciliationFailure;
    let unknown = prepare(&manager, reconciliation_request);
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&unknown.id))
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &unknown.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Unknown { .. }
    ));
    assert!(matches!(
        manager.reconcile(&unknown.id).unwrap(),
        ReconciliationResult::StillUnknown { .. }
    ));
    assert_eq!(
        store.effect(&unknown.id).unwrap().lifecycle,
        EffectLifecycle::Unknown
    );
    assert_eq!(
        external
            .resource("mock-http", reconciliation_target)
            .unwrap()
            .mutation_count,
        1
    );
}

#[test]
fn commit_authorization_is_bound_to_exact_payload_and_target_scope() {
    let (home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/service-a";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let effect = prepare(&manager, request(target, json!({"version":2})));
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    let mut tampered = store.effect(&effect.id).unwrap();
    tampered.payload = json!({"version":3});
    Connection::open(home.path().join("hardknock.db"))
        .unwrap()
        .execute(
            "UPDATE effects SET data=?2 WHERE id=?1",
            rusqlite::params![
                effect.id.to_string(),
                serde_json::to_string(&tampered).unwrap()
            ],
        )
        .unwrap();
    let error = manager
        .commit(
            &effect.id,
            Some(&authorization),
            &EffectManager::user_context(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("current effect scope"));
    assert_eq!(
        external.resource("mock-http", target).unwrap().state,
        json!({"version":1})
    );
}

#[test]
fn response_loss_enters_unknown_then_reconciliation_recovers_real_receipt() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/service-a";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let mut effect_request = request(target, json!({"version":3}));
    effect_request.fault = EffectFault::ResponseLossAfterMutation;
    let effect = prepare(&manager, effect_request);
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Unknown { .. }
    ));
    assert_eq!(
        store.effect(&effect.id).unwrap().lifecycle,
        EffectLifecycle::Unknown
    );
    assert_eq!(
        external
            .resource("mock-http", target)
            .unwrap()
            .mutation_count,
        1
    );
    assert!(matches!(
        manager.reconcile(&effect.id).unwrap(),
        ReconciliationResult::Committed { .. }
    ));
    assert_eq!(
        store.effect(&effect.id).unwrap().lifecycle,
        EffectLifecycle::Committed
    );
    assert_eq!(
        external
            .resource("mock-http", target)
            .unwrap()
            .mutation_count,
        1
    );
}

#[test]
fn idempotent_retry_after_lost_response_does_not_duplicate_mutation() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock://deployment/retry";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let mut effect_request = request(target, json!({"version":2}));
    effect_request.fault = EffectFault::ResponseLossAfterMutation;
    let effect = prepare(&manager, effect_request);
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    assert!(matches!(
        manager
            .commit(
                &effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Unknown { .. }
    ));
    assert!(matches!(
        manager
            .commit(
                &effect.id,
                Some(&authorization),
                &EffectManager::user_context()
            )
            .unwrap(),
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(
        external
            .resource("mock-http", target)
            .unwrap()
            .mutation_count,
        1
    );
}

#[test]
fn partial_group_commit_and_failed_compensation_remain_explicit() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let mut first_request = request("mock://plan/a", json!({"created":true}));
    first_request.fault = EffectFault::CompensationFailure;
    let first = prepare(&manager, first_request);
    let second = prepare(
        &manager,
        request("mock://plan/b", json!({"configured":true})),
    );
    let mut third_request = request("mock://plan/c", json!({"promoted":true}));
    third_request.fault = EffectFault::CommitFailureBeforeMutation;
    let third = prepare(&manager, third_request);
    let plan = manager
        .create_plan(
            vec![first.id.clone(), second.id.clone(), third.id.clone()],
            vec![
                EffectDependency {
                    before: first.id.clone(),
                    after: second.id.clone(),
                },
                EffectDependency {
                    before: second.id.clone(),
                    after: third.id.clone(),
                },
            ],
            EffectAtomicity::CompensatingGroup,
        )
        .unwrap();
    let authorization = manager
        .authorize(CommitAuthority::User, &plan.effects)
        .unwrap();
    let result = manager
        .commit_plan(&plan, &authorization, &EffectManager::user_context())
        .unwrap();
    assert_eq!(
        result.commit_outcome,
        CommitGroupOutcome::PartiallyCommitted
    );
    assert_eq!(result.outcome, CommitGroupOutcome::PartiallyCompensated);
    assert_eq!(result.committed.len(), 2);
    assert_eq!(result.failed_effect, Some(third.id));
    assert!(result.manual_intervention_required);
    assert!(
        result
            .compensation
            .iter()
            .any(|receipt| receipt.status == CompensationStatus::Failed)
    );
    assert_eq!(
        external
            .resource("mock-http", "mock://plan/c")
            .unwrap()
            .mutation_count,
        0
    );
}

#[test]
fn proposing_agent_can_prepare_but_cannot_self_authorize_commit() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "mock-message://recipient@example.test";
    external
        .seed("mock-message", target, &json!({"delivered":[]}))
        .unwrap();
    let agent = EffectManager::agent_context("test-agent");
    let mut message = request(target, json!({"subject":"hello"}));
    message.kind = EffectKind::Message;
    message.operation = EffectOperation::Dispatch;
    let (effect, _) = manager.propose_and_prepare(message, &agent).unwrap();
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&effect.id))
        .unwrap();
    let error = manager
        .commit(&effect.id, Some(&authorization), &agent)
        .unwrap_err();
    assert!(error.to_string().contains("self-authorize"));
    assert_eq!(
        external.resource("mock-message", target).unwrap().state,
        json!({"delivered":[]})
    );
}

#[test]
fn passing_experiment_does_not_imply_commit_and_shadow_discard_removes_stage() {
    let (_home, store, external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let target = "shadow://service-a";
    external
        .seed("shadow-deployment", target, &json!({"active":"v1"}))
        .unwrap();
    let mut shadow = request(target, json!({"active":"v3"}));
    shadow.kind = EffectKind::Deployment;
    shadow.operation = EffectOperation::Promote;
    let effect = prepare(&manager, shadow);
    // This stands for an evaluator PASS; only an explicit commit path can mutate authority.
    assert_eq!(
        external
            .resource("shadow-deployment", target)
            .unwrap()
            .state,
        json!({"active":"v1"})
    );
    manager
        .discard(&effect.id, &EffectManager::user_context())
        .unwrap();
    assert_eq!(
        external
            .prepared_count("shadow-deployment", target)
            .unwrap(),
        0
    );
    assert_eq!(
        external
            .resource("shadow-deployment", target)
            .unwrap()
            .mutation_count,
        0
    );
}

#[test]
fn append_only_effect_events_refuse_update_and_delete() {
    let (home, store, _external) = setup();
    let manager = EffectManager::new(&store).unwrap();
    let effect = prepare(&manager, request("mock://immutable", json!({"value":1})));
    let connection = Connection::open(home.path().join("hardknock.db")).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE effect_events SET event_type='forged' WHERE effect_id=?1",
                [effect.id.to_string()]
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM effect_events WHERE effect_id=?1",
                [effect.id.to_string()]
            )
            .is_err()
    );
}

#[test]
fn cli_keeps_prepare_and_commit_separate_and_exposes_capabilities() {
    let fixture = Fixture::new();
    let target = "mock://deployment/cli";
    let seeded = fixture.cli(
        &[
            "effect",
            "fixture-set",
            "--adapter",
            "mock-http",
            "--target",
            target,
            "--state",
            r#"{"version":1}"#,
        ],
        0,
    );
    assert_eq!(seeded["result"]["resource"]["version"], 1);
    let prepared = fixture.cli(
        &[
            "effect",
            "propose",
            "--kind",
            "http-api",
            "--operation",
            "update",
            "--target",
            target,
            "--payload",
            r#"{"version":3}"#,
            "--prepare",
        ],
        0,
    );
    assert_eq!(prepared["result"]["committed"], false);
    assert_eq!(prepared["result"]["effect"]["lifecycle"], "prepared");
    let unchanged = fixture.cli(
        &[
            "effect",
            "fixture-show",
            "--adapter",
            "mock-http",
            "--target",
            target,
        ],
        0,
    );
    assert_eq!(unchanged["result"]["resource"]["state"]["version"], 1);
    let id = prepared["result"]["effect"]["id"].as_str().unwrap();
    let committed = fixture.cli(&["effect", "commit", id, "--yes"], 0);
    assert_eq!(committed["result"]["result"]["outcome"], "committed");
    let changed = fixture.cli(
        &[
            "effect",
            "fixture-show",
            "--adapter",
            "mock-http",
            "--target",
            target,
        ],
        0,
    );
    assert_eq!(changed["result"]["resource"]["state"]["version"], 3);
    let capabilities = fixture.cli(&["effect", "capabilities"], 0);
    assert!(capabilities["result"]["adapters"]["mock-http"]["prepare"] == true);
}

#[test]
fn reality_discard_cleans_attached_prepared_effect_before_worktree_removal() {
    let fixture = Fixture::new();
    let target = "shadow://reality-cleanup";
    fixture.cli(
        &[
            "effect",
            "fixture-set",
            "--adapter",
            "shadow-deployment",
            "--target",
            target,
            "--state",
            r#"{"active":"v1"}"#,
        ],
        0,
    );
    let reality = fixture.cli(&["reality", "create"], 0);
    let reality_id = reality["reality"]["id"].as_str().unwrap();
    let prepared = fixture.cli(
        &[
            "effect",
            "propose",
            "--reality",
            reality_id,
            "--kind",
            "deployment",
            "--operation",
            "promote",
            "--target",
            target,
            "--payload",
            r#"{"active":"v2"}"#,
            "--prepare",
        ],
        0,
    );
    let effect_id = prepared["result"]["effect"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = fixture.cli(&["reality", "show", reality_id], 0);
    assert_eq!(shown["effects"]["prepared"], 1);
    fixture.cli(&["reality", "discard", reality_id], 0);
    let effect = fixture.cli(&["effect", "show", &effect_id], 0);
    assert_eq!(effect["result"]["effect"]["lifecycle"], "discarded");
    let external = MockExternalSystem::new(&fixture.home).unwrap();
    assert_eq!(
        external
            .resource("shadow-deployment", target)
            .unwrap()
            .state,
        json!({"active":"v1"})
    );
    assert_eq!(
        external
            .prepared_count("shadow-deployment", target)
            .unwrap(),
        0
    );
}

#[test]
fn bridge_agent_tool_can_prepare_and_inspect_but_commit_requires_external_authority() {
    let fixture = Fixture::new();
    let external = MockExternalSystem::new(&fixture.home).unwrap();
    let target = "mock://deployment/bridge";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let (bridge, worker) = Bridge::open(&fixture.home).unwrap();
    let session = bridge
        .handle(AgentEvent::SessionStarted(SessionStarted {
            session_id: "external-effect-session".into(),
            agent: AgentIdentity::new("fixture-agent"),
            cwd: fixture.repo.to_string_lossy().into(),
            repository: None,
            task: Some("prepare a governed deployment".into()),
            environment: Default::default(),
        }))
        .unwrap()["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut effect_request = request(target, json!({"version":3}));
    effect_request.session_id = session.clone();
    let prepared = bridge
        .handle(AgentEvent::EffectProposed(EffectProposal {
            hardknock_session_id: session.clone(),
            request: effect_request,
        }))
        .unwrap();
    assert_eq!(prepared["status"], "prepared");
    assert_eq!(prepared["committed"], false);
    let effect_id: hardknock::core::EffectId =
        prepared["effect_id"].as_str().unwrap().parse().unwrap();
    let commit = bridge
        .handle(AgentEvent::EffectCommitRequested {
            hardknock_session_id: session.clone(),
            effect_id: effect_id.clone(),
        })
        .unwrap();
    assert_eq!(commit["status"], "authorization_required");
    assert_eq!(
        external.resource("mock-http", target).unwrap().state,
        json!({"version":1})
    );
    let status = bridge
        .handle(AgentEvent::EffectStatus {
            hardknock_session_id: session.clone(),
            effect_id: effect_id.clone(),
        })
        .unwrap();
    assert_eq!(status["effect"]["lifecycle"], "prepared");
    bridge
        .handle(AgentEvent::EffectDiscardRequested {
            hardknock_session_id: session,
            effect_id,
        })
        .unwrap();
    bridge.flush().unwrap();
    drop(bridge);
    worker.join().unwrap();
}

#[tokio::test]
async fn effect_plan_experiment_discards_loser_and_leaves_only_winner_prepared() {
    let fixture = Fixture::from_fixture("strategy-choice");
    let store = Store::open(&fixture.home).unwrap();
    let external = MockExternalSystem::new(&fixture.home).unwrap();
    let target = "mock://deployment/experiment";
    external
        .seed("mock-http", target, &json!({"version":1}))
        .unwrap();
    let effect_request = |version| EffectRequest {
        session_id: "effect-experiment".into(),
        reality_id: None,
        source_action: ActionRef {
            id: format!("candidate-v{version}"),
            kind: "effect-plan".into(),
        },
        kind: EffectKind::Deployment,
        target: EffectTarget { uri: target.into() },
        operation: EffectOperation::Update,
        payload: json!({"version":version}),
        adapter: None,
        evidence: Vec::new(),
        fault: EffectFault::None,
    };
    let request = ExperimentRequest {
        id: ExperimentRequestId::new(),
        session_id: "effect-experiment".into(),
        question: "Which prepared deployment strategy passes compatibility checks?".into(),
        hypothesis: None,
        candidates: vec![
            ExperimentCandidate {
                id: CandidateId::new(),
                name: "direct".into(),
                description: "incomplete direct update".into(),
                execution: CandidateExecution::EffectPlan {
                    effects: vec![effect_request(2)],
                    simulation: vec!["./agent-script.sh direct-upgrade".into()],
                },
                expected_outcome: None,
            },
            ExperimentCandidate {
                id: CandidateId::new(),
                name: "shadow".into(),
                description: "compatible staged update".into(),
                execution: CandidateExecution::EffectPlan {
                    effects: vec![effect_request(3)],
                    simulation: vec!["./agent-script.sh staged-upgrade".into()],
                },
                expected_outcome: None,
            },
        ],
        starting_state: ExperimentStartingState {
            state_ref: capture_state(&fixture.repo).unwrap(),
            expected_fingerprint: None,
            parent_reality: None,
            source: SnapshotSource::RepositoryCommit,
        },
        evaluator: hardknock::evaluation::EvaluationSpec {
            checks: vec!["./test.sh".into()],
        },
        budget: ExperienceBudget::default(),
        requested_by: hardknock::core::AgentIdentity {
            kind: "effect-plan-test".into(),
            executable: "hardknock".into(),
            version: None,
            model: None,
        },
        created_at: chrono::Utc::now(),
        criteria: ComparisonCriteria::default(),
        origin: ExperimentOrigin::User,
        intent: ExperimentIntent::CompareStrategies,
        capabilities: ExperimentCapabilities::default(),
    };
    let experiment = ExperimentOrchestrator {
        store: &store,
        config: &Config::default(),
    }
    .run(request, &Cancellation::default())
    .await
    .unwrap();
    assert_eq!(experiment.status, ExperimentStatus::Completed);
    let result = experiment.result.unwrap();
    let direct = result
        .candidates
        .iter()
        .find(|candidate| candidate.name == "direct")
        .unwrap();
    let shadow = result
        .candidates
        .iter()
        .find(|candidate| candidate.name == "shadow")
        .unwrap();
    assert!(!direct.evaluation.success);
    assert!(shadow.evaluation.success);
    assert_eq!(direct.prepared_effects.len(), 1);
    assert_eq!(shadow.prepared_effects.len(), 1);
    assert_eq!(
        store.effect(&direct.prepared_effects[0]).unwrap().lifecycle,
        EffectLifecycle::Discarded
    );
    let winner = store.effect(&shadow.prepared_effects[0]).unwrap();
    assert_eq!(winner.lifecycle, EffectLifecycle::Prepared);
    assert!(winner.reality_id.is_none());
    assert_eq!(
        external.resource("mock-http", target).unwrap().state,
        json!({"version":1})
    );
    let manager = EffectManager::new(&store).unwrap();
    let authorization = manager
        .authorize(CommitAuthority::User, std::slice::from_ref(&winner.id))
        .unwrap();
    manager
        .commit(
            &winner.id,
            Some(&authorization),
            &EffectManager::user_context(),
        )
        .unwrap();
    let commit_experience = store
        .experience(
            &store
                .effect_experience_links(&winner.id, Some("commit"))
                .unwrap()[0],
        )
        .unwrap();
    assert!(commit_experience.relations.iter().any(|relation| {
        matches!(
            relation,
            hardknock::application::ExperienceRelation::CommitOf(id)
                if id == &shadow.experience_id
        )
    }));
    assert_eq!(
        external.resource("mock-http", target).unwrap().state,
        json!({"version":3})
    );
}

#[tokio::test]
async fn transactional_effect_benchmark_reports_zero_supported_escape_and_real_recovery() {
    let (_home, store, _external) = setup();
    let result = hardknock::effects::benchmark::run(&store, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        result.metrics["authoritative_mutations_from_failed_candidates"]["hardknock_transactional_reality"],
        0
    );
    assert_eq!(
        result.metrics["external_mistake_escape_rate"]["hardknock_transactional_reality"],
        0.0
    );
    assert_eq!(result.metrics["unknown_outcome_recovery_rate"], "1/1");
    assert_eq!(
        result.scenarios["partial_commit"]["commit_outcome"],
        "partially_committed"
    );
    assert_eq!(
        result.scenarios["partial_commit"]["final_outcome"],
        "partially_compensated"
    );
    assert!(result.artifact.is_file());
    assert!(store.latest_effect_benchmark().unwrap().is_some());
}
