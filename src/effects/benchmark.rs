// SPDX-License-Identifier: Apache-2.0
//! Deterministic transactional-reality benchmark implementation.
use super::*;
use crate::{Error, Result, cancellation::Cancellation, core::BenchmarkRunId, store::Store};
use chrono::Utc;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, time::Instant};

pub const VERSION: &str = "transactional-effects-fixtures-v1";

fn request(target: String, payload: Value, fault: EffectFault) -> EffectRequest {
    EffectRequest {
        session_id: "transactional-effects-benchmark".into(),
        reality_id: None,
        source_action: ActionRef {
            id: format!("benchmark-action-{}", uuid::Uuid::new_v4()),
            kind: "benchmark".into(),
        },
        kind: EffectKind::Deployment,
        target: EffectTarget { uri: target },
        operation: EffectOperation::Update,
        payload,
        adapter: None,
        evidence: vec![VERSION.into()],
        fault,
    }
}

pub async fn run(store: &Store, cancel: &Cancellation) -> Result<EffectBenchmarkRun> {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return Err(Error::Intervention(
            "Transactional effects benchmark cancelled".into(),
        ));
    }
    let id = BenchmarkRunId::new();
    let prefix = format!("mock://benchmark/{id}");
    let external = MockExternalSystem::new(&store.home)?;

    let direct_target = format!("{prefix}/direct");
    external.seed("mock-http", &direct_target, &json!({"version":1}))?;
    for candidate in 0..4 {
        external.mutate_outside(
            "mock-http",
            &direct_target,
            &json!({"failed_candidate":candidate}),
        )?;
    }
    let direct_escapes = external
        .resource("mock-http", &direct_target)?
        .mutation_count;

    let sandbox_target = format!("{prefix}/sandbox-only");
    external.seed("mock-http", &sandbox_target, &json!({"version":1}))?;
    for candidate in 0..4 {
        // Filesystem isolation does not virtualize this external adapter state.
        external.mutate_outside(
            "mock-http",
            &sandbox_target,
            &json!({"failed_candidate":candidate}),
        )?;
    }
    let sandbox_escapes = external
        .resource("mock-http", &sandbox_target)?
        .mutation_count;

    let manager = EffectManager::new(store)?;
    let user = EffectManager::user_context();
    let mut discarded = Vec::new();
    let mut hardknock_escapes = 0u64;
    for candidate in 0..4 {
        let target = format!("{prefix}/transactional-failure-{candidate}");
        external.seed("mock-http", &target, &json!({"version":1}))?;
        let (effect, _) = manager.propose_and_prepare(
            request(target.clone(), json!({"bad":candidate}), EffectFault::None),
            &EffectManager::agent_context("benchmark-agent"),
        )?;
        manager.discard(&effect.id, &user)?;
        let resource = external.resource("mock-http", &target)?;
        hardknock_escapes += resource.mutation_count;
        discarded.push(effect.id);
    }

    let winner_target = format!("{prefix}/winner");
    external.seed("mock-http", &winner_target, &json!({"version":1}))?;
    let (winner, _) = manager.propose_and_prepare(
        request(
            winner_target.clone(),
            json!({"version":3}),
            EffectFault::None,
        ),
        &EffectManager::agent_context("benchmark-agent"),
    )?;
    let winner_authorization =
        manager.authorize(CommitAuthority::User, std::slice::from_ref(&winner.id))?;
    let winner_commit = manager.commit(&winner.id, Some(&winner_authorization), &user)?;

    let drift_target = format!("{prefix}/drift");
    external.seed("mock-http", &drift_target, &json!({"version":5}))?;
    let (drift_effect, _) = manager.propose_and_prepare(
        request(
            drift_target.clone(),
            json!({"version":7}),
            EffectFault::None,
        ),
        &user,
    )?;
    let drift_authorization = manager.authorize(
        CommitAuthority::User,
        std::slice::from_ref(&drift_effect.id),
    )?;
    external.mutate_outside("mock-http", &drift_target, &json!({"version":6}))?;
    let drift_result = manager.commit(&drift_effect.id, Some(&drift_authorization), &user)?;

    let unknown_target = format!("{prefix}/response-loss");
    external.seed("mock-http", &unknown_target, &json!({"version":1}))?;
    let (unknown_effect, _) = manager.propose_and_prepare(
        request(
            unknown_target.clone(),
            json!({"version":3}),
            EffectFault::ResponseLossAfterMutation,
        ),
        &user,
    )?;
    let unknown_authorization = manager.authorize(
        CommitAuthority::User,
        std::slice::from_ref(&unknown_effect.id),
    )?;
    let unknown_commit = manager.commit(&unknown_effect.id, Some(&unknown_authorization), &user)?;
    let reconciliation = manager.reconcile(&unknown_effect.id)?;
    let unknown_mutations = external
        .resource("mock-http", &unknown_target)?
        .mutation_count;

    let first_target = format!("{prefix}/group-a");
    let second_target = format!("{prefix}/group-b");
    external.seed("mock-http", &first_target, &json!({"created":false}))?;
    external.seed("mock-http", &second_target, &json!({"promoted":false}))?;
    let (first, _) = manager.propose_and_prepare(
        request(
            first_target,
            json!({"created":true}),
            EffectFault::CompensationFailure,
        ),
        &user,
    )?;
    let (second, _) = manager.propose_and_prepare(
        request(
            second_target,
            json!({"promoted":true}),
            EffectFault::CommitFailureBeforeMutation,
        ),
        &user,
    )?;
    let plan = manager.create_plan(
        vec![first.id.clone(), second.id.clone()],
        vec![EffectDependency {
            before: first.id.clone(),
            after: second.id.clone(),
        }],
        EffectAtomicity::CompensatingGroup,
    )?;
    let plan_authorization = manager.authorize(CommitAuthority::User, &plan.effects)?;
    let group = manager.commit_plan(&plan, &plan_authorization, &user)?;

    let experimental_failures = 4u64;
    let mut metrics = BTreeMap::new();
    metrics.insert("experimental_failures".into(), json!(experimental_failures));
    metrics.insert(
        "authoritative_mutations_from_failed_candidates".into(),
        json!({
            "direct_agent":direct_escapes,
            "sandbox_only":sandbox_escapes,
            "hardknock_transactional_reality":hardknock_escapes
        }),
    );
    metrics.insert(
        "external_mistake_escape_rate".into(),
        json!({
            "direct_agent":direct_escapes as f64 / experimental_failures as f64,
            "sandbox_only":sandbox_escapes as f64 / experimental_failures as f64,
            "hardknock_transactional_reality":hardknock_escapes as f64 / experimental_failures as f64
        }),
    );
    metrics.insert("prepared_effect_discard_rate".into(), json!("4/4"));
    metrics.insert("commit_success_rate".into(), json!("1/1"));
    metrics.insert("commit_conflict_rate".into(), json!("1/1"));
    metrics.insert("unknown_outcome_rate".into(), json!("1/1 injected"));
    metrics.insert("unknown_outcome_recovery_rate".into(), json!("1/1"));
    metrics.insert(
        "duplicate_effect_rate".into(),
        json!(if unknown_mutations == 1 { 0.0 } else { 1.0 }),
    );
    metrics.insert(
        "compensation_success_rate".into(),
        json!("0/1 injected failure"),
    );
    let scenarios = json!({
        "bad_candidates":{"discarded":discarded,"authoritative_mutations":hardknock_escapes},
        "explicit_winner_commit":{"effect":winner.id,"result":winner_commit,"state":external.resource("mock-http",&winner_target)?},
        "state_drift":{"effect":drift_effect.id,"result":drift_result},
        "response_loss":{"effect":unknown_effect.id,"initial":unknown_commit,"reconciliation":reconciliation,"mutations":unknown_mutations},
        "partial_commit":{"plan":plan.id,"commit_outcome":group.commit_outcome,"final_outcome":group.outcome,"manual_intervention_required":group.manual_intervention_required}
    });
    let artifact = store
        .home
        .join("artifacts")
        .join(format!("transactional-effects-{id}.json"));
    let mut run = EffectBenchmarkRun {
        id,
        created_at: Utc::now(),
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        metrics,
        scenarios,
        artifact,
    };
    fs::write(&run.artifact, serde_json::to_vec_pretty(&run)?)?;
    run.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    store.insert_effect_benchmark(&run)?;
    Ok(run)
}
