// SPDX-License-Identifier: Apache-2.0
mod support;

use hardknock::{
    bridge::config::Config,
    cancellation::Cancellation,
    core::EnvironmentMode,
    dojo::capture_state,
    experience::ExperienceContext,
    federation::*,
    lesson::{ActionPattern, ConfidenceScore, LessonStatus},
    resilience::{ReflexResponse, ReflexStatus},
    retrieval::QueryContext,
    store::Store,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use support::Fixture;

fn digest(value: u8) -> String {
    format!("{value:02x}").repeat(32)
}
fn provenance(value: u8) -> ProvenanceNodeId {
    format!("hk-provenance:{}", digest(value)).parse().unwrap()
}
fn object_identity(node: &ExperienceNodeId, id: &str, value: u8) -> FederatedObjectIdentity {
    FederatedObjectIdentity {
        origin_node: node.clone(),
        origin_object_id: id.into(),
        lineage_hash: digest(value),
    }
}
fn context(fixture_kind: &str) -> EvidenceContext {
    EvidenceContext {
        repository_family: Some("fixture repo".into()),
        markers: vec![
            "package.json".into(),
            "pnpm-workspace.yaml".into(),
            "hardknock-fixture.json".into(),
        ],
        tags: vec![
            format!("fixture-kind:{fixture_kind}"),
            "fixture-family:pnpm-workspace-v2".into(),
        ],
        os: Some(std::env::consts::OS.into()),
        arch: Some(std::env::consts::ARCH.into()),
        ..Default::default()
    }
}
fn portable_lesson(node: &ExperienceNodeId, claim: &str) -> PortableLesson {
    PortableLesson {
        identity: object_identity(node, "lesson-remote-001", 1),
        claim: claim.into(),
        context: context("pnpm-workspace-conflict"),
        avoid: Some(ActionPattern::shell("./agent-script.sh baseline")),
        prefer: Some(ActionPattern::shell("./agent-script.sh alternative")),
        evaluation_checks: vec!["/bin/sh ./test.sh".into()],
        source_status: LessonStatus::Validated,
        source_confidence: ConfidenceScore::try_from(0.92).unwrap(),
        evidence_summary: PortableEvidenceSummary {
            support_count: 2,
            contradiction_count: 0,
            experiment_count: 1,
            application_count: 1,
            evaluation_summaries: vec!["baseline failed; alternative passed".into()],
            evidence_hashes: vec![digest(9)],
        },
        originating_agents: vec![],
        freshness: "fresh at source".into(),
        dependencies: Default::default(),
        provenance_ref: provenance(1),
    }
}
fn portable_reflex(node: &ExperienceNodeId) -> PortableReflex {
    PortableReflex {
        identity: object_identity(node, "reflex-remote-001", 2),
        trigger_context: context("pnpm-workspace-conflict"),
        proposed_action: ActionPattern::shell("npm install"),
        requested_response: ReflexResponse::Block,
        effective_response: ReflexResponse::Block,
        source_status: ReflexStatus::Active,
        confidence: ConfidenceScore::try_from(0.99).unwrap(),
        evidence_hashes: vec![digest(8)],
        provenance_ref: provenance(2),
    }
}
fn make_bundle(identity: &NodeIdentity, claim: &str, with_reflex: bool) -> SignedExperienceBundle {
    let lesson = portable_lesson(&identity.node.id, claim);
    let reflexes = if with_reflex {
        vec![portable_reflex(&identity.node.id)]
    } else {
        vec![]
    };
    let mut nodes = vec![ProvenanceNode {
        id: lesson.provenance_ref.clone(),
        kind: ProvenanceNodeKind::Lesson,
        external_id: lesson.identity.origin_object_id.clone(),
        node: identity.node.id.clone(),
        lineage_hash: Some(lesson.identity.lineage_hash.clone()),
        summary: "validated remote lesson".into(),
    }];
    if let Some(r) = reflexes.first() {
        nodes.push(ProvenanceNode {
            id: r.provenance_ref.clone(),
            kind: ProvenanceNodeKind::Reflex,
            external_id: r.identity.origin_object_id.clone(),
            node: identity.node.id.clone(),
            lineage_hash: Some(r.identity.lineage_hash.clone()),
            summary: "remote reflex requests BLOCK".into(),
        });
    }
    let mut bundle = ExperienceBundle {
        manifest: ExperienceBundleManifest {
            bundle_id: format!("hk-bundle:{}", digest(0)).parse().unwrap(),
            schema_version: BUNDLE_SCHEMA_V1.into(),
            producer: identity.node.id.clone(),
            created_at: chrono::Utc::now(),
            scope: ExportScope::Object,
            evidence_count: 1 + reflexes.len(),
            minimum_hardknock_version: None,
            labels: vec!["benchmark".into()],
            visibility: FederationVisibility::Team,
            ancestry: BundleAncestry {
                parent_bundles: vec![],
                source_nodes: vec![identity.node.id.clone()],
            },
        },
        experiences: vec![],
        lessons: vec![lesson],
        skills: vec![],
        experiments: vec![],
        reflexes,
        recoveries: vec![],
        envelopes: vec![],
        provenance: ProvenanceGraph {
            nodes,
            edges: vec![],
        },
    };
    bundle.manifest.bundle_id = bundle.computed_id().unwrap();
    identity.sign(bundle).unwrap()
}
fn query(f: &Fixture) -> (hardknock::core::StateRef, QueryContext) {
    let state = capture_state(&f.repo).unwrap();
    let context =
        ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled).unwrap();
    (
        state,
        QueryContext::new(&context, "federation test", vec![]),
    )
}
fn sender(temp: &Path) -> NodeIdentity {
    fs::create_dir_all(temp).unwrap();
    NodeIdentity::load_or_create(temp, "platform-team", ExperienceNodeType::Team).unwrap()
}

#[test]
fn node_identity_is_content_addressed_private_and_signatures_detect_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let identity = sender(temp.path());
    assert!(identity.node.id.to_string().starts_with("hk-node:"));
    assert_eq!(
        fs::metadata(&identity.private_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let signed = make_bundle(&identity, "prefer the tested package manager", false);
    let key = embedded_verifying_key(&signed).unwrap();
    verify_signed_bundle(&signed, &key).unwrap();
    let mut tampered = signed;
    tampered.bundle.lessons[0].claim.push_str(" poisoned");
    assert!(verify_signed_bundle(&tampered, &key).is_err());
}

#[test]
fn export_redaction_removes_tokens_headers_secret_fields_and_home_paths() {
    let temp = tempfile::tempdir().unwrap();
    let identity = sender(temp.path());
    let mut signed = make_bundle(
        &identity,
        "Authorization: Bearer abcdef123456 API_TOKEN=topsecret AWS_SECRET_ACCESS_KEY=hunter2 at /Users/alice/company/repo",
        false,
    );
    signed.bundle.lessons[0]
        .evidence_summary
        .evaluation_summaries
        .push("AKIAABCDEFGHIJKLMNOP".into());
    let redacted = DeterministicFederationRedaction { repository: None }
        .redact(signed.bundle)
        .unwrap();
    let serialized = serde_json::to_string(&redacted).unwrap();
    for secret in [
        "abcdef123456",
        "topsecret",
        "hunter2",
        "AKIAABCDEFGHIJKLMNOP",
        "/Users/alice",
    ] {
        assert!(!serialized.contains(secret), "leaked {secret}");
    }
    assert!(serialized.contains("[REDACTED"));
}

#[test]
fn filesystem_transport_is_content_addressed_indexed_and_network_free() {
    let temp = tempfile::tempdir().unwrap();
    let identity = sender(&temp.path().join("sender"));
    let signed = make_bundle(&identity, "portable lesson", false);
    let repo = temp.path().join("team-experience");
    let transport = FilesystemTransport::new(&repo, 5 * 1024 * 1024).unwrap();
    let path = transport.publish(&signed).unwrap();
    assert!(path.is_file());
    let results = transport
        .search(&FederationSelector {
            marker: Some("pnpm-workspace.yaml".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    let fetched = transport.fetch(&FederationSelector::default()).unwrap();
    assert_eq!(fetched[0].payload_hash, signed.payload_hash);
    assert_eq!(transport.publish(&signed).unwrap(), path);
}

#[tokio::test]
async fn imported_lesson_stays_advisory_then_supports_or_conflicts_after_local_experiment() {
    let signing = tempfile::tempdir().unwrap();
    let identity = sender(signing.path());
    let signed = make_bundle(&identity, "use the workspace package manager", true);
    let support = Fixture::from_fixture("pnpm-workspace-transfer");
    let support_store = Store::open(&support.home).unwrap();
    let config = Config::default();
    let service = LocalFederationService {
        store: &support_store,
        config: &config,
    };
    let key = embedded_verifying_key(&signed).unwrap();
    support_store
        .add_peer("platform-team", &public_key_hex(&key), &identity.node.id)
        .unwrap();
    let (state, local_query) = query(&support);
    let report = service.import(signed.clone(), &local_query).unwrap();
    assert_eq!(report.authenticity, AuthenticityStatus::SignatureValid);
    let lesson_id = report
        .objects
        .iter()
        .find(|id| support_store.federated_object(id).unwrap().object_type == "lesson")
        .unwrap()
        .clone();
    let before = support_store.federated_object(&lesson_id).unwrap();
    assert_eq!(
        before.state,
        FederatedExperienceState::ReproductionRecommended
    );
    let reflex = support_store
        .federated_objects()
        .unwrap()
        .into_iter()
        .find(|o| o.object_type == "reflex")
        .unwrap();
    assert_eq!(reflex.object["requested_response"], "block");
    assert_eq!(reflex.object["effective_response"], "advise");
    let reproduced = service
        .reproduce(&lesson_id, state, vec![], &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(reproduced.result, ReproductionResult::Supports);
    assert_eq!(
        support_store.federated_object(&lesson_id).unwrap().state,
        FederatedExperienceState::LocallySupported
    );
    let contradiction = Fixture::from_fixture("pnpm-workspace-contradiction");
    let contradiction_store = Store::open(&contradiction.home).unwrap();
    let service = LocalFederationService {
        store: &contradiction_store,
        config: &config,
    };
    contradiction_store
        .add_peer("platform-team", &public_key_hex(&key), &identity.node.id)
        .unwrap();
    let (state, local_query) = query(&contradiction);
    let report = service.import(signed, &local_query).unwrap();
    let lesson_id = report
        .objects
        .into_iter()
        .find(|id| {
            contradiction_store
                .federated_object(id)
                .unwrap()
                .object_type
                == "lesson"
        })
        .unwrap();
    let reproduced = service
        .reproduce(&lesson_id, state, vec![], &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(reproduced.result, ReproductionResult::Contradicts);
    assert_eq!(
        contradiction_store
            .federated_object(&lesson_id)
            .unwrap()
            .state,
        FederatedExperienceState::LocallyContradicted
    );
    let conflict = contradiction_store
        .federated_conflicts()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(conflict.external_object, lesson_id);
    assert!(!conflict.local_evidence.is_empty());
    assert!(!conflict.remote_evidence.is_empty());
    let graph = contradiction_store
        .provenance_graph("lesson-remote-001")
        .unwrap();
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.kind == ProvenanceNodeKind::Experiment)
    );
}

#[tokio::test]
async fn reexport_preserves_origin_lineage_and_suppresses_duplicate_evidence() {
    let signing = tempfile::tempdir().unwrap();
    let identity_a = sender(signing.path());
    let original = make_bundle(&identity_a, "deduplicated lesson", false);
    let key_a = embedded_verifying_key(&original).unwrap();
    let b = Fixture::from_fixture("pnpm-workspace-transfer");
    let store_b = Store::open(&b.home).unwrap();
    let mut config_b = Config::default();
    config_b.federation.node_name = "team-b".into();
    let service_b = LocalFederationService {
        store: &store_b,
        config: &config_b,
    };
    store_b
        .add_peer("team-a", &public_key_hex(&key_a), &identity_a.node.id)
        .unwrap();
    let (state, query_b) = query(&b);
    let imported = service_b.import(original.clone(), &query_b).unwrap();
    let lesson_b = imported
        .objects
        .into_iter()
        .find(|id| store_b.federated_object(id).unwrap().object_type == "lesson")
        .unwrap();
    service_b
        .reproduce(&lesson_b, state, vec![], &Cancellation::default())
        .await
        .unwrap();
    let reexport = service_b
        .reexport(&lesson_b, vec!["reexport".into()])
        .unwrap();
    assert_eq!(
        reexport.bundle.lessons[0].identity.origin_node,
        identity_a.node.id
    );
    assert_eq!(reexport.bundle.experiences.len(), 2);
    let c = Fixture::from_fixture("pnpm-workspace-transfer");
    let store_c = Store::open(&c.home).unwrap();
    let config_c = Config::default();
    let service_c = LocalFederationService {
        store: &store_c,
        config: &config_c,
    };
    store_c
        .add_peer("team-a", &public_key_hex(&key_a), &identity_a.node.id)
        .unwrap();
    let key_b = embedded_verifying_key(&reexport).unwrap();
    store_c
        .add_peer("team-b", &public_key_hex(&key_b), &reexport.signer)
        .unwrap();
    let (_, query_c) = query(&c);
    let first = service_c.import(original, &query_c).unwrap();
    let second = service_c.import(reexport, &query_c).unwrap();
    assert_eq!(first.duplicates, 0);
    assert_eq!(second.duplicates, 1);
    let hashes: Vec<_> = store_c
        .federated_objects()
        .unwrap()
        .into_iter()
        .map(|o| o.identity.lineage_hash)
        .collect();
    let unique: std::collections::HashSet<_> = hashes.iter().collect();
    assert_eq!(hashes.len(), unique.len());
}

#[test]
fn malicious_bundles_are_rejected_without_partial_import() {
    let signing = tempfile::tempdir().unwrap();
    let identity = sender(signing.path());
    let mut signed = make_bundle(&identity, "safe", false);
    signed.signature.replace_range(0..2, "ff");
    let f = Fixture::from_fixture("pnpm-workspace-transfer");
    let store = Store::open(&f.home).unwrap();
    let config = Config::default();
    let service = LocalFederationService {
        store: &store,
        config: &config,
    };
    let (_, query) = query(&f);
    assert!(service.import(signed, &query).is_err());
    assert_eq!(store.federation_status().unwrap().received_bundles, 0);
    let mut traversal = make_bundle(&identity, "../../escape", false);
    traversal.bundle.manifest.bundle_id = traversal.bundle.computed_id().unwrap();
    let traversal = identity.sign(traversal.bundle).unwrap();
    assert!(service.import(traversal, &query).is_err());
    assert_eq!(store.federation_status().unwrap().received_bundles, 0);
    let mut bad_ref = make_bundle(&identity, "bad reference", false);
    bad_ref.bundle.lessons[0].provenance_ref = provenance(99);
    bad_ref.bundle.manifest.bundle_id = bad_ref.bundle.computed_id().unwrap();
    let bad_ref = identity.sign(bad_ref.bundle).unwrap();
    assert!(service.import(bad_ref, &query).is_err());
    assert_eq!(store.federation_status().unwrap().received_bundles, 0);
}

#[test]
fn cli_exports_imports_and_reproduces_a_real_validated_lesson() {
    let origin = Fixture::from_fixture("pnpm-workspace-conflict");
    let trained = origin.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "--retry-with-experience",
            "learn package manager",
        ],
        0,
    );
    let lesson = trained["lesson"]["id"].as_str().unwrap();
    let mut validation = Fixture::from_fixture("pnpm-workspace-transfer");
    validation.home = origin.home.clone();
    let applied = validation.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "validate transfer",
        ],
        0,
    );
    assert_eq!(applied["lesson"]["status"], "validated");
    let output = origin.temp.path().join("lesson.hkexp");
    let exported = origin.cli(
        &[
            "federate",
            "export",
            "--lesson",
            lesson,
            "--output",
            output.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(exported["result"]["bundle_id"].as_str().unwrap().len(), 74);
    let dry = origin.cli(&["federate", "export", "--lesson", lesson, "--dry-run"], 0);
    assert_eq!(dry["result"]["published"], false);
    let receiver = Fixture::from_fixture("pnpm-workspace-transfer");
    receiver.cli(
        &[
            "peer",
            "add",
            "--name",
            "platform-team",
            "--public-key",
            origin.home.join("identity/node.pub").to_str().unwrap(),
        ],
        0,
    );
    let imported = receiver.cli(&["federate", "import", output.to_str().unwrap()], 0);
    assert_eq!(
        imported["result"]["report"]["authenticity"],
        "signature_valid"
    );
    assert_eq!(
        imported["result"]["report"]["state"],
        "imported as advisory evidence"
    );
    let search = receiver.cli(&["federate", "search", "--kind", "lesson"], 0);
    let id = search["result"]["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reproduced = receiver.cli(&["federate", "test", &id], 0);
    assert_eq!(reproduced["result"]["reproduction"]["result"], "supports");
    assert_eq!(reproduced["result"]["object"]["state"], "locally_supported");
    let graph = receiver.cli(&["provenance", lesson], 0);
    assert!(graph["result"]["graph"]["nodes"].as_array().unwrap().len() >= 4);
}

#[test]
fn thousand_bundles_and_ten_thousand_external_objects_remain_practical() {
    let signing = tempfile::tempdir().unwrap();
    let identity = sender(signing.path());
    let receiver = Fixture::from_fixture("pnpm-workspace-transfer");
    let store = Store::open(&receiver.home).unwrap();
    let config = Config::default();
    let service = LocalFederationService {
        store: &store,
        config: &config,
    };
    let (_, local) = query(&receiver);
    let started = std::time::Instant::now();
    let mut last = None;
    for bundle_index in 0..1000usize {
        let mut experiences = Vec::with_capacity(10);
        let mut nodes = Vec::with_capacity(10);
        for object_index in 0..10usize {
            let key = format!("{bundle_index}:{object_index}");
            let hash = blake3::hash(key.as_bytes()).to_hex().to_string();
            let prov: ProvenanceNodeId = format!("hk-provenance:{hash}").parse().unwrap();
            let object_id = format!("external-experience-{bundle_index}-{object_index}");
            experiences.push(PortableExperience {
                identity: FederatedObjectIdentity {
                    origin_node: identity.node.id.clone(),
                    origin_object_id: object_id.clone(),
                    lineage_hash: hash.clone(),
                },
                created_at: chrono::Utc::now(),
                goal: "bounded normalized task".into(),
                context: context("pnpm-workspace-transfer"),
                starting_state_hash: hash.clone(),
                outcome: hardknock::experience::Outcome::Success,
                evaluation_summary: "pass".into(),
                originating_agent: hardknock::core::AgentIdentity {
                    kind: "scale-fixture".into(),
                    executable: "fixture".into(),
                    version: Some("1".into()),
                    model: None,
                },
                dependencies: Default::default(),
                provenance_ref: prov.clone(),
            });
            nodes.push(ProvenanceNode {
                id: prov,
                kind: ProvenanceNodeKind::Experience,
                external_id: object_id,
                node: identity.node.id.clone(),
                lineage_hash: Some(hash),
                summary: "scale fixture".into(),
            });
        }
        let mut bundle = ExperienceBundle {
            manifest: ExperienceBundleManifest {
                bundle_id: format!("hk-bundle:{}", digest(0)).parse().unwrap(),
                schema_version: BUNDLE_SCHEMA_V1.into(),
                producer: identity.node.id.clone(),
                created_at: chrono::Utc::now(),
                scope: ExportScope::Selected(vec![]),
                evidence_count: 10,
                minimum_hardknock_version: None,
                labels: vec!["scale".into()],
                visibility: FederationVisibility::Team,
                ancestry: BundleAncestry {
                    parent_bundles: vec![],
                    source_nodes: vec![identity.node.id.clone()],
                },
            },
            experiences,
            lessons: vec![],
            skills: vec![],
            experiments: vec![],
            reflexes: vec![],
            recoveries: vec![],
            envelopes: vec![],
            provenance: ProvenanceGraph {
                nodes,
                edges: vec![],
            },
        };
        bundle.manifest.bundle_id = bundle.computed_id().unwrap();
        let signed = identity.sign(bundle).unwrap();
        service.import(signed.clone(), &local).unwrap();
        last = Some(signed);
    }
    let import_ms = started.elapsed().as_millis();
    assert_eq!(store.federation_status().unwrap().received_bundles, 1000);
    assert_eq!(store.federated_objects().unwrap().len(), 10_000);
    let search_start = std::time::Instant::now();
    let results = store
        .search_federated(Some("experience"), Some("pnpm-workspace.yaml"))
        .unwrap();
    let search_ms = search_start.elapsed().as_millis();
    assert_eq!(results.len(), 10_000);
    let provenance_start = std::time::Instant::now();
    let graph = store.provenance_graph("external-experience-999-9").unwrap();
    let provenance_ms = provenance_start.elapsed().as_millis();
    assert_eq!(graph.nodes.len(), 2);
    let duplicate_start = std::time::Instant::now();
    let duplicate = service.import(last.unwrap(), &local).unwrap();
    let duplicate_ms = duplicate_start.elapsed().as_millis();
    assert_eq!(duplicate.duplicates, 10);
    eprintln!(
        "federation scale: import={import_ms}ms search={search_ms}ms provenance={provenance_ms}ms duplicate={duplicate_ms}ms"
    );
    assert!(import_ms < 30_000);
    assert!(search_ms < 5_000);
    assert!(provenance_ms < 5_000);
    assert!(duplicate_ms < 5_000);
}

#[tokio::test]
async fn deterministic_three_node_benchmark_meets_transfer_and_safety_gates() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let result = hardknock::federation::benchmark::run(&store, &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(result.status, "completed");
    assert_eq!(
        result.scenarios["successful_transfer"]["reproduction"]["result"],
        "supports"
    );
    assert_eq!(
        result.scenarios["contradiction"]["reproduction"]["result"],
        "contradicts"
    );
    assert_eq!(
        result.scenarios["duplicate_reexport"]["duplicates_suppressed"],
        1
    );
    assert_eq!(result.scenarios["remote_reflex"]["effective"], "ADVISE");
    assert_eq!(
        result.metrics["task_success"]["hardknock_federation"],
        "2/2"
    );
    assert_eq!(result.metrics["task_success"]["naive_shared"], "1/2");
    assert!(result.artifact.is_file());
    assert_eq!(store.federation_benchmarks().unwrap().len(), 1);
}
