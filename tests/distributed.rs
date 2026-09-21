// SPDX-License-Identifier: Apache-2.0

use chrono::Utc;
use hardknock::{
    abstraction::KnowledgeMaturity,
    core::*,
    epistemic::{
        Claim, ClaimKind, EpistemicDependencySet, EvidenceContext, EvidenceOutcome, EvidencePath,
        EvidenceRef, EvidenceSource,
    },
    federation::*,
    lesson::ContextSelector,
    store::{EpistemicStore, Store},
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
use tempfile::TempDir;

struct TestHardknockNode {
    _home: TempDir,
    store: Store,
    identity: NodeIdentity,
    environment: EnvironmentIdentity,
}

struct TestNodeCluster {
    nodes: Vec<TestHardknockNode>,
}

impl TestNodeCluster {
    fn new(kinds: &[EnvironmentKind]) -> Self {
        let nodes = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let home = tempfile::tempdir().unwrap();
                let store = Store::open(home.path()).unwrap();
                let identity = NodeIdentity::load_or_create(
                    home.path(),
                    &format!("test-node-{index}"),
                    ExperienceNodeType::Ci,
                )
                .unwrap();
                let environment = environment(kind.clone(), "kubernetes", "1.30");
                store
                    .initialize_hardknock_node(&identity, environment.clone())
                    .unwrap();
                TestHardknockNode {
                    _home: home,
                    store,
                    identity,
                    environment,
                }
            })
            .collect();
        Self { nodes }
    }

    fn trust(&self, receiver: usize, sender: usize, endpoint: PeerEndpoint) {
        let source = &self.nodes[sender].identity.node;
        self.nodes[receiver]
            .store
            .save_sync_peer(&SyncPeer {
                node: source.id.clone(),
                name: source.name.clone(),
                public_key: source.public_identity.public_key.clone(),
                endpoint,
                trust: PeerTrust::TrustedForAdvisoryEvidence,
                trust_policy: PeerTrustPolicy::default(),
                filters: SyncFilter::default(),
                status: PeerStatus::Active,
                last_sync: None,
            })
            .unwrap();
    }
}

fn environment(kind: EnvironmentKind, runtime: &str, version: &str) -> EnvironmentIdentity {
    EnvironmentIdentity {
        kind,
        organization: Some("openkedge".into()),
        team: Some("platform".into()),
        environment: Some("test".into()),
        region: Some("west".into()),
        account_scope: Some("fixture".into()),
        tags: BTreeMap::from([
            ("runtime".into(), runtime.into()),
            ("version".into(), version.into()),
        ]),
    }
}

fn artifact(
    node: &TestHardknockNode,
    kind: SyncArtifactType,
    critical: bool,
    availability: ArtifactAvailability,
) -> SyncArtifact {
    let object = format!("fixture-{}", SyncArtifactId::new());
    let mut artifact = SyncArtifact {
        id: SyncArtifactId::new(),
        artifact_type: kind,
        artifact_ref: PortableArtifactRef {
            artifact_id: object.clone(),
            schema_version: SYNC_ARTIFACT_SCHEMA_V1.into(),
        },
        lineage: ArtifactLineage {
            artifact_id: object.clone(),
            origin: node.identity.node.id.clone(),
            revision: 1,
            parent_revision: None,
        },
        origin: node.identity.node.id.clone(),
        root_origin: RootEvidenceOrigin {
            node: node.identity.node.id.clone(),
            artifact_id: object,
            revision: 1,
        },
        relay_nodes: Vec::new(),
        environment: node.environment.clone(),
        dependencies: Vec::new(),
        content_hash: String::new(),
        task_family: Some("deployment".into()),
        origin_maturity: Some(KnowledgeMaturity::Validated),
        critical,
        availability,
        reproducibility: if availability == ArtifactAvailability::Full {
            RemoteReproducibility::FullyReproducible
        } else {
            RemoteReproducibility::PartiallyReproducible
        },
        payload: Some(json!({"claim":"reconcile authoritative state before retry"})),
        created_at: Utc::now(),
    };
    artifact.content_hash = artifact.computed_content_hash().unwrap();
    artifact
}

fn envelope(signer: &TestHardknockNode, artifacts: Vec<SyncArtifact>) -> SyncEnvelope {
    let mut envelope = SyncEnvelope {
        id: SyncEnvelopeId::new(),
        protocol: SYNC_PROTOCOL_V1.into(),
        sender: signer.identity.node.id.clone(),
        key_revision: 1,
        artifacts,
        created_at: Utc::now(),
        nonce: SyncEnvelopeId::new().to_string(),
        signature: String::new(),
    };
    envelope.sign(&signer.identity).unwrap();
    envelope
}

fn receive_one(
    cluster: &TestNodeCluster,
    receiver: usize,
    sender: usize,
    artifact: SyncArtifact,
) -> RemoteKnowledgeRecord {
    cluster.trust(
        receiver,
        sender,
        PeerEndpoint::Filesystem("/tmp/unused".into()),
    );
    cluster.nodes[receiver]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[sender], vec![artifact]),
            &cluster.nodes[receiver].environment,
        )
        .unwrap();
    cluster.nodes[receiver]
        .store
        .remote_knowledge_records()
        .unwrap()
        .remove(0)
}

#[test]
fn basic_signed_sync_is_advisory_and_never_local_validation() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        ),
    );
    assert_eq!(record.origin_status, Some(KnowledgeMaturity::Validated));
    assert_eq!(record.local_state, RemoteArtifactState::Advisory);
    assert!(record.local_evidence.is_empty());
}

#[test]
fn signature_and_artifact_tampering_are_rejected() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let mut signed = envelope(
        &cluster.nodes[0],
        vec![artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        )],
    );
    signed.artifacts[0].payload = Some(json!({"claim":"tampered"}));
    assert!(
        cluster.nodes[1]
            .store
            .receive_sync_envelope(&signed, &cluster.nodes[1].environment)
            .is_err()
    );
    assert!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn duplicate_envelope_is_replay_rejected_without_duplicate_artifact() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let signed = envelope(
        &cluster.nodes[0],
        vec![artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        )],
    );
    let first = cluster.nodes[1]
        .store
        .receive_sync_envelope(&signed, &cluster.nodes[1].environment)
        .unwrap();
    let second = cluster.nodes[1]
        .store
        .receive_sync_envelope(&signed, &cluster.nodes[1].environment)
        .unwrap();
    assert_eq!(first.accepted, 1);
    assert_eq!(second.status, SyncSessionStatus::ReplayRejected);
    assert_eq!(cluster.nodes[1].store.sync_artifacts().unwrap().len(), 1);
}

#[test]
fn relay_echo_deduplicates_to_one_root_origin() {
    let cluster = TestNodeCluster::new(&[
        EnvironmentKind::Ci,
        EnvironmentKind::Staging,
        EnvironmentKind::Integration,
        EnvironmentKind::Developer,
    ]);
    cluster.trust(3, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    cluster.trust(3, 2, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let original = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    cluster.nodes[3]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[0], vec![original.clone()]),
            &cluster.nodes[3].environment,
        )
        .unwrap();
    let mut relayed = original;
    relayed.id = SyncArtifactId::new();
    relayed.relay_nodes = vec![
        cluster.nodes[1].identity.node.id.clone(),
        cluster.nodes[2].identity.node.id.clone(),
    ];
    let result = cluster.nodes[3]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[2], vec![relayed]),
            &cluster.nodes[3].environment,
        )
        .unwrap();
    assert_eq!(result.deduplicated, 1);
    assert_eq!(cluster.nodes[3].store.sync_artifacts().unwrap().len(), 1);
}

#[test]
fn unverified_relay_origin_is_quarantined() {
    let cluster = TestNodeCluster::new(&[
        EnvironmentKind::Ci,
        EnvironmentKind::Staging,
        EnvironmentKind::Ci,
    ]);
    cluster.trust(2, 1, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let mut relayed = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    relayed
        .relay_nodes
        .push(cluster.nodes[1].identity.node.id.clone());
    let session = cluster.nodes[2]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[1], vec![relayed]),
            &cluster.nodes[2].environment,
        )
        .unwrap();
    assert_eq!(session.quarantined, 1);
    assert!(
        cluster.nodes[2]
            .store
            .advisory_remote_knowledge()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn peer_key_must_identify_declared_node() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let claimed = &cluster.nodes[0].identity.node;
    let other = &cluster.nodes[1].identity.node;
    let result = cluster.nodes[1].store.save_sync_peer(&SyncPeer {
        node: claimed.id.clone(),
        name: "spoofed".into(),
        public_key: other.public_identity.public_key.clone(),
        endpoint: PeerEndpoint::Filesystem("/tmp/unused".into()),
        trust: PeerTrust::Known,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Active,
        last_sync: None,
    });
    assert!(result.is_err());
}

#[test]
fn independent_reproduction_adds_a_real_root_while_team_echo_does_not() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Staging]);
    let a = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    let b = artifact(
        &cluster.nodes[1],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    let echo = DistributedEvidencePath {
        evidence: None,
        origin_node: cluster.nodes[0].identity.node.id.clone(),
        origin_environment: cluster.nodes[0].environment.clone(),
        root_origin: a.root_origin.clone(),
        relay_nodes: vec![cluster.nodes[1].identity.node.id.clone()],
        local_reproduction: None,
    };
    let original = DistributedEvidencePath {
        relay_nodes: Vec::new(),
        ..echo.clone()
    };
    let reproduced = DistributedEvidencePath {
        evidence: Some(EvidencePathId::new()),
        origin_node: cluster.nodes[1].identity.node.id.clone(),
        origin_environment: cluster.nodes[1].environment.clone(),
        root_origin: b.root_origin,
        relay_nodes: Vec::new(),
        local_reproduction: Some(EvidencePathId::new()),
    };
    assert_eq!(distinct_root_origins([&original, &echo]).len(), 1);
    assert_eq!(
        distinct_root_origins([&original, &echo, &reproduced]).len(),
        2
    );
}

#[test]
fn remote_constraint_requires_reproduction_and_cannot_authorize_critical_act() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Constraint,
            true,
            ArtifactAvailability::Full,
        ),
    );
    assert_eq!(
        record.local_state,
        RemoteArtifactState::ReproductionRequired
    );
    assert_eq!(
        conservative_promotion_decision(&record, true),
        RemotePromotionDecision::ReproduceLocally
    );
    assert!(!remote_support_allows_act(&record, true));
}

#[test]
fn critical_constraint_refuses_unverified_local_support() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Constraint,
            true,
            ArtifactAvailability::Full,
        ),
    );
    let path = local_support_path(&cluster.nodes[1].store, &record.remote_artifact);
    assert!(
        cluster.nodes[1]
            .store
            .support_remote_artifact(&record.remote_artifact, vec![path])
            .is_err()
    );
    assert_eq!(
        cluster.nodes[1]
            .store
            .sync_metrics()
            .unwrap()
            .blind_critical_promotions,
        0
    );
}

#[test]
fn remote_exception_cannot_relax_fresh_local_constraint_without_local_exception_evidence() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::KnowledgeException,
            true,
            ArtifactAvailability::Full,
        ),
    );
    assert!(!remote_exception_can_relax_local_constraint(&record, &[]));
}

#[test]
fn environment_match_and_mismatch_are_qualitative_and_never_auto_promote() {
    let exact = environment(EnvironmentKind::Ci, "kubernetes", "1.30");
    let different = environment(EnvironmentKind::Production, "nomad", "1.9");
    assert_eq!(
        assess_environment_compatibility(&exact, &exact).status,
        EnvironmentCompatibilityStatus::Compatible
    );
    assert!(matches!(
        assess_environment_compatibility(&exact, &different).status,
        EnvironmentCompatibilityStatus::PartiallyCompatible
            | EnvironmentCompatibilityStatus::Incompatible
    ));
}

#[test]
fn remote_contradiction_creates_revalidation_signal_without_deleting_target() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        ),
    );
    let contradiction = RemoteContradiction {
        target: record.remote_artifact.clone(),
        evidence: vec![EvidencePathId::new()],
        context: cluster.nodes[1].environment.clone(),
        origin: cluster.nodes[1].identity.node.id.clone(),
        received_at: Utc::now(),
    };
    cluster.nodes[1]
        .store
        .save_remote_contradiction(&contradiction)
        .unwrap();
    assert_eq!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn revocation_removes_unreproduced_advice_but_preserves_independent_local_support() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let first = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        ),
    );
    let mut revocation = ArtifactRevocation {
        id: ArtifactRevocationId::new(),
        artifact: first.remote_artifact.clone(),
        origin: cluster.nodes[0].identity.node.id.clone(),
        reason: RevocationReason::CorruptEvidence,
        evidence: Vec::new(),
        created_at: Utc::now(),
        signature: String::new(),
    };
    revocation.sign(&cluster.nodes[0].identity).unwrap();
    let revoked = cluster.nodes[1]
        .store
        .apply_artifact_revocation(&revocation)
        .unwrap();
    assert_eq!(revoked.local_state, RemoteArtifactState::Revoked);
    assert!(
        cluster.nodes[1]
            .store
            .advisory_remote_knowledge()
            .unwrap()
            .is_empty()
    );

    let second_artifact = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Recovery,
        false,
        ArtifactAvailability::Full,
    );
    let second = receive_one_fresh(&cluster, second_artifact);
    let path = local_support_path(&cluster.nodes[1].store, &second.remote_artifact);
    cluster.nodes[1]
        .store
        .support_remote_artifact(&second.remote_artifact, vec![path])
        .unwrap();
    let mut second_revocation = ArtifactRevocation {
        id: ArtifactRevocationId::new(),
        artifact: second.remote_artifact,
        origin: cluster.nodes[0].identity.node.id.clone(),
        reason: RevocationReason::Superseded,
        evidence: Vec::new(),
        created_at: Utc::now(),
        signature: String::new(),
    };
    second_revocation.sign(&cluster.nodes[0].identity).unwrap();
    let preserved = cluster.nodes[1]
        .store
        .apply_artifact_revocation(&second_revocation)
        .unwrap();
    assert_eq!(preserved.local_state, RemoteArtifactState::LocallySupported);
    assert!(preserved.origin_revoked);
    assert!(
        cluster.nodes[1]
            .store
            .advisory_remote_knowledge()
            .unwrap()
            .is_empty()
    );
    assert!(!preserved.local_evidence.is_empty());
    assert!(preserved.review_required);
}

fn receive_one_fresh(cluster: &TestNodeCluster, artifact: SyncArtifact) -> RemoteKnowledgeRecord {
    cluster.nodes[1]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[0], vec![artifact]),
            &cluster.nodes[1].environment,
        )
        .unwrap();
    cluster.nodes[1]
        .store
        .remote_knowledge_records()
        .unwrap()
        .pop()
        .unwrap()
}

fn local_support_path(store: &Store, remote: &SyncArtifactRef) -> EvidencePathId {
    let claim = Claim {
        id: ClaimId::new(),
        kind: ClaimKind::LessonClaim,
        statement: format!("Local check for {}", remote.content_hash),
        scope: ContextSelector {
            repository: None,
            required_markers: Vec::new(),
            tags: Vec::new(),
            os: None,
            arch: None,
        },
        created_at: Utc::now(),
    };
    store.insert_claim(&claim).unwrap();
    store
        .insert_evidence_path(&EvidencePath {
            id: EvidencePathId::new(),
            claim: claim.id.into(),
            source: EvidenceSource::StaticCheck {
                evaluator: "local-fixture-check".into(),
            },
            context: EvidenceContext::default(),
            dependencies: EpistemicDependencySet::default(),
            evidence_refs: vec![EvidenceRef {
                kind: "remote_sync_artifact".into(),
                id: remote.content_hash.clone(),
            }],
            outcome: EvidenceOutcome::Supports,
            created_at: Utc::now(),
        })
        .unwrap()
        .id
}

#[test]
fn remote_certification_preserves_origin_certified_local_uncertified_split() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Certification,
            false,
            ArtifactAvailability::Full,
        ),
    );
    assert!(record.origin_certified);
    assert!(!record.locally_certified);
}

#[test]
fn offline_transport_failure_does_not_break_local_store() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let peer = SyncPeer {
        node: cluster.nodes[0].identity.node.id.clone(),
        name: "offline".into(),
        public_key: cluster.nodes[0]
            .identity
            .node
            .public_identity
            .public_key
            .clone(),
        endpoint: PeerEndpoint::Http("http://offline.invalid".into()),
        trust: PeerTrust::Known,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Offline,
        last_sync: None,
    };
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();
    assert!(transport.pull(&peer, None).is_err());
    assert!(cluster.nodes[0].store.hardknock_node().unwrap().is_some());
}

#[test]
fn filesystem_sync_resumes_after_cursor_without_full_rescan() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = SyncPeer {
        node: cluster.nodes[0].identity.node.id.clone(),
        name: "repository".into(),
        public_key: cluster.nodes[0]
            .identity
            .node
            .public_identity
            .public_key
            .clone(),
        endpoint: PeerEndpoint::Filesystem(repository.path().display().to_string()),
        trust: PeerTrust::Known,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Active,
        last_sync: None,
    };
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();
    let first = envelope(&cluster.nodes[0], vec![]);
    let first_receipt = transport.push(&peer, &first).unwrap();
    let second = envelope(&cluster.nodes[0], vec![]);
    transport.push(&peer, &second).unwrap();
    let position = Path::new(&first_receipt.location)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let cursor = SyncCursor {
        peer: peer.node.clone(),
        stream: SyncStreamKind::Knowledge,
        position,
        updated_at: Utc::now(),
    };
    let resumed = transport.pull(&peer, Some(&cursor)).unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].id, second.id);
}

#[test]
fn redacted_evidence_retains_limited_availability_and_reproducibility() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Redacted,
        ),
    );
    assert_eq!(record.availability, ArtifactAvailability::Redacted);
    assert_eq!(
        record.reproducibility,
        RemoteReproducibility::PartiallyReproducible
    );
}

#[test]
fn derivative_keeps_new_owner_and_remote_dependency() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Staging]);
    let source = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    let mut derivative = artifact(
        &cluster.nodes[1],
        SyncArtifactType::AbstractKnowledge,
        false,
        ArtifactAvailability::Full,
    );
    derivative.dependencies = vec![source.reference()];
    derivative.content_hash = derivative.computed_content_hash().unwrap();
    assert_eq!(derivative.origin, cluster.nodes[1].identity.node.id);
    assert_eq!(
        derivative.dependencies[0].origin,
        cluster.nodes[0].identity.node.id
    );
    derivative.verify_hash().unwrap();
}

#[test]
fn compromised_origin_requires_review_without_erasing_local_reproduction() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    let record = receive_one(
        &cluster,
        1,
        0,
        artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        ),
    );
    let path = local_support_path(&cluster.nodes[1].store, &record.remote_artifact);
    cluster.nodes[1]
        .store
        .support_remote_artifact(&record.remote_artifact, vec![path])
        .unwrap();
    assert_eq!(
        cluster.nodes[1]
            .store
            .mark_sync_origin_compromised(&cluster.nodes[0].identity.node.id)
            .unwrap(),
        1
    );
    let record = cluster.nodes[1]
        .store
        .remote_knowledge_record(&record.remote_artifact)
        .unwrap();
    assert!(record.review_required);
    assert_eq!(record.local_state, RemoteArtifactState::LocallySupported);
    assert!(!record.local_evidence.is_empty());
    assert!(
        cluster.nodes[1]
            .store
            .receive_sync_envelope(
                &envelope(&cluster.nodes[0], vec![]),
                &cluster.nodes[1].environment,
            )
            .is_err()
    );
}

#[test]
fn plan_revalidation_only_follows_material_irreversible_remote_contradiction() {
    let plan = ExecutionPlanId::new();
    assert_eq!(
        remote_contradiction_plan_impact(Some(&plan), true, true),
        RemotePlanImpact::VerificationRequired
    );
    assert_eq!(
        remote_contradiction_plan_impact(Some(&plan), false, true),
        RemotePlanImpact::NoChange
    );
}

#[test]
fn economics_selects_only_the_relevant_critical_artifact_from_large_batch() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let mut artifacts = (0..99)
        .map(|_| {
            let mut item = artifact(
                &cluster.nodes[0],
                SyncArtifactType::Lesson,
                false,
                ArtifactAvailability::Full,
            );
            item.task_family = Some("unused-language".into());
            item.content_hash = item.computed_content_hash().unwrap();
            item
        })
        .collect::<Vec<_>>();
    artifacts.push(artifact(
        &cluster.nodes[0],
        SyncArtifactType::Recovery,
        true,
        ArtifactAvailability::Full,
    ));
    let selected = prioritize_remote_artifacts(&artifacts, "deployment", true);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].artifact_type, SyncArtifactType::Recovery);
}

#[test]
fn low_relevance_sync_batch_does_not_create_work_or_plan_thrash() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let artifacts = (0..50)
        .map(|_| {
            let mut item = artifact(
                &cluster.nodes[0],
                SyncArtifactType::Lesson,
                false,
                ArtifactAvailability::Full,
            );
            item.task_family = Some("irrelevant".into());
            item.content_hash = item.computed_content_hash().unwrap();
            item
        })
        .collect();
    cluster.nodes[1]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[0], artifacts),
            &cluster.nodes[1].environment,
        )
        .unwrap();
    assert!(
        cluster.nodes[1]
            .store
            .reproduction_queue()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        remote_contradiction_plan_impact(Some(&ExecutionPlanId::new()), false, true),
        RemotePlanImpact::NoChange
    );
}

#[test]
fn secret_bearing_payload_is_refused_before_signing() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let mut unsafe_artifact = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    unsafe_artifact.payload = Some(json!({"aws_secret_access_key":"fake-secret"}));
    unsafe_artifact.content_hash = unsafe_artifact.computed_content_hash().unwrap();
    let mut unsigned = SyncEnvelope {
        id: SyncEnvelopeId::new(),
        protocol: SYNC_PROTOCOL_V1.into(),
        sender: cluster.nodes[0].identity.node.id.clone(),
        key_revision: 1,
        artifacts: vec![unsafe_artifact],
        created_at: Utc::now(),
        nonce: "secret-test".into(),
        signature: String::new(),
    };
    assert!(unsigned.sign(&cluster.nodes[0].identity).is_err());
}
