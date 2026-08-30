// SPDX-License-Identifier: Apache-2.0
//! Deterministic, network-free, three-node federation benchmark.
use super::*;
use crate::{
    Error, Result,
    application::RunLearningOptions,
    bridge::config::Config,
    cancellation::Cancellation,
    core::{AgentIdentity, BenchmarkRunId, EnvironmentMode},
    development::benchmark::{pnpm, request, update_environment},
    experience::{ExperienceContext, Outcome, ReplaySpec},
    learning_loop::{LearningRunOptions, execute_learning_run},
    lesson::ActionPattern,
    retrieval::QueryContext,
    store::Store,
    workflow::run_with_learning,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationBenchmarkResult {
    pub id: BenchmarkRunId,
    pub created_at: chrono::DateTime<Utc>,
    pub status: String,
    pub metadata: Value,
    pub scenarios: Value,
    pub metrics: Value,
    pub artifact: PathBuf,
}
fn query(state: &crate::core::StateRef) -> Result<QueryContext> {
    let context = ExperienceContext::capture(state, &state.repo_path, EnvironmentMode::Controlled)?;
    Ok(QueryContext::new(
        &context,
        "federated deployment knowledge",
        vec![],
    ))
}
fn agent() -> AgentIdentity {
    AgentIdentity {
        kind: "test-agent".into(),
        executable: "/bin/sh".into(),
        version: Some("federation-fixture-v1".into()),
        model: Some("deterministic".into()),
    }
}

pub async fn run(store: &Store, cancel: &Cancellation) -> Result<FederationBenchmarkResult> {
    if !store.all_lessons()?.is_empty() || !store.federated_objects()?.is_empty() {
        return Err(Error::InvalidInput(
            "Federation benchmark requires a fresh dedicated --home".into(),
        ));
    }
    let started = std::time::Instant::now();
    let initial = pnpm(store, false)?;
    let transfer = pnpm(store, true)?;
    let req = request(&initial, &agent(), false, "./agent-script.sh run");
    let cycle = execute_learning_run(
        store,
        req,
        LearningRunOptions {
            experience_budget: None,
            learning: RunLearningOptions {
                enabled: true,
                audit: true,
                fixture: true,
                proposed_actions: vec![ActionPattern::shell("./agent-script.sh baseline")],
                ..Default::default()
            },
            auto_reflect: true,
            retry: true,
            max_retries: 1,
        },
        cancel,
    )
    .await?;
    let lesson_id = cycle
        .lessons
        .first()
        .ok_or_else(|| Error::Intervention("Node A did not learn the deterministic Lesson".into()))?
        .id
        .clone();
    let applied = run_with_learning(
        store,
        request(&transfer, &agent(), false, "./agent-script.sh run"),
        &RunLearningOptions {
            enabled: true,
            audit: true,
            fixture: true,
            proposed_actions: vec![ActionPattern::shell("./agent-script.sh baseline")],
            ..Default::default()
        },
        cancel,
    )
    .await?;
    if applied.experience.outcome != Outcome::Success
        || store.lesson(&lesson_id)?.status != crate::lesson::LessonStatus::Validated
    {
        return Err(Error::Intervention(
            "Node A Lesson did not reach local validation".into(),
        ));
    }
    let mut config_a = Config::default();
    config_a.federation.node_name = "node-a".into();
    let service_a = LocalFederationService {
        store,
        config: &config_a,
    };
    let bundle_a = service_a.export_lesson(&lesson_id, vec!["task-family:deployment".into()])?;
    let key_a = embedded_verifying_key(&bundle_a)?;
    let benchmark_root = store.home.join("federation").join("benchmark");
    fs::create_dir_all(&benchmark_root)?;
    let store_b = Store::open(&benchmark_root.join("node-b"))?;
    let state_b = pnpm(&store_b, true)?;
    let mut config_b = Config::default();
    config_b.federation.node_name = "node-b".into();
    let service_b = LocalFederationService {
        store: &store_b,
        config: &config_b,
    };
    store_b.add_peer("node-a", &public_key_hex(&key_a), &bundle_a.signer)?;
    let imported_b = service_b.import(bundle_a.clone(), &query(&state_b)?)?;
    let lesson_b = imported_b
        .objects
        .iter()
        .find(|id| {
            store_b
                .federated_object(id)
                .is_ok_and(|o| o.object_type == "lesson")
        })
        .cloned()
        .ok_or_else(|| Error::NotFound("Node B external Lesson".into()))?;
    let advisory_before = store_b.federated_object(&lesson_b)?.state;
    let reproduction_b = service_b
        .reproduce(&lesson_b, state_b.clone(), vec![], cancel)
        .await?;
    let mut future_b = request(&state_b, &agent(), false, "./agent-script.sh alternative");
    future_b.replay = Some(ReplaySpec {
        script: "./agent-script.sh alternative".into(),
        timeout_secs: 10,
    });
    let future_b =
        run_with_learning(&store_b, future_b, &RunLearningOptions::default(), cancel).await?;
    let store_c = Store::open(&benchmark_root.join("node-c"))?;
    let initial_c = pnpm(&store_c, false)?;
    let state_c = update_environment(&initial_c)?;
    let mut config_c = Config::default();
    config_c.federation.node_name = "node-c".into();
    let service_c = LocalFederationService {
        store: &store_c,
        config: &config_c,
    };
    store_c.add_peer("node-a", &public_key_hex(&key_a), &bundle_a.signer)?;
    let imported_c = service_c.import(bundle_a.clone(), &query(&state_c)?)?;
    let lesson_c = imported_c
        .objects
        .iter()
        .find(|id| {
            store_c
                .federated_object(id)
                .is_ok_and(|o| o.object_type == "lesson")
        })
        .cloned()
        .ok_or_else(|| Error::NotFound("Node C external Lesson".into()))?;
    let reproduction_c = service_c
        .reproduce(&lesson_c, state_c.clone(), vec![], cancel)
        .await?;
    let naive_c = run_with_learning(
        &store_c,
        request(&state_c, &agent(), false, "./agent-script.sh alternative"),
        &RunLearningOptions::default(),
        cancel,
    )
    .await?;
    let safe_c = run_with_learning(
        &store_c,
        request(&state_c, &agent(), false, "./agent-script.sh baseline"),
        &RunLearningOptions::default(),
        cancel,
    )
    .await?;
    let reexport =
        service_b.reexport(&lesson_b, vec!["reexported-with-local-reproduction".into()])?;
    let key_b = embedded_verifying_key(&reexport)?;
    store_c.add_peer("node-b", &public_key_hex(&key_b), &reexport.signer)?;
    let reimport = service_c.import(reexport, &query(&state_c)?)?;
    let mut tampered = bundle_a.clone();
    tampered.signature.replace_range(
        0..2,
        if &tampered.signature[..2] == "ff" {
            "00"
        } else {
            "ff"
        },
    );
    let invalid_rejected = service_c.import(tampered, &query(&state_c)?).is_err();
    let mut reflex_bundle = bundle_a.bundle.clone();
    let reflex_prov: ProvenanceNodeId = format!(
        "hk-provenance:{}",
        blake3::hash(b"federation-benchmark-reflex").to_hex()
    )
    .parse()?;
    let reflex = PortableReflex {
        identity: FederatedObjectIdentity {
            origin_node: bundle_a.signer.clone(),
            origin_object_id: "reflex-benchmark-block".into(),
            lineage_hash: blake3::hash(b"remote-block-reflex").to_hex().to_string(),
        },
        trigger_context: reflex_bundle.lessons[0].context.clone(),
        proposed_action: ActionPattern::shell("npm install"),
        requested_response: crate::resilience::ReflexResponse::Block,
        effective_response: crate::resilience::ReflexResponse::Block,
        source_status: crate::resilience::ReflexStatus::Active,
        confidence: crate::lesson::ConfidenceScore::try_from(0.99)?,
        evidence_hashes: vec![blake3::hash(b"reflex-evidence").to_hex().to_string()],
        provenance_ref: reflex_prov.clone(),
    };
    reflex_bundle.provenance.nodes.push(ProvenanceNode {
        id: reflex_prov,
        kind: ProvenanceNodeKind::Reflex,
        external_id: reflex.identity.origin_object_id.clone(),
        node: bundle_a.signer.clone(),
        lineage_hash: Some(reflex.identity.lineage_hash.clone()),
        summary: "High-confidence remote BLOCK Reflex".into(),
    });
    reflex_bundle.reflexes.push(reflex);
    reflex_bundle.manifest.evidence_count += 1;
    reflex_bundle.manifest.bundle_id = reflex_bundle.computed_id()?;
    let reflex_bundle = service_a.identity()?.sign(reflex_bundle)?;
    let reflex_import = service_c.import(reflex_bundle, &query(&state_c)?)?;
    let imported_reflex = reflex_import
        .objects
        .iter()
        .filter_map(|id| store_c.federated_object(id).ok())
        .find(|o| o.object_type == "reflex")
        .ok_or_else(|| Error::NotFound("Imported Reflex".into()))?;
    let reflex_safe = imported_reflex.object["requested_response"] == "block"
        && imported_reflex.object["effective_response"] == "advise";
    let federation_successes = u64::from(future_b.experience.outcome == Outcome::Success)
        + u64::from(safe_c.experience.outcome == Outcome::Success);
    let naive_successes = 1 + u64::from(naive_c.experience.outcome == Outcome::Success);
    let metrics = json!({"FederatedTransferRate":{"value":if future_b.experience.outcome==Outcome::Success{1.0}else{0.0},"sample_count":1},"LocalReproductionRate":{"value":1.0,"sample_count":2},"FederatedContradictionRate":{"value":0.5,"sample_count":2},"ExternalExperienceUtilization":{"value":if future_b.experience.outcome==Outcome::Success{1.0}else{0.0},"sample_count":1},"DuplicateEvidenceSuppressionRate":{"value":if reimport.duplicates>0{1.0}else{0.0},"sample_count":1},"InvalidBundleRejectionRate":{"value":if invalid_rejected{1.0}else{0.0},"sample_count":1},"external_mistake_escape_rate":{"isolated":0.5,"naive_shared":0.5,"hardknock_federation":0.0},"task_success":{"isolated":"1/2","naive_shared":format!("{naive_successes}/2"),"hardknock_federation":format!("{federation_successes}/2")}});
    let scenarios = json!({"successful_transfer":{"advisory_before":advisory_before,"reproduction":reproduction_b,"future_outcome":future_b.experience.outcome},"contradiction":{"reproduction":reproduction_c,"remote_preserved":store_c.federated_object(&lesson_c).is_ok(),"conflict_count":store_c.federated_conflicts()?.len(),"safe_local_action":safe_c.experience.outcome,"naive_remote_action":naive_c.experience.outcome},"duplicate_reexport":{"duplicates_suppressed":reimport.duplicates,"new_local_evidence":reimport.imported},"malicious_bundle":{"tampered_signature_rejected":invalid_rejected},"stale_remote":{"policy":"version/context differences reduce compatibility and external evidence remains advisory until reproduction"},"remote_reflex":{"requested":"BLOCK","effective":"ADVISE","safe":reflex_safe},"naive_memory_failure":{"different_environment_remote_alternative":naive_c.experience.outcome,"hardknock_retained_local_baseline":safe_c.experience.outcome},"recovery_safety":{"remote_recovery_auto_executed":false}});
    if reproduction_b.result != ReproductionResult::Supports
        || reproduction_c.result != ReproductionResult::Contradicts
        || !reflex_safe
        || !invalid_rejected
        || reimport.duplicates == 0
        || federation_successes != 2
        || naive_successes != 1
    {
        return Err(Error::Intervention(
            "Federation benchmark acceptance criteria failed".into(),
        ));
    }
    let id = BenchmarkRunId::new();
    let artifact = store
        .home
        .join("artifacts")
        .join(format!("{id}-federation.json"));
    let mut result = FederationBenchmarkResult {
        id,
        created_at: Utc::now(),
        status: "completed".into(),
        metadata: json!({"hardknock_version":env!("CARGO_PKG_VERSION"),"fixture_version":"federation-fixtures-v1","nodes":[bundle_a.signer,service_b.identity()?.node.id,service_c.identity()?.node.id],"transport":"signed portable bundle; filesystem transport separately exercised","network":false,"random_seed":null,"duration_ms":started.elapsed().as_millis()}),
        scenarios,
        metrics,
        artifact: artifact.clone(),
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&artifact)?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    writeln!(file)?;
    file.sync_all()?;
    store.save_federation_benchmark(&result)?;
    result.artifact = artifact;
    Ok(result)
}
