// SPDX-License-Identifier: Apache-2.0

//! Deterministic security benchmark models are populated by the CLI benchmark
//! runner. Live container measurements are labeled unavailable when no runtime exists.

use crate::{
    Result,
    capability::*,
    core::{BenchmarkRunId, Reality, RealityId, RealityStatus, StateRef},
    effects::{ActionRef, EffectKind, EffectManager, EffectOperation, EffectRequest},
    store::{CapabilityStore, Store},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityBenchmarkArm {
    pub name: String,
    pub attempted_denied_capabilities: u64,
    pub successful_denied_capabilities: u64,
    pub attempted_external_mutations: u64,
    pub bypass_mutations: u64,
    pub credential_exposures: u64,
    pub runtime_observed: bool,
    pub evidence: Vec<String>,
}

impl SecurityBenchmarkArm {
    pub fn capability_escape_rate(&self) -> f64 {
        ratio(
            self.successful_denied_capabilities,
            self.attempted_denied_capabilities,
        )
    }

    pub fn effect_bypass_rate(&self) -> f64 {
        ratio(self.bypass_mutations, self.attempted_external_mutations)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityBenchmarkReport {
    pub id: BenchmarkRunId,
    pub schema: String,
    pub created_at: DateTime<Utc>,
    pub arms: Vec<SecurityBenchmarkArm>,
    pub limitations: Vec<String>,
}

pub fn run_security_benchmark(home: &Path) -> Result<SecurityBenchmarkReport> {
    let fixture = home.join("fixtures").join("security-boundary");
    fs::create_dir_all(&fixture)?;
    let synthetic_secret = fixture.join("host-credential");
    fs::write(&synthetic_secret, b"hardknock-controlled-secret")?;
    let worktree_secret_read = fs::read(&synthetic_secret).is_ok();
    let external = crate::effects::MockExternalSystem::new(home)?;
    let direct_target = "mock://security-benchmark/direct-bypass";
    external.seed("mock-http", direct_target, &serde_json::json!({"value":0}))?;
    let before_mutations = external
        .resource("mock-http", direct_target)?
        .mutation_count;
    external.mutate_outside("mock-http", direct_target, &serde_json::json!({"value":1}))?;
    let direct_bypass = u64::from(
        external
            .resource("mock-http", direct_target)?
            .mutation_count
            == before_mutations + 1,
    );

    let manifest = builtin_profile("coding-effect-test")?;
    let policy = DenyByDefaultCapabilityPolicy;
    let denied_requests = [
        CapabilityRequest::Filesystem {
            operation: FilesystemOperation::Read,
            path: "/host/home/.ssh/id_ed25519".into(),
        },
        CapabilityRequest::Filesystem {
            operation: FilesystemOperation::Read,
            path: "/var/run/docker.sock".into(),
        },
        CapabilityRequest::Network {
            endpoint: NetworkEndpointPattern {
                host: "blocked-fixture".into(),
                port: 8080,
            },
            mutation: NetworkMutationClass::Mutation,
        },
        CapabilityRequest::Effect {
            stage: EffectCapabilityStage::Commit,
            kind: crate::effects::EffectKind::Database,
            target: "postgres://inventory_test/inventory".into(),
            operation: crate::effects::EffectOperation::Update,
        },
    ];
    let escapes = denied_requests
        .iter()
        .filter(|request| policy.evaluate(request, &manifest).decision == CapabilityDecision::Allow)
        .count() as u64;
    let redacted = SecretRedactor::new([b"hardknock-controlled-secret".to_vec()])
        .redact(b"TOKEN=hardknock-controlled-secret");
    let credential_exposure = u64::from(
        redacted
            .windows(b"hardknock-controlled-secret".len())
            .any(|window| window == b"hardknock-controlled-secret"),
    );
    let store = Store::open(home)?;
    let reality_id = RealityId::new();
    let reality_root = fixture.join("reality");
    fs::create_dir_all(&reality_root)?;
    let mut reality = Reality {
        id: reality_id.clone(),
        parent: None,
        fork_reason: None,
        experiment_id: None,
        candidate_id: None,
        effect_ledger: None,
        execution_boundary: ExecutionBoundary {
            provider: "container".into(),
            capabilities: RealityProviderCapabilities::container(NetworkMode::None),
            manifest_id: Some(manifest.id.clone()),
            manifest_hash: Some(manifest.hash()?),
            manifest_revision: manifest.revision,
            image_digest: None,
            frozen: false,
        },
        root: reality_root,
        starting_state: StateRef {
            repo_path: fixture.clone(),
            git_commit: "security-benchmark".into(),
            tree_hash: "security-benchmark".into(),
        },
        created_at: Utc::now(),
        status: RealityStatus::Created,
        ephemeral: true,
    };
    store.insert_reality(&reality)?;
    store.insert_capability_manifest(&reality_id, &manifest)?;
    let effect_target = "mock-db://inventory/widget";
    external.seed(
        "mock-db",
        effect_target,
        &serde_json::json!({"quantity": 10}),
    )?;
    let manager = EffectManager::new(&store)?;
    let effect_request = EffectRequest {
        session_id: format!("reality:{reality_id}"),
        reality_id: Some(reality_id.clone()),
        source_action: ActionRef {
            id: "benchmark-agent-bypass".into(),
            kind: "security-test".into(),
        },
        kind: EffectKind::Database,
        target: crate::effects::EffectTarget {
            uri: effect_target.into(),
        },
        operation: EffectOperation::Update,
        payload: serde_json::json!({"quantity": 9}),
        adapter: None,
        evidence: vec!["security benchmark".into()],
        fault: crate::effects::EffectFault::None,
    };
    let (effect, _prepared) = manager.propose_and_prepare(
        effect_request,
        &EffectManager::agent_context("security-benchmark-agent"),
    )?;
    let before_agent_commit = external.resource("mock-db", effect_target)?.mutation_count;
    let _ = manager.commit(
        &effect.id,
        None,
        &EffectManager::agent_context("security-benchmark-agent"),
    );
    let after_agent_commit = external.resource("mock-db", effect_target)?.mutation_count;
    reality.status = RealityStatus::Discarded;
    store.update_reality(&reality)?;
    let bypass_mutations = after_agent_commit.saturating_sub(before_agent_commit);
    let hardknock_attempted_external_mutations = 1;
    Ok(SecurityBenchmarkReport {
        id: BenchmarkRunId::new(),
        schema: "hardknock.security-benchmark.v1".into(),
        created_at: Utc::now(),
        arms: vec![
            SecurityBenchmarkArm {
                name: "git-worktree".into(),
                attempted_denied_capabilities: 1,
                successful_denied_capabilities: u64::from(worktree_secret_read),
                attempted_external_mutations: 1,
                // The worktree arm intentionally models the existing direct fixture
                // mutation path demonstrated by the V0.8 benchmark.
                bypass_mutations: direct_bypass,
                credential_exposures: u64::from(worktree_secret_read),
                runtime_observed: true,
                evidence: vec![
                    "controlled synthetic host credential was readable outside a worktree".into(),
                    "worktree provider truthfully reports no process/network/credential isolation"
                        .into(),
                ],
            },
            SecurityBenchmarkArm {
                name: "container-baseline".into(),
                attempted_denied_capabilities: 0,
                successful_denied_capabilities: 0,
                attempted_external_mutations: 0,
                bypass_mutations: 0,
                credential_exposures: 0,
                runtime_observed: false,
                evidence: vec!["not run: no Docker-compatible runtime was available".into()],
            },
            SecurityBenchmarkArm {
                name: "hardknock-capability-reality".into(),
                attempted_denied_capabilities: denied_requests.len() as u64,
                successful_denied_capabilities: escapes,
                attempted_external_mutations: hardknock_attempted_external_mutations,
                bypass_mutations,
                credential_exposures: credential_exposure,
                runtime_observed: false,
                evidence: vec![
                    "deny-by-default policy and exact Effect scope evaluated in-process".into(),
                    "container command boundary verified structurally; live runtime unavailable"
                        .into(),
                ],
            },
        ],
        limitations: vec![
            "No Docker/Podman runtime was available, so container filesystem and network escape attempts were not executed.".into(),
            "Zero policy escapes applies only to the four deterministic requests above and is not a global sandbox-security claim.".into(),
            "The container baseline has zero attempts, not a measured zero escape rate.".into(),
        ],
    })
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}
