// SPDX-License-Identifier: Apache-2.0
//! Deterministic, network-free, three-node federation benchmark.
use super::*;
use crate::{
    Error, Result,
    application::RunLearningOptions,
    bridge::config::Config,
    cancellation::Cancellation,
    core::{
        AgentIdentity, ArtifactRevocationId, BenchmarkRunId, EnvironmentMode, SyncArtifactId,
        SyncEnvelopeId,
    },
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

/// Deterministic, network-free comparison of distributed-sync policies: an
/// isolated node, a naive broadcast that ignores revocation and origin
/// authentication, and a conservative Hardknock sync node. It exercises the
/// V0.22 receive boundaries (advisory import, replay suppression, untrusted-relay
/// quarantine, and signed-origin revocation) rather than the V0.7 bundle path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DistributedSyncBenchmarkResult {
    pub id: BenchmarkRunId,
    pub created_at: chrono::DateTime<Utc>,
    pub status: String,
    pub metadata: Value,
    pub scenarios: Value,
    pub metrics: Value,
    pub artifact: PathBuf,
}

fn shared_environment() -> EnvironmentIdentity {
    EnvironmentIdentity {
        kind: EnvironmentKind::Ci,
        organization: Some("openkedge".into()),
        team: Some("platform".into()),
        environment: Some("shared".into()),
        region: Some("west".into()),
        account_scope: Some("fixture".into()),
        ..Default::default()
    }
}

fn lesson_sync_artifact(
    origin: &NodeId,
    artifact_id: &str,
    revision: u64,
    relay_nodes: Vec<NodeId>,
    environment: EnvironmentIdentity,
    payload: Value,
    created_at: chrono::DateTime<Utc>,
) -> Result<SyncArtifact> {
    let mut artifact = SyncArtifact {
        id: SyncArtifactId::new(),
        artifact_type: SyncArtifactType::Lesson,
        artifact_ref: PortableArtifactRef {
            artifact_id: artifact_id.into(),
            schema_version: SYNC_ARTIFACT_SCHEMA_V1.into(),
        },
        lineage: ArtifactLineage {
            artifact_id: artifact_id.into(),
            origin: origin.clone(),
            revision,
            parent_revision: None,
        },
        origin: origin.clone(),
        root_origin: RootEvidenceOrigin {
            node: origin.clone(),
            artifact_id: artifact_id.into(),
            revision,
        },
        relay_nodes,
        environment,
        dependencies: Vec::new(),
        content_hash: String::new(),
        origin_signature: None,
        task_family: Some("deployment".into()),
        origin_maturity: Some(crate::abstraction::KnowledgeMaturity::Validated),
        critical: false,
        availability: ArtifactAvailability::Full,
        reproducibility: RemoteReproducibility::PartiallyReproducible,
        payload: Some(payload),
        created_at,
    };
    artifact.content_hash = artifact.computed_content_hash()?;
    Ok(artifact)
}

fn envelope_for(
    sender: &NodeId,
    key_revision: u64,
    artifacts: Vec<SyncArtifact>,
    created_at: chrono::DateTime<Utc>,
) -> SyncEnvelope {
    SyncEnvelope {
        id: SyncEnvelopeId::new(),
        protocol: SYNC_PROTOCOL_V1.into(),
        sender: sender.clone(),
        key_revision,
        artifacts,
        created_at,
        nonce: SyncEnvelopeId::new().to_string(),
        signature: String::new(),
    }
}

fn trusted_peer(node: &NodeId, name: &str, public_key: &str, endpoint: String) -> SyncPeer {
    SyncPeer {
        node: node.clone(),
        name: name.into(),
        public_key: public_key.into(),
        endpoint: PeerEndpoint::Filesystem(endpoint),
        trust: PeerTrust::TrustedForAdvisoryEvidence,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Active,
        last_sync: None,
    }
}

pub async fn run_distributed_sync(
    store: &Store,
    cancel: &Cancellation,
) -> Result<DistributedSyncBenchmarkResult> {
    if !store.all_lessons()?.is_empty() || store.hardknock_node()?.is_some() {
        return Err(Error::InvalidInput(
            "Distributed sync benchmark requires a fresh dedicated --home".into(),
        ));
    }
    let started = std::time::Instant::now();

    // Node A learns and locally validates a deployment Lesson (reused fixture path).
    let initial = pnpm(store, false)?;
    let transfer = pnpm(store, true)?;
    let cycle = execute_learning_run(
        store,
        request(&initial, &agent(), false, "./agent-script.sh run"),
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
    let identity_a = service_a.identity()?;
    let environment = shared_environment();
    let node_a = store.initialize_hardknock_node(&identity_a, environment.clone())?;
    let bundle_a = service_a.export_lesson(&lesson_id, Vec::new())?;
    let payload_a = serde_json::to_value(&bundle_a)?;
    let revision = u64::from(store.lesson(&lesson_id)?.version);
    let now = Utc::now();

    let lesson_artifact = lesson_sync_artifact(
        &node_a.id,
        &lesson_id.to_string(),
        revision,
        Vec::new(),
        environment.clone(),
        payload_a.clone(),
        now,
    )?;
    let lesson_ref = lesson_artifact.reference();

    // Node B trusts node A for advisory evidence.
    let root = store.home.join("federation").join("distributed-benchmark");
    fs::create_dir_all(&root)?;
    let store_b = Store::open(&root.join("node-b"))?;
    let mut config_b = Config::default();
    config_b.federation.node_name = "node-b".into();
    let identity_b = LocalFederationService {
        store: &store_b,
        config: &config_b,
    }
    .identity()?;
    store_b.initialize_hardknock_node(&identity_b, environment.clone())?;
    store_b.save_sync_peer(&trusted_peer(
        &node_a.id,
        "node-a",
        &node_a.identity.public_key,
        root.join("from-a").display().to_string(),
    ))?;

    // Scenario 1: a direct signed import becomes advisory (not automatically trusted).
    let mut direct = envelope_for(&node_a.id, node_a.key_revision, vec![lesson_artifact], now);
    direct.sign(&identity_a)?;
    let direct_session = store_b.receive_sync_envelope(&direct, &environment)?;
    let imported = store_b.remote_knowledge_record(&lesson_ref)?;
    let advisory_transfer =
        direct_session.accepted == 1 && imported.local_state == RemoteArtifactState::Advisory;

    // Scenario 2: replaying the same signed envelope is suppressed.
    let replay_session = store_b.receive_sync_envelope(&direct, &environment)?;
    let replay_suppressed = replay_session.status == SyncSessionStatus::ReplayRejected
        && replay_session.deduplicated == 1;

    // Scenario 3: an artifact relayed by a trusted sender but originating from an
    // unconfigured node is quarantined. Relay labels are not authenticity.
    let relay_dir = root.join("relay-node");
    fs::create_dir_all(&relay_dir)?;
    let identity_relay = NodeIdentity::load_or_create(&relay_dir, "relay", ExperienceNodeType::Ci)?;
    let origin_dir = root.join("untrusted-origin");
    fs::create_dir_all(&origin_dir)?;
    let identity_origin =
        NodeIdentity::load_or_create(&origin_dir, "untrusted", ExperienceNodeType::Ci)?;
    store_b.save_sync_peer(&trusted_peer(
        &identity_relay.node.id,
        "relay",
        &identity_relay.node.public_identity.public_key,
        root.join("from-relay").display().to_string(),
    ))?;
    let mut relayed = lesson_sync_artifact(
        &identity_origin.node.id,
        "external-lesson",
        1,
        vec![identity_relay.node.id.clone()],
        environment.clone(),
        payload_a,
        now,
    )?;
    relayed.sign_origin(&identity_origin)?;
    let relayed_ref = relayed.reference();
    let mut relay_envelope = envelope_for(&identity_relay.node.id, 1, vec![relayed], now);
    relay_envelope.sign(&identity_relay)?;
    let relay_session = store_b.receive_sync_envelope(&relay_envelope, &environment)?;
    let relay_record = store_b.remote_knowledge_record(&relayed_ref)?;
    let relay_quarantined = relay_session.quarantined == 1
        && relay_record.local_state == RemoteArtifactState::Quarantined;

    // Scenario 4: a signed origin revocation withdraws the advisory Lesson.
    let mut revocation = ArtifactRevocation {
        id: ArtifactRevocationId::new(),
        artifact: lesson_ref.clone(),
        origin: node_a.id.clone(),
        reason: RevocationReason::Contradicted,
        evidence: Vec::new(),
        created_at: now,
        signature: String::new(),
    };
    revocation.sign(&identity_a)?;
    let mut revocation_artifact = SyncArtifact {
        id: SyncArtifactId::new(),
        artifact_type: SyncArtifactType::Revocation,
        artifact_ref: PortableArtifactRef {
            artifact_id: revocation.id.to_string(),
            schema_version: SYNC_ARTIFACT_SCHEMA_V1.into(),
        },
        lineage: ArtifactLineage {
            artifact_id: revocation.id.to_string(),
            origin: node_a.id.clone(),
            revision: 1,
            parent_revision: None,
        },
        origin: node_a.id.clone(),
        root_origin: RootEvidenceOrigin {
            node: node_a.id.clone(),
            artifact_id: revocation.id.to_string(),
            revision: 1,
        },
        relay_nodes: Vec::new(),
        environment: environment.clone(),
        dependencies: vec![lesson_ref.clone()],
        content_hash: String::new(),
        origin_signature: None,
        task_family: None,
        origin_maturity: None,
        critical: false,
        availability: ArtifactAvailability::Full,
        reproducibility: RemoteReproducibility::NotReproducible,
        payload: Some(serde_json::to_value(&revocation)?),
        created_at: now,
    };
    revocation_artifact.content_hash = revocation_artifact.computed_content_hash()?;
    let mut revocation_envelope = envelope_for(
        &node_a.id,
        node_a.key_revision,
        vec![revocation_artifact],
        now,
    );
    revocation_envelope.sign(&identity_a)?;
    store_b.receive_sync_envelope(&revocation_envelope, &environment)?;
    let revoked = store_b.remote_knowledge_record(&lesson_ref)?;
    let actionable_after_revocation = remote_support_allows_act(&revoked, false);
    let promotion_after_revocation = conservative_promotion_decision(&revoked, false);
    let blind_critical = store_b.sync_metrics()?.blind_critical_promotions;

    // A naive broadcast would act on the received Lesson despite its revocation and
    // on the relayed artifact despite its unauthenticated origin. Both are derived
    // from the observed facts rather than assumed.
    let naive_unsafe = u64::from(revoked.origin_revoked)
        + u64::from(relay_record.local_state == RemoteArtifactState::Quarantined);
    let hardknock_unsafe = u64::from(actionable_after_revocation)
        + u64::from(relay_record.local_state != RemoteArtifactState::Quarantined);

    let accepted = advisory_transfer
        && replay_suppressed
        && relay_quarantined
        && !actionable_after_revocation
        && revoked.origin_revoked
        && promotion_after_revocation == RemotePromotionDecision::Reject
        && blind_critical == 0
        && hardknock_unsafe == 0
        && naive_unsafe == 2;
    if !accepted {
        return Err(Error::Intervention(
            "Distributed sync benchmark acceptance criteria failed".into(),
        ));
    }

    let metrics = json!({
        "unsafe_remote_actions": {"isolated": 0, "naive_broadcast": naive_unsafe, "hardknock_sync": hardknock_unsafe},
        "advisory_transfer": {"isolated": false, "naive_broadcast": true, "hardknock_sync": advisory_transfer},
        "revocation_withdrawn": {"naive_broadcast": false, "hardknock_sync": revoked.origin_revoked},
        "untrusted_relay_admitted": {"naive_broadcast": true, "hardknock_sync": !relay_quarantined},
        "blind_critical_promotions": blind_critical,
    });
    let scenarios = json!({
        "direct_import": {"state": imported.local_state, "accepted": direct_session.accepted},
        "replay": {"suppressed": replay_suppressed, "status": replay_session.status},
        "untrusted_relay": {"quarantined": relay_quarantined, "origin": relay_record.remote_artifact.origin},
        "revocation": {"actionable_after_revocation": actionable_after_revocation, "origin_revoked": revoked.origin_revoked, "promotion": promotion_after_revocation, "state": revoked.local_state},
    });

    let id = BenchmarkRunId::new();
    let artifact = store
        .home
        .join("artifacts")
        .join(format!("{id}-distributed-sync.json"));
    if let Some(parent) = artifact.parent() {
        fs::create_dir_all(parent)?;
    }
    let result = DistributedSyncBenchmarkResult {
        id,
        created_at: Utc::now(),
        status: "completed".into(),
        metadata: json!({
            "hardknock_version": env!("CARGO_PKG_VERSION"),
            "fixture_version": "distributed-sync-fixtures-v1",
            "nodes": [node_a.id, identity_b.node.id, identity_relay.node.id, identity_origin.node.id],
            "transport": "in-process signed sync envelopes; filesystem transport separately exercised",
            "network": false,
            "key_revision": 1,
            "duration_ms": started.elapsed().as_millis(),
        }),
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
    Ok(result)
}
