// SPDX-License-Identifier: Apache-2.0

mod support;

use chrono::Utc;
use hardknock::{
    capability::{NetworkMode, builtin_profile},
    core::Reality,
    dojo::{GitRealityProvider, RealityProvider, capture_state},
    store::{CapabilityStore, Store, ToolStore},
    tool::*,
    tool_runtime::{
        ContainerMicroSandboxProvider, HostMicroSandboxProvider, MicroSandboxProvider, ToolRouter,
    },
};
use serde_json::json;
use std::fs;

fn test_tool(name: &str, network: ToolNetworkCapabilities) -> ToolDefinition {
    let mut tool = ToolDefinition {
        id: hardknock::core::ToolId::new(),
        name: name.into(),
        version: "1.0.0".into(),
        description: "test tool".into(),
        invocation: ToolInvocation::NativeBinary {
            executable: "/usr/bin/printf".into(),
            args_template: vec!["ok".into()],
        },
        capabilities: ToolCapabilityManifest {
            filesystem: ToolFilesystemCapabilities {
                read: vec!["$TMP/**".into()],
                write: vec![],
            },
            process: hardknock::capability::ProcessCapabilities {
                allow_exec: true,
                allowed_executables: vec![hardknock::capability::ExecutablePattern(
                    "/usr/bin/printf".into(),
                )],
                denied_executables: vec![],
                max_processes: Some(1),
            },
            network,
            environment: ToolEnvironmentCapabilities {
                readable: vec![],
                values: Default::default(),
            },
            credentials: vec![],
            effects: hardknock::capability::EffectCapabilities {
                propose: false,
                prepare: false,
                commit: false,
                scope: hardknock::capability::EffectCapabilityScope {
                    kinds: vec![],
                    target_patterns: vec![],
                    operations: vec![],
                },
            },
            resources: hardknock::capability::ResourceLimits::default(),
            duration: DurationCapability::default(),
        },
        inputs: ToolInputSchema::default(),
        outputs: ToolOutputSchema::default(),
        integrity: ToolIntegrity {
            artifact_hash: None,
            manifest_hash: String::new(),
            signature: None,
        },
        provenance: ToolProvenance {
            source: ToolSource::Local,
            registered_at: Utc::now(),
            publisher: None,
        },
        trust: ToolTrust::LocalTrusted,
        disabled: false,
    };
    tool.integrity.manifest_hash = tool.manifest_hash().unwrap();
    tool
}

fn reality(
    fixture: &support::Fixture,
    store: &Store,
) -> (Reality, hardknock::capability::CapabilityManifest) {
    let reality = GitRealityProvider::new(store)
        .create(&capture_state(&fixture.repo).unwrap())
        .unwrap();
    let manifest = builtin_profile("coding-networked").unwrap();
    store
        .insert_capability_manifest(&reality.id, &manifest)
        .unwrap();
    (reality, manifest)
}

#[test]
fn tool_manifest_toml_is_portable_and_rejects_unsafe_paths() {
    let input = r#"
schema = "hardknock.tool.v1"
name = "run-tests"
version = "1.0.0"

[invocation]
type = "native"
executable = "printf"
args = ["ok"]

[capabilities.filesystem]
read = ["$WORKSPACE/**"]
write = ["$TMP/**"]

[capabilities.network]
mode = "none"

[capabilities.effects]
propose = false
prepare = false
commit = false

[resources]
memory_mb = 128
pids = 2
timeout_seconds = 10
"#;
    let tool = ToolDefinition::from_toml(input).unwrap();
    assert_eq!(tool.name, "run-tests");
    assert_eq!(tool.capabilities.duration.max_ms, Some(10_000));
    let encoded = tool.portable_toml().unwrap();
    let decoded = ToolDefinition::from_toml(&encoded).unwrap();
    assert_eq!(decoded.name, "run-tests");
    assert_eq!(decoded.capabilities, tool.capabilities);
    assert_eq!(decoded.inputs, tool.inputs);
    assert_eq!(decoded.outputs, tool.outputs);
    assert!(
        ToolDefinition::from_toml(&input.replace("$WORKSPACE/**", "$WORKSPACE/../secret")).is_err()
    );
}

#[test]
fn intersection_never_expands_reality_network_or_effect_authority() {
    let reality = builtin_profile("coding-networked").unwrap();
    let tool = test_tool(
        "github-request",
        ToolNetworkCapabilities {
            mode: NetworkMode::AllowList,
            allow: vec![hardknock::capability::NetworkEndpointPattern {
                host: "github.com".into(),
                port: 443,
            }],
        },
    );
    let effective = resolve_effective_capabilities(&reality, &tool.capabilities, &[]).unwrap();
    assert_eq!(effective.network.mode, NetworkMode::None);
    assert!(effective.network.allow.is_empty());
    assert!(!effective.effects.commit);
    assert_eq!(effective.filesystem.read, vec!["/tmp/**"]);
}

#[test]
fn empty_process_and_effect_intersections_are_denials_not_unrestricted_access() {
    let mut reality = builtin_profile("coding-effect-test").unwrap();
    reality.process.allowed_executables =
        vec![hardknock::capability::ExecutablePattern("/bin/echo".into())];
    let mut tool = test_tool(
        "scoped-effect",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    tool.capabilities.effects = hardknock::capability::EffectCapabilities {
        propose: true,
        prepare: true,
        commit: false,
        scope: hardknock::capability::EffectCapabilityScope {
            kinds: vec![hardknock::effects::EffectKind::Database],
            target_patterns: vec!["postgres://*".into()],
            operations: vec![hardknock::capability::EffectOperationPattern(
                hardknock::effects::EffectOperation::Update,
            )],
        },
    };
    let effective = resolve_effective_capabilities(&reality, &tool.capabilities, &[]).unwrap();
    assert!(!effective.process.allow_exec);
    assert!(effective.process.allowed_executables.is_empty());
    assert_eq!(
        effective.effects.scope.target_patterns,
        vec!["postgres://inventory_test/*"]
    );
    assert!(effective.effects.prepare);
    assert!(!effective.effects.commit);

    tool.capabilities.effects.scope.target_patterns = vec!["postgres://other/*".into()];
    let denied = resolve_effective_capabilities(&reality, &tool.capabilities, &[]).unwrap();
    assert!(!denied.effects.propose);
    assert!(!denied.effects.prepare);
}

#[test]
fn exposure_benchmark_reports_raw_dimensions_without_synthetic_score() {
    let report = hardknock::tool_benchmark::builtin_exposure_benchmark().unwrap();
    assert_eq!(report.tool_count, 6);
    assert!(report.capability_resolution_ms < 10_000);
    assert!(
        report.micro_sandboxes.network_exposure_duration_ms
            < report.session_container.network_exposure_duration_ms
    );
    assert!(report.notes.iter().any(|note| note.contains("separately")));
}

#[test]
fn builtin_tools_resolve_typed_inputs_without_shell_interpolation() {
    let tools = builtin_tools();
    assert!(tools.iter().any(|tool| tool.name == "shell-generic"));
    let read = tools.iter().find(|tool| tool.name == "read-file").unwrap();
    let invocation = resolve_invocation(read, json!({"path":"/workspace/package.json"})).unwrap();
    assert_eq!(invocation.args, vec!["/workspace/package.json"]);
    assert!(resolve_invocation(read, json!({})).is_err());

    let write = tools.iter().find(|tool| tool.name == "write-file").unwrap();
    let invocation = resolve_invocation(
        write,
        json!({"path":"/workspace/package.json","content":"literal $HOME; $(false)"}),
    )
    .unwrap();
    assert_eq!(invocation.args[3], "/workspace/package.json");
    assert_eq!(invocation.args[4], "literal $HOME; $(false)");
}

#[test]
fn container_arguments_keep_specialized_mounts_and_network_separate() {
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let (reality, manifest) = reality(&fixture, &store);
    let provider = ContainerMicroSandboxProvider::new("docker", "fixture:image").unwrap();
    let tools = builtin_tools();

    let read = tools.iter().find(|tool| tool.name == "read-file").unwrap();
    let read_caps = resolve_effective_capabilities(&manifest, &read.capabilities, &[]).unwrap();
    let read_args = provider
        .create_arguments(&reality, read, &read_caps)
        .unwrap();
    assert!(read_args.iter().any(|arg| arg == "none"));
    assert!(
        read_args
            .iter()
            .any(|arg| arg.contains("dst=/workspace,readonly"))
    );

    let tests = tools.iter().find(|tool| tool.name == "run-tests").unwrap();
    let test_caps = resolve_effective_capabilities(&manifest, &tests.capabilities, &[]).unwrap();
    let test_args = provider
        .create_arguments(&reality, tests, &test_caps)
        .unwrap();
    assert!(
        test_args
            .iter()
            .any(|arg| arg.contains("dst=/workspace,readonly"))
    );
    assert!(
        test_args
            .iter()
            .any(|arg| arg.starts_with("/workspace/.cache:rw"))
    );

    let write = tools.iter().find(|tool| tool.name == "write-file").unwrap();
    let write_caps = resolve_effective_capabilities(&manifest, &write.capabilities, &[]).unwrap();
    let write_args = provider
        .create_arguments(&reality, write, &write_caps)
        .unwrap();
    assert!(
        write_args
            .iter()
            .any(|arg| arg.contains("dst=/workspace") && !arg.contains("readonly"))
    );
    for arguments in [&read_args, &test_args, &write_args] {
        assert!(
            arguments
                .iter()
                .filter(|arg| arg.starts_with("type=bind,"))
                .all(|arg| !arg.ends_with(",rw") && !arg.ends_with(",ro"))
        );
    }
}

#[test]
fn imported_tools_are_disabled_and_commit_authority_is_rejected() {
    let mut tool = test_tool(
        "imported-tool",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    tool.provenance.source = ToolSource::Imported;
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).unwrap();
    assert!(registry.get("imported-tool").unwrap().disabled);
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    store.insert_tool_definition(&tool).unwrap();
    assert!(
        store
            .tool_definitions(true)
            .unwrap()
            .into_iter()
            .find(|stored| stored.name == "imported-tool")
            .unwrap()
            .disabled
    );
    assert!(store.tool_definition_by_name("imported-tool").is_err());
    let mut commit_tool = test_tool(
        "commit-tool",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    commit_tool.capabilities.effects.commit = true;
    assert!(commit_tool.validate().is_err());
}

#[test]
fn registry_verification_detects_manifest_mutation() {
    let tool = test_tool(
        "tamper-test",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    assert!(registry.verify("tamper-test").unwrap().manifest_matches);
    registry.get_mut("tamper-test").unwrap().description = "changed after registration".into();
    assert!(!registry.verify("tamper-test").unwrap().manifest_matches);
}

#[test]
fn registry_verification_detects_artifact_mutation() {
    let fixture = support::Fixture::new();
    let executable = fixture.temp.path().join("fixture-tool");
    fs::write(&executable, "version one").unwrap();
    let mut tool = test_tool(
        "artifact-tamper-test",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    tool.invocation = ToolInvocation::NativeBinary {
        executable: executable.display().to_string(),
        args_template: vec![],
    };
    tool.capabilities.process.allowed_executables = vec![hardknock::capability::ExecutablePattern(
        executable.display().to_string(),
    )];
    tool.integrity.artifact_hash = tool.artifact_hash().unwrap();
    tool.integrity.manifest_hash = tool.manifest_hash().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    assert!(
        registry
            .verify("artifact-tamper-test")
            .unwrap()
            .artifact_matches
    );
    fs::write(&executable, "version two").unwrap();
    assert!(
        !registry
            .verify("artifact-tamper-test")
            .unwrap()
            .artifact_matches
    );
}

#[tokio::test]
async fn capabilities_and_secret_environment_do_not_leak_between_tool_runs() {
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let (reality, mut manifest) = reality(&fixture, &store);
    manifest
        .environment
        .readable
        .push("HARDKNOCK_TEST_SECRET".into());
    manifest
        .environment
        .values
        .insert("HARDKNOCK_TEST_SECRET".into(), "invocation-a-only".into());

    let mut first = test_tool(
        "environment-a",
        ToolNetworkCapabilities {
            mode: NetworkMode::AllowList,
            allow: vec![hardknock::capability::NetworkEndpointPattern {
                host: "registry.npmjs.org".into(),
                port: 443,
            }],
        },
    );
    first.invocation = ToolInvocation::NativeBinary {
        executable: "/usr/bin/env".into(),
        args_template: vec![],
    };
    first.capabilities.process.allowed_executables =
        vec![hardknock::capability::ExecutablePattern(
            "/usr/bin/env".into(),
        )];
    first
        .capabilities
        .environment
        .values
        .insert("HARDKNOCK_TEST_SECRET".into(), "invocation-a-only".into());
    first.integrity.manifest_hash = first.manifest_hash().unwrap();

    let mut second = first.clone();
    second.id = hardknock::core::ToolId::new();
    second.name = "environment-b".into();
    second.capabilities.network = ToolNetworkCapabilities {
        mode: NetworkMode::None,
        allow: vec![],
    };
    second.capabilities.environment.values.clear();
    second.integrity.manifest_hash = second.manifest_hash().unwrap();

    let mut registry = ToolRegistry::new();
    registry.register(first).unwrap();
    registry.register(second).unwrap();
    let router = ToolRouter::new(registry, HostMicroSandboxProvider::trusted_development());
    let first_run = router
        .execute(&reality, &manifest, "environment-a", json!({}), &[])
        .await
        .unwrap();
    let second_run = router
        .execute(&reality, &manifest, "environment-b", json!({}), &[])
        .await
        .unwrap();
    assert!(first_run.result.stdout.contains("invocation-a-only"));
    assert!(!second_run.result.stdout.contains("invocation-a-only"));
    assert_eq!(
        first_run.sandbox.capabilities.network.mode,
        NetworkMode::AllowList
    );
    assert_eq!(
        second_run.sandbox.capabilities.network.mode,
        NetworkMode::None
    );
    assert_ne!(first_run.sandbox.id, second_run.sandbox.id);
    assert!(first_run.sandbox.destroyed_at.is_some());
    assert!(second_run.sandbox.destroyed_at.is_some());
}

#[tokio::test]
async fn builtin_read_file_uses_the_reality_workspace_in_explicit_host_mode() {
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let (reality, manifest) = reality(&fixture, &store);
    let read_file = builtin_tools()
        .into_iter()
        .find(|tool| tool.name == "read-file")
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(read_file).unwrap();
    let run = ToolRouter::new(registry, HostMicroSandboxProvider::trusted_development())
        .execute(
            &reality,
            &manifest,
            "read-file",
            json!({"path":"/workspace/tracked.txt"}),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(run.result.status, ToolExecutionStatus::Success);
    assert_eq!(run.result.stdout, "original\n");
    assert_eq!(run.attestation.assurance, AttestationAssurance::Observed);
}

#[tokio::test]
async fn effect_tool_emits_credentialless_request_without_starting_a_container() {
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let reality = GitRealityProvider::new(&store)
        .create(&capture_state(&fixture.repo).unwrap())
        .unwrap();
    let manifest = builtin_profile("coding-effect-test").unwrap();
    let effect_tool = builtin_tools()
        .into_iter()
        .find(|tool| tool.name == "effect-request")
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(effect_tool).unwrap();
    let provider =
        ContainerMicroSandboxProvider::new("container-runtime-must-not-be-called", "unused:image")
            .unwrap();
    let run = ToolRouter::new(registry, provider)
        .execute(
            &reality,
            &manifest,
            "effect-request",
            json!({"target":"postgres://inventory_test/items/1","value":2}),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(run.sandbox.runtime, MicroSandboxRuntime::EffectBoundary);
    assert_eq!(run.attestation.assurance, AttestationAssurance::Observed);
    assert!(run.sandbox.capabilities.network.mode == NetworkMode::None);
    assert!(run.sandbox.capabilities.credentials.is_empty());
    assert!(!run.sandbox.capabilities.effects.commit);
    assert!(run.result.effect_request().is_some());
}

#[tokio::test]
async fn host_runtime_is_explicit_and_produces_verifiable_attestation() {
    let fixture = support::Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let (reality, manifest) = reality(&fixture, &store);
    let tool = test_tool(
        "printf-tool",
        ToolNetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
    );
    store.insert_tool_definition(&tool).unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).unwrap();
    let router = ToolRouter::new(registry, HostMicroSandboxProvider::trusted_development());
    let run = router
        .execute(&reality, &manifest, "printf-tool", json!({}), &[])
        .await
        .unwrap();
    assert_eq!(
        run.result.status,
        ToolExecutionStatus::Success,
        "result: {:?}",
        run.result
    );
    assert_eq!(run.result.stdout, "ok");
    assert_eq!(run.attestation.assurance, AttestationAssurance::Observed);
    store.insert_micro_sandbox(&run.sandbox).unwrap();
    store
        .insert_execution_attestation(&run.attestation)
        .unwrap();
    let saved = store.execution_attestation(&run.attestation.id).unwrap();
    let verification = saved
        .verify(Some(&tool), Some(&manifest.hash().unwrap()))
        .unwrap();
    assert!(verification.valid, "{verification:?}");
    let mut tampered = saved;
    tampered.recorded_hash = Some("tampered".into());
    assert!(!tampered.verify(None, None).unwrap().valid);

    let sandbox_id = run.sandbox.id.to_string();
    let explained = fixture.cli(&["capability", "explain", &sandbox_id], 0);
    assert_eq!(explained["event"], "capability");
    assert_eq!(explained["result"]["sandbox"], sandbox_id);
    let attestation_id = run.attestation.id.to_string();
    let verified = fixture.cli(&["attestation", "verify", &attestation_id], 0);
    assert_eq!(verified["event"], "attestations");
    assert_eq!(verified["result"]["valid"], true);

    let invocation = resolve_invocation(&tool, json!({})).unwrap();
    let reused = router
        .provider
        .execute(&run.sandbox, &invocation)
        .await
        .unwrap();
    assert_eq!(reused.status, ToolExecutionStatus::Denied);

    let artifact_path = fixture.temp.path().join("tool-output.txt");
    fs::write(&artifact_path, "original output").unwrap();
    let original_hash = blake3::hash(b"original output").to_hex().to_string();
    let mut artifact_attestation = run.attestation.clone();
    artifact_attestation.output_hashes = vec![original_hash.clone()];
    artifact_attestation.output_artifacts = vec![hardknock::core::ArtifactRef {
        path: artifact_path.clone(),
        blake3: original_hash,
        bytes: 15,
        kind: hardknock::core::ArtifactKind::Other,
    }];
    assert!(artifact_attestation.verify(None, None).unwrap().valid);
    fs::write(&artifact_path, "mutated output").unwrap();
    assert!(!artifact_attestation.verify(None, None).unwrap().valid);

    let mut replay = run.attestation.clone();
    replay.id = hardknock::core::ExecutionAttestationId::new();
    assert_eq!(
        run.attestation.compare_replay(&replay).unwrap(),
        ReplayOutcome::ReplayMatch
    );
    replay.output_hashes[0] = "different-output".into();
    assert_eq!(
        run.attestation.compare_replay(&replay).unwrap(),
        ReplayOutcome::ReplayDivergence
    );
}
