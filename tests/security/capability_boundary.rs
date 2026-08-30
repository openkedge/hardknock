// SPDX-License-Identifier: Apache-2.0

use crate::support::Fixture;
use chrono::{Duration, Utc};
use hardknock::{
    capability::*,
    core::{CapabilityManifestId, Reality, RealityId},
    dojo::{GitRealityProvider, RealityProvider, capture_state},
    effects::*,
    store::{CapabilityStore, Store, token_hash},
};
use serde_json::json;
use std::{fs, os::unix::fs::symlink};

fn isolated_reality(fixture: &Fixture, profile: &str) -> (Store, Reality, CapabilityManifest) {
    let store = Store::open(&fixture.home).unwrap();
    let mut reality = GitRealityProvider::new(&store)
        .create(&capture_state(&fixture.repo).unwrap())
        .unwrap();
    let manifest = builtin_profile(profile).unwrap();
    reality.execution_boundary = ExecutionBoundary {
        provider: "container".into(),
        capabilities: RealityProviderCapabilities::container(manifest.network.mode),
        manifest_id: Some(manifest.id.clone()),
        manifest_hash: Some(manifest.hash().unwrap()),
        manifest_revision: manifest.revision,
        image_digest: Some("sha256:test-fixture".into()),
        frozen: false,
    };
    store.update_reality(&reality).unwrap();
    store
        .insert_capability_manifest(&reality.id, &manifest)
        .unwrap();
    (store, reality, manifest)
}

#[test]
fn deny_by_default_profiles_and_provider_selection_are_truthful() {
    let offline = builtin_profile("coding-offline").unwrap();
    let policy = DenyByDefaultCapabilityPolicy;
    let denied = policy.evaluate(
        &CapabilityRequest::Network {
            endpoint: NetworkEndpointPattern {
                host: "example.invalid".into(),
                port: 443,
            },
            mutation: NetworkMutationClass::Unknown,
        },
        &offline,
    );
    assert_eq!(denied.decision, CapabilityDecision::Deny);
    let mut implicit_effect_wildcard = offline.clone();
    implicit_effect_wildcard.effects.propose = true;
    assert!(implicit_effect_wildcard.validate().is_err());
    assert_eq!(
        policy
            .evaluate(
                &CapabilityRequest::Effect {
                    stage: EffectCapabilityStage::Propose,
                    kind: EffectKind::Database,
                    target: "postgres://production/users".into(),
                    operation: EffectOperation::Update,
                },
                &implicit_effect_wildcard,
            )
            .decision,
        CapabilityDecision::Deny
    );
    let allowed = policy.evaluate(
        &CapabilityRequest::Filesystem {
            operation: FilesystemOperation::Write,
            path: "/workspace/src/lib.rs".into(),
        },
        &offline,
    );
    assert_eq!(allowed.decision, CapabilityDecision::Allow);

    let requirements = RealityRequirements {
        filesystem_isolation: IsolationLevel::Container,
        process_isolation: IsolationLevel::Process,
        network_isolation: IsolationLevel::Container,
        credential_isolation: IsolationLevel::Container,
        effect_gating: true,
    };
    assert!(
        MinimumSufficientRealityPolicy
            .select(
                &requirements,
                &[("git-worktree", RealityProviderCapabilities::git_worktree())]
            )
            .unwrap_err()
            .to_string()
            .contains("No available Reality provider")
    );
    assert_eq!(
        MinimumSufficientRealityPolicy
            .select(
                &requirements,
                &[
                    ("git-worktree", RealityProviderCapabilities::git_worktree()),
                    (
                        "container",
                        RealityProviderCapabilities::container(NetworkMode::None)
                    )
                ]
            )
            .unwrap(),
        "container"
    );
}

#[test]
fn traversal_and_symlink_escape_are_denied_while_workspace_paths_work() {
    let fixture = Fixture::new();
    let outside = fixture.temp.path().join("outside-secret");
    fs::write(&outside, "secret").unwrap();
    symlink(&outside, fixture.repo.join("escape-link")).unwrap();
    assert!(
        resolve_workspace_path(&fixture.repo, "/workspace/../outside-secret")
            .unwrap_err()
            .to_string()
            .contains("traversal")
    );
    assert!(
        resolve_workspace_path(&fixture.repo, "/workspace/escape-link")
            .unwrap_err()
            .to_string()
            .contains("escapes")
    );
    assert_eq!(
        resolve_workspace_path(&fixture.repo, "/workspace/tracked.txt").unwrap(),
        fixture.repo.canonicalize().unwrap().join("tracked.txt")
    );
    for path in [
        "/",
        "/home",
        "/var/run/docker.sock",
        "/Users/test/.ssh",
        "/Users/test/.aws",
        "/Users/test/.kube",
    ] {
        assert!(dangerous_mount(std::path::Path::new(path)));
    }
}

#[test]
fn container_command_has_no_ambient_credentials_host_network_or_dangerous_mounts() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let reality = GitRealityProvider::new(&store)
        .create(&capture_state(&fixture.repo).unwrap())
        .unwrap();
    let manifest = builtin_profile("coding-offline").unwrap();
    let provider = ContainerRealityProvider::with_runtime(
        &store,
        ContainerRuntime::named("docker").unwrap(),
        "fixture@sha256:0123456789abcdef",
    )
    .unwrap();
    let arguments = provider
        .create_arguments(&reality, &manifest, None)
        .unwrap();
    let command = arguments.join(" ");
    for forbidden in [
        "--privileged",
        "--network host",
        "/var/run/docker.sock",
        "AWS_ACCESS_KEY_ID",
        "AWS_PROFILE",
        "/.ssh",
        "/.aws",
        "/.kube",
    ] {
        assert!(
            !command.contains(forbidden),
            "unexpected {forbidden}: {command}"
        );
    }
    for required in [
        "--read-only",
        "--cap-drop ALL",
        "no-new-privileges",
        "--network none",
        "--user",
        "dst=/workspace,rw",
        "dst=/run/hardknock,ro",
        "--pids-limit 256",
        "--memory 1024m",
    ] {
        assert!(command.contains(required), "missing {required}: {command}");
    }
    assert!(
        !command.contains("--user 0:"),
        "container must be non-root: {command}"
    );
}

#[test]
fn signed_tokens_reject_tampering_cross_reality_use_revision_drift_and_revocation() {
    let fixture = Fixture::new();
    let (store, mut reality, manifest) = isolated_reality(&fixture, "coding-offline");
    let authority = CapabilityTokenAuthority::load_or_create(&store.home).unwrap();
    let token = authority
        .issue(&reality, &manifest, Duration::minutes(5))
        .unwrap();
    store.audit_capability_token(&token).unwrap();
    authority
        .verify(&token, &reality, &manifest, RealityTokenOperation::Shell)
        .unwrap();
    assert!(
        !store
            .capability_token_revoked(&token_hash(&token).unwrap())
            .unwrap()
    );

    let mut tampered = token.clone();
    tampered.claims.reality_id = RealityId::new();
    assert!(
        authority
            .verify(&tampered, &reality, &manifest, RealityTokenOperation::Shell)
            .is_err()
    );

    let mut revised = manifest.clone();
    revised.id = CapabilityManifestId::new();
    revised.revision += 1;
    revised.created_at = Utc::now();
    revised.process.allow_exec = false;
    reality.execution_boundary.manifest_id = Some(revised.id.clone());
    reality.execution_boundary.manifest_hash = Some(revised.hash().unwrap());
    reality.execution_boundary.manifest_revision = revised.revision;
    store.update_reality(&reality).unwrap();
    store
        .insert_capability_manifest(&reality.id, &revised)
        .unwrap();
    assert!(
        authority
            .verify(&token, &reality, &revised, RealityTokenOperation::Shell)
            .is_err()
    );
    store.revoke_capability_tokens(&reality.id).unwrap();
    assert!(
        store
            .capability_token_revoked(&token_hash(&token).unwrap())
            .unwrap()
    );
}

#[test]
fn scoped_credentials_never_enter_sqlite_and_known_secrets_are_redacted_then_revoked() {
    let fixture = Fixture::new();
    let (store, mut reality, previous) = isolated_reality(&fixture, "coding-offline");
    let mut manifest = previous;
    manifest.id = CapabilityManifestId::new();
    manifest.revision += 1;
    manifest.created_at = Utc::now();
    manifest.credentials.push(CredentialCapability {
        provider: "fixture".into(),
        name: "read-only".into(),
        scope: CredentialScope {
            resource: "inventory/*".into(),
        },
        permissions: vec!["read".into()],
        expires_at: Some(Utc::now() + Duration::minutes(5)),
    });
    reality.execution_boundary.manifest_id = Some(manifest.id.clone());
    reality.execution_boundary.manifest_hash = Some(manifest.hash().unwrap());
    reality.execution_boundary.manifest_revision = manifest.revision;
    store.update_reality(&reality).unwrap();
    store
        .insert_capability_manifest(&reality.id, &manifest)
        .unwrap();

    let raw = b"test-secret-value".to_vec();
    let broker = StaticTestCredentialBroker::new(&store).unwrap();
    let mut issued = broker
        .issue(
            CredentialRequest {
                provider: "fixture".into(),
                name: "read-only".into(),
                resource: "inventory/widget".into(),
                permission: "read".into(),
                secret: raw.clone(),
            },
            &reality,
            &manifest,
        )
        .unwrap();
    assert_eq!(broker.secret(&issued).unwrap(), raw);
    assert!(
        !serde_json::to_vec(&issued)
            .unwrap()
            .windows(raw.len())
            .any(|v| v == raw)
    );
    let database = fs::read(store.home.join("hardknock.db")).unwrap();
    assert!(!database.windows(raw.len()).any(|window| window == raw));
    assert_eq!(
        SecretRedactor::new([raw.clone()]).redact(b"TOKEN=test-secret-value\n"),
        b"TOKEN=[REDACTED]\n"
    );
    let materialized = broker.materialize_for_action(&reality).unwrap();
    assert!(
        materialized
            .environment()
            .contains_key("HARDKNOCK_CREDENTIAL_FIXTURE_READ_ONLY")
    );
    assert_eq!(materialized.secrets(), std::slice::from_ref(&raw));
    drop(materialized);
    broker.revoke(&mut issued).unwrap();
    assert!(broker.secret(&issued).is_err());
    let events = store.capability_events(Some(&reality.id)).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == CapabilityEventKind::CredentialIssued)
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == CapabilityEventKind::CredentialRevoked)
    );
}

#[test]
fn effect_scope_blocks_confused_deputy_and_agent_commit_but_external_authority_can_commit() {
    let fixture = Fixture::new();
    let (store, reality, _manifest) = isolated_reality(&fixture, "coding-effect-test");
    let external = MockExternalSystem::new(&store.home).unwrap();
    let target = "mock-db://inventory/widget";
    external
        .seed("mock-db", target, &json!({"quantity":10,"balance":10}))
        .unwrap();
    let manager = EffectManager::new(&store).unwrap();
    let request = |kind, target: &str, payload| EffectRequest {
        session_id: format!("reality:{}", reality.id),
        reality_id: Some(reality.id.clone()),
        source_action: ActionRef {
            id: "agent-action".into(),
            kind: "tool".into(),
        },
        kind,
        target: EffectTarget { uri: target.into() },
        operation: EffectOperation::Update,
        payload,
        adapter: None,
        evidence: vec![],
        fault: EffectFault::None,
    };
    assert!(
        manager
            .propose(
                request(
                    EffectKind::Message,
                    "mock-message://people/all",
                    json!({"body":"bypass"})
                ),
                &EffectManager::agent_context("fixture-agent")
            )
            .unwrap_err()
            .to_string()
            .contains("denied by default")
    );
    let (effect, _) = manager
        .propose_and_prepare(
            request(
                EffectKind::Database,
                target,
                json!({"quantity":9,"balance":9}),
            ),
            &EffectManager::agent_context("fixture-agent"),
        )
        .unwrap();
    assert!(
        manager
            .commit(
                &effect.id,
                None,
                &EffectManager::agent_context("fixture-agent")
            )
            .unwrap_err()
            .to_string()
            .contains("denied by default")
    );
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
        CommitOutcome::Committed { .. }
    ));
    assert_eq!(
        external.resource("mock-db", target).unwrap().mutation_count,
        1
    );
}

#[test]
fn capability_cli_lists_explains_and_diffs_profiles_without_a_container_runtime() {
    let fixture = Fixture::new();
    let listed = fixture.cli(&["capability", "list"], 0);
    assert!(
        listed["result"]["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|profile| profile == "coding-offline")
    );
    let shown = fixture.cli(&["capability", "show", "coding-effect-test"], 0);
    assert_eq!(shown["result"]["profile"], "coding-effect-test");
    let diff = fixture.cli(
        &["capability", "diff", "coding-offline", "coding-networked"],
        0,
    );
    assert!(!diff["result"]["changes"]["network"].is_null());
    let benchmark = fixture.cli(&["capability", "benchmark"], 0);
    let arms = benchmark["result"]["arms"].as_array().unwrap();
    let capability = arms
        .iter()
        .find(|arm| arm["name"] == "hardknock-capability-reality")
        .unwrap();
    assert_eq!(capability["successful_denied_capabilities"], 0);
    assert_eq!(capability["bypass_mutations"], 0);
    assert_eq!(capability["credential_exposures"], 0);
    let baseline = arms
        .iter()
        .find(|arm| arm["name"] == "container-baseline")
        .unwrap();
    assert_eq!(baseline["runtime_observed"], false);
}

#[tokio::test]
async fn bridge_accepts_reality_token_only_for_scoped_effect_events_and_rejects_tampering() {
    use hardknock::{bridge::transport, cancellation::Cancellation};
    use std::time::Duration as StdDuration;

    let mut fixture = Fixture::new();
    fixture.home = std::path::PathBuf::from(format!(
        "/tmp/hk-security-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    let (store, reality, manifest) = isolated_reality(&fixture, "coding-effect-test");
    let target = "mock-db://inventory/bridge-widget";
    MockExternalSystem::new(&store.home)
        .unwrap()
        .seed("mock-db", target, &json!({"quantity":10,"balance":10}))
        .unwrap();
    let authority = CapabilityTokenAuthority::load_or_create(&store.home).unwrap();
    let token = authority
        .issue(&reality, &manifest, Duration::minutes(5))
        .unwrap();
    store.audit_capability_token(&token).unwrap();
    let token_path = fixture.temp.path().join("reality-token.json");
    fs::write(&token_path, serde_json::to_vec(&token).unwrap()).unwrap();
    drop(store);

    let cancel = Cancellation::default();
    let server_cancel = cancel.clone();
    let home = fixture.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &server_cancel).await });
    let socket = fixture
        .home
        .join("run")
        .join("realities")
        .join(reality.id.to_string())
        .join("bridge.sock");
    for _ in 0..250 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    }
    if server.is_finished() {
        panic!("Bridge server exited early: {:?}", server.await);
    }
    assert!(
        socket.exists(),
        "Reality relay did not start; global socket exists={}, server finished={}",
        fixture.home.join("run").join("hardknock.sock").exists(),
        server.is_finished()
    );
    let request_path = fixture.temp.path().join("effect.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&EffectRequest {
            session_id: "caller-value-is-rebound".into(),
            reality_id: None,
            source_action: ActionRef {
                id: "bridge-agent-action".into(),
                kind: "tool".into(),
            },
            kind: EffectKind::Database,
            target: EffectTarget { uri: target.into() },
            operation: EffectOperation::Update,
            payload: json!({"quantity":9,"balance":9}),
            adapter: None,
            evidence: vec![],
            fault: EffectFault::None,
        })
        .unwrap(),
    )
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_hk-effect"))
        .args(["--socket"])
        .arg(&socket)
        .args(["--token"])
        .arg(&token_path)
        .arg("propose")
        .arg(&request_path)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "prepared");
    assert_eq!(response["committed"], false);

    let mut tampered = token;
    tampered.claims.reality_id = RealityId::new();
    fs::remove_file(&token_path).unwrap();
    fs::write(&token_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let denied = tokio::process::Command::new(env!("CARGO_BIN_EXE_hk-effect"))
        .args(["--socket"])
        .arg(&socket)
        .args(["--token"])
        .arg(&token_path)
        .arg("status")
        .arg(response["effect_id"].as_str().unwrap())
        .output()
        .await
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("authentication failed"));
    cancel.cancel();
    server.await.unwrap().unwrap();
    let _ = fs::remove_dir_all(&fixture.home);
}
