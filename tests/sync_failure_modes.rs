// SPDX-License-Identifier: Apache-2.0
use chrono::{DateTime, Utc};
use hardknock::{abstraction::KnowledgeMaturity, core::*, federation::*, store::Store};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::symlink,
    path::Path,
};
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
        origin_signature: None,
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

/// Build the same `SyncPeer` that `trust()` builds, run `mutate`, then persist it on the
/// receiver's store. Used to exercise per-peer trust-policy and filter behaviour.
fn configure_peer(
    cluster: &TestNodeCluster,
    receiver: usize,
    sender: usize,
    mutate: impl FnOnce(&mut SyncPeer),
) {
    let source = &cluster.nodes[sender].identity.node;
    let mut peer = SyncPeer {
        node: source.id.clone(),
        name: source.name.clone(),
        public_key: source.public_identity.public_key.clone(),
        endpoint: PeerEndpoint::Filesystem("/tmp/unused".into()),
        trust: PeerTrust::TrustedForAdvisoryEvidence,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Active,
        last_sync: None,
    };
    mutate(&mut peer);
    cluster.nodes[receiver].store.save_sync_peer(&peer).unwrap();
}

/// Mirror of the private `signed_revocation_artifact` helper in tests/distributed.rs: a signed
/// `ArtifactRevocation` over `target`, wrapped in a Revocation-type sync artifact.
fn signed_revocation_control(node: &TestHardknockNode, target: &SyncArtifact) -> SyncArtifact {
    let mut revocation = ArtifactRevocation {
        id: ArtifactRevocationId::new(),
        artifact: target.reference(),
        origin: node.identity.node.id.clone(),
        reason: RevocationReason::CorruptEvidence,
        evidence: Vec::new(),
        created_at: Utc::now(),
        signature: String::new(),
    };
    revocation.sign(&node.identity).unwrap();
    let id = revocation.id.to_string();
    let mut control = artifact(
        node,
        SyncArtifactType::Revocation,
        false,
        ArtifactAvailability::Full,
    );
    control.artifact_ref.artifact_id = id.clone();
    control.lineage.artifact_id = id.clone();
    control.root_origin.artifact_id = id;
    control.dependencies = vec![target.reference()];
    control.payload = Some(serde_json::to_value(revocation).unwrap());
    control.content_hash = control.computed_content_hash().unwrap();
    control
}

/// A peer that only names a filesystem repository; it is never saved to the store because the
/// transport layer reads only `peer.endpoint` (and `peer.node` for the pull sender filter).
fn transport_peer(node: &TestHardknockNode, repo: &Path) -> SyncPeer {
    SyncPeer {
        node: node.identity.node.id.clone(),
        name: node.identity.node.name.clone(),
        public_key: node.identity.node.public_identity.public_key.clone(),
        endpoint: PeerEndpoint::Filesystem(repo.display().to_string()),
        trust: PeerTrust::Known,
        trust_policy: PeerTrustPolicy::default(),
        filters: SyncFilter::default(),
        status: PeerStatus::Active,
        last_sync: None,
    }
}

// ---------------------------------------------------------------------------
// Transport-level failure modes (FilesystemSyncTransport).
// ---------------------------------------------------------------------------

#[test]
fn new_rejects_out_of_range_byte_limits() {
    assert!(FilesystemSyncTransport::new(1023).is_err());
    assert!(FilesystemSyncTransport::new(0).is_err());
    assert!(FilesystemSyncTransport::new(1024).is_ok());
    assert!(FilesystemSyncTransport::new(1024 * 1024 * 1024).is_ok());
    assert!(FilesystemSyncTransport::new(1024 * 1024 * 1024 + 1).is_err());
}

#[test]
fn push_is_idempotent_for_identical_content_and_rejects_divergent_content() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = transport_peer(&cluster.nodes[0], repository.path());
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();

    let a = envelope(&cluster.nodes[0], vec![]);
    let mut b = a.clone();
    b.nonce = "divergent".into();
    b.sign(&cluster.nodes[0].identity).unwrap();

    let r1 = transport.push(&peer, &a).unwrap();
    let r2 = transport.push(&peer, &a).unwrap();
    assert_eq!(r1.location, r2.location);
    assert_eq!(r2.bytes, r1.bytes);
    // `b` reuses `a`'s id and created_at (identical filename) but has different bytes.
    assert!(transport.push(&peer, &b).is_err());
    assert_eq!(
        fs::read_dir(repository.path().join("envelopes"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn push_rejects_oversized_envelope() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = transport_peer(&cluster.nodes[0], repository.path());
    let transport = FilesystemSyncTransport::new(1024).unwrap();

    let mut art = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    art.payload = Some(json!({ "filler": "a".repeat(4096) }));
    art.content_hash = art.computed_content_hash().unwrap();
    let env = envelope(&cluster.nodes[0], vec![art]);

    assert!(transport.push(&peer, &env).is_err());
    assert_eq!(
        fs::read_dir(repository.path().join("envelopes"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn pull_rejects_oversized_on_disk_envelope() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = transport_peer(&cluster.nodes[0], repository.path());

    let mut art = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    art.payload = Some(json!({ "filler": "a".repeat(4096) }));
    art.content_hash = art.computed_content_hash().unwrap();
    let env = envelope(&cluster.nodes[0], vec![art]);

    let big = FilesystemSyncTransport::new(1024 * 1024).unwrap();
    big.push(&peer, &env).unwrap();

    let small = FilesystemSyncTransport::new(1024).unwrap();
    assert!(small.pull(&peer, None).is_err());
    assert_eq!(big.pull(&peer, None).unwrap().len(), 1);
}

#[test]
fn transport_rejects_symlinked_repository_and_envelope_dir() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();
    let env = envelope(&cluster.nodes[0], vec![]);
    let base = tempfile::tempdir().unwrap();

    // Case A: the repository root itself is a symlink.
    let real = base.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = base.path().join("link");
    symlink(&real, &link).unwrap();
    let peer_a = transport_peer(&cluster.nodes[0], &link);
    assert!(transport.push(&peer_a, &env).is_err());

    // Case B: a real root whose envelopes directory is a symlink.
    let repo_b = base.path().join("repo_b");
    fs::create_dir_all(&repo_b).unwrap();
    let elsewhere = base.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, repo_b.join("envelopes")).unwrap();
    let peer_b = transport_peer(&cluster.nodes[0], &repo_b);
    assert!(transport.push(&peer_b, &env).is_err());
}

#[test]
fn pull_ignores_non_hksync_files() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = transport_peer(&cluster.nodes[0], repository.path());
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();

    let e = envelope(&cluster.nodes[0], vec![]);
    transport.push(&peer, &e).unwrap();
    let envelopes = repository.path().join("envelopes");
    fs::write(envelopes.join("notes.txt"), "not an envelope").unwrap();
    fs::write(envelopes.join("pending.hksync.tmp"), "partial write").unwrap();

    let pulled = transport.pull(&peer, None).unwrap();
    assert_eq!(pulled.len(), 1);
    assert_eq!(pulled[0].id, e.id);
}

#[test]
fn pull_returns_envelopes_in_filename_order_and_resumes_from_cursor() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci]);
    let repository = tempfile::tempdir().unwrap();
    let peer = transport_peer(&cluster.nodes[0], repository.path());
    let transport = FilesystemSyncTransport::new(1024 * 1024).unwrap();

    let signed = |k: i64| -> SyncEnvelope {
        let mut envelope = SyncEnvelope {
            id: SyncEnvelopeId::new(),
            protocol: SYNC_PROTOCOL_V1.into(),
            sender: cluster.nodes[0].identity.node.id.clone(),
            key_revision: 1,
            artifacts: vec![],
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000 + k, 0).unwrap(),
            nonce: format!("n{k}"),
            signature: String::new(),
        };
        envelope.sign(&cluster.nodes[0].identity).unwrap();
        envelope
    };
    let e0 = signed(0);
    let e1 = signed(1);
    let e2 = signed(2);
    // Push out of order; the transport orders by filename (timestamp-prefixed).
    transport.push(&peer, &e2).unwrap();
    transport.push(&peer, &e0).unwrap();
    transport.push(&peer, &e1).unwrap();
    let pos0 = format!("{}-{}.hksync", e0.created_at.timestamp_micros(), e0.id);

    let all = transport.pull(&peer, None).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, e0.id);
    assert_eq!(all[1].id, e1.id);
    assert_eq!(all[2].id, e2.id);

    let cursor = SyncCursor {
        peer: peer.node.clone(),
        stream: SyncStreamKind::Knowledge,
        position: pos0,
        updated_at: Utc::now(),
    };
    let tail = transport.pull(&peer, Some(&cursor)).unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].id, e1.id);
    assert_eq!(tail[1].id, e2.id);
}

// ---------------------------------------------------------------------------
// Store-level receive failure modes (Store::receive_sync_envelope).
// ---------------------------------------------------------------------------

#[test]
fn receive_rejects_key_revision_other_than_one() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    // Built manually: the signature covers key_revision=2 so verification passes and the
    // subsequent `!= 1` check is what rejects the envelope.
    let mut env = SyncEnvelope {
        id: SyncEnvelopeId::new(),
        protocol: SYNC_PROTOCOL_V1.into(),
        sender: cluster.nodes[0].identity.node.id.clone(),
        key_revision: 2,
        artifacts: vec![],
        created_at: Utc::now(),
        nonce: SyncEnvelopeId::new().to_string(),
        signature: String::new(),
    };
    env.sign(&cluster.nodes[0].identity).unwrap();

    assert!(
        cluster.nodes[1]
            .store
            .receive_sync_envelope(&env, &cluster.nodes[1].environment)
            .is_err()
    );
    assert!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .is_empty()
    );
    assert!(cluster.nodes[1].store.sync_artifacts().unwrap().is_empty());
}

#[test]
fn receive_rejects_peer_that_declines_signed_artifacts() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    configure_peer(&cluster, 1, 0, |peer| {
        peer.trust_policy.accept_signed_artifacts = false;
    });
    let env = envelope(
        &cluster.nodes[0],
        vec![artifact(
            &cluster.nodes[0],
            SyncArtifactType::Lesson,
            false,
            ArtifactAvailability::Full,
        )],
    );

    assert!(
        cluster.nodes[1]
            .store
            .receive_sync_envelope(&env, &cluster.nodes[1].environment)
            .is_err()
    );
    assert!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .is_empty()
    );
    assert!(cluster.nodes[1].store.sync_artifacts().unwrap().is_empty());
}

#[test]
fn receive_rejects_artifact_with_unsupported_schema_version() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let mut art = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    art.artifact_ref.schema_version = "hardknock.sync-artifact.v0".into();
    art.content_hash = art.computed_content_hash().unwrap();
    let env = envelope(&cluster.nodes[0], vec![art]);

    // Unsupported schema is a per-artifact rejection, not a whole-envelope error.
    let session = cluster.nodes[1]
        .store
        .receive_sync_envelope(&env, &cluster.nodes[1].environment)
        .unwrap();
    assert_eq!(session.rejected, 1);
    assert_eq!(session.accepted, 0);
    assert_eq!(session.status, SyncSessionStatus::Completed);
    assert!(
        session
            .reasons
            .iter()
            .any(|reason| reason.contains("unsupported artifact schema"))
    );
    assert!(cluster.nodes[1].store.sync_artifacts().unwrap().is_empty());
    assert!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn receive_applies_artifact_type_filter_but_admits_revocation_via_include_revocations() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    configure_peer(&cluster, 1, 0, |peer| {
        peer.filters.artifact_types = BTreeSet::from([SyncArtifactType::Constraint]);
        peer.filters.include_revocations = true;
    });
    let excluded = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    let target = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    let control = signed_revocation_control(&cluster.nodes[0], &target);
    let env = envelope(&cluster.nodes[0], vec![excluded, control]);

    let session = cluster.nodes[1]
        .store
        .receive_sync_envelope(&env, &cluster.nodes[1].environment)
        .unwrap();
    // The Lesson is filtered out; the Revocation is admitted via include_revocations.
    assert_eq!(session.rejected, 1);
    assert_eq!(session.accepted, 1);
    assert!(
        session
            .reasons
            .iter()
            .any(|reason| reason.contains("excluded by peer filter"))
    );
    assert_eq!(cluster.nodes[1].store.sync_artifacts().unwrap().len(), 1);
    // A Revocation-type record is never surfaced as advisory knowledge.
    assert!(
        cluster.nodes[1]
            .store
            .advisory_remote_knowledge()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn receive_deduplicates_same_content_artifact_from_second_envelope() {
    let cluster = TestNodeCluster::new(&[EnvironmentKind::Ci, EnvironmentKind::Ci]);
    cluster.trust(1, 0, PeerEndpoint::Filesystem("/tmp/unused".into()));
    let art = artifact(
        &cluster.nodes[0],
        SyncArtifactType::Lesson,
        false,
        ArtifactAvailability::Full,
    );
    // Fresh id + nonce per envelope => not a replay; identical content_hash + root_origin => dedup.
    let first = cluster.nodes[1]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[0], vec![art.clone()]),
            &cluster.nodes[1].environment,
        )
        .unwrap();
    let second = cluster.nodes[1]
        .store
        .receive_sync_envelope(
            &envelope(&cluster.nodes[0], vec![art.clone()]),
            &cluster.nodes[1].environment,
        )
        .unwrap();

    assert_eq!(first.accepted, 1);
    assert_eq!(second.status, SyncSessionStatus::Completed);
    assert_eq!(second.deduplicated, 1);
    assert_eq!(second.accepted, 0);
    assert_eq!(second.received, 1);
    assert_eq!(cluster.nodes[1].store.sync_artifacts().unwrap().len(), 1);
    assert_eq!(
        cluster.nodes[1]
            .store
            .remote_knowledge_records()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        cluster.nodes[1]
            .store
            .advisory_remote_knowledge()
            .unwrap()
            .len(),
        1
    );
}
