// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    capability::*,
    cli::{Cli, Commands},
    core::{CapabilityManifestId, MicroSandboxId, RealityId},
    store::{CapabilityStore, Store, ToolStore},
};
use chrono::{Duration, Utc};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum CapabilityCommand {
    List,
    Show {
        profile: String,
    },
    Validate {
        file: PathBuf,
    },
    Explain {
        #[arg(help = "Reality or micro-sandbox identifier")]
        subject: String,
        #[arg(long, help = "CapabilityRequest JSON")]
        request: Option<String>,
    },
    Audit {
        #[arg(long)]
        reality: Option<RealityId>,
    },
    Diff {
        left: String,
        right: String,
    },
    Revoke {
        #[arg(long)]
        reality: RealityId,
        #[arg(value_parser = ["network", "process", "credentials", "effects"])]
        capability: String,
    },
    Benchmark {
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

pub fn handles(command: &Commands) -> bool {
    matches!(command, Commands::Capability { .. })
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    let Commands::Capability { command } = &cli.command else {
        return Err(Error::InvalidInput("Capability dispatch failed".into()));
    };
    match command {
        CapabilityCommand::List => Ok(json!({"profiles":BUILTIN_PROFILES})),
        CapabilityCommand::Show { profile } => Ok(serde_json::to_value(builtin_profile(profile)?)?),
        CapabilityCommand::Validate { file } => {
            let manifest = read_manifest(file)?;
            manifest.validate()?;
            Ok(json!({
                "valid":true,
                "manifest":manifest,
                "manifest_hash":manifest.hash()?
            }))
        }
        CapabilityCommand::Explain { subject, request } => {
            if let Ok(sandbox_id) = subject.parse::<MicroSandboxId>() {
                let sandbox = store.micro_sandbox(&sandbox_id)?;
                return Ok(json!({
                    "sandbox":sandbox.id,
                    "reality":sandbox.reality_id,
                    "tool":sandbox.tool_id,
                    "runtime":sandbox.runtime,
                    "created_at":sandbox.created_at,
                    "expires_at":sandbox.expires_at,
                    "destroyed_at":sandbox.destroyed_at,
                    "effective":sandbox.capabilities,
                    "surface":sandbox.capabilities.surface()
                }));
            }
            let reality: RealityId = subject.parse().map_err(|_| {
                Error::InvalidInput(
                    "Capability explain expects a Reality or micro-sandbox identifier".into(),
                )
            })?;
            let request = request.as_deref().ok_or_else(|| {
                Error::InvalidInput("Reality capability explanation requires --request".into())
            })?;
            let reality_record = store.reality(&reality)?;
            let manifest = store.effective_capability_manifest(&reality)?;
            let request: CapabilityRequest = serde_json::from_str(request)?;
            let evaluation = DenyByDefaultCapabilityPolicy.evaluate(&request, &manifest);
            Ok(json!({
                "reality":reality_record.id,
                "provider":reality_record.execution_boundary.provider,
                "profile":manifest.profile,
                "request":request,
                "decision":evaluation.decision,
                "reason":evaluation.reason
            }))
        }
        CapabilityCommand::Audit { reality } => {
            let events = store.capability_events(reality.as_ref())?;
            let count = |kind| events.iter().filter(|event| event.kind == kind).count();
            Ok(json!({
                "reality":reality,
                "summary":{
                    "allowed":count(CapabilityEventKind::Allowed),
                    "denied":count(CapabilityEventKind::Denied),
                    "approval_required":count(CapabilityEventKind::ApprovalRequired),
                    "credentials_issued":count(CapabilityEventKind::CredentialIssued),
                    "credentials_revoked":count(CapabilityEventKind::CredentialRevoked),
                    "manifest_revisions":count(CapabilityEventKind::ManifestRevised)
                },
                "events":events
            }))
        }
        CapabilityCommand::Diff { left, right } => {
            let left = builtin_profile(left)?;
            let right = builtin_profile(right)?;
            Ok(profile_diff(&left, &right))
        }
        CapabilityCommand::Revoke {
            reality,
            capability,
        } => revoke(store, reality, capability),
        CapabilityCommand::Benchmark { output } => {
            let report = run_security_benchmark(&store.home)?;
            store.insert_capability_benchmark(&report)?;
            if let Some(path) = output {
                if path.exists() {
                    return Err(Error::Intervention(
                        "Security benchmark output already exists".into(),
                    ));
                }
                fs::write(path, serde_json::to_vec_pretty(&report)?)?;
            }
            Ok(serde_json::to_value(report)?)
        }
    }
}

pub fn issue_reality_token(
    store: &Store,
    reality: &crate::core::Reality,
) -> Result<SignedRealityCapabilityToken> {
    let manifest = store.effective_capability_manifest(&reality.id)?;
    let authority = CapabilityTokenAuthority::load_or_create(&store.home)?;
    let token = authority.issue(reality, &manifest, Duration::minutes(15))?;
    store.audit_capability_token(&token)?;
    Ok(token)
}

pub fn publish_reality_token(
    store: &Store,
    reality: &crate::core::Reality,
    token: &SignedRealityCapabilityToken,
) -> Result<PathBuf> {
    let directory = reality_control_directory(store, &reality.id);
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
    let path = directory.join("capability-token.json");
    if path.exists() {
        fs::remove_file(&path)?;
    }
    let mut options = OpenOptions::new();
    // The enclosing HARDKNOCK_HOME is 0700 on the host. Read-only mode lets the
    // non-root container UID read the file through its narrow bind mount.
    options.write(true).create_new(true).mode(0o444);
    let mut file = options.open(&path)?;
    file.write_all(&serde_json::to_vec(token)?)?;
    file.sync_all()?;
    Ok(path)
}

pub fn reality_control_directory(store: &Store, id: &RealityId) -> PathBuf {
    store
        .home
        .join("run")
        .join("realities")
        .join(id.to_string())
}

fn read_manifest(path: &Path) -> Result<CapabilityManifest> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 * 1024 {
        return Err(Error::Intervention(
            "Capability manifest must be a small regular file".into(),
        ));
    }
    let data = fs::read(path)?;
    match path.extension().and_then(|value| value.to_str()) {
        Some("toml") => toml::from_str(
            &String::from_utf8(data)
                .map_err(|_| Error::InvalidInput("Capability TOML must be UTF-8".into()))?,
        )
        .map_err(|error| Error::InvalidInput(format!("Invalid capability TOML: {error}"))),
        _ => serde_json::from_slice(&data).map_err(Into::into),
    }
}

fn revoke(store: &Store, reality_id: &RealityId, capability: &str) -> Result<Value> {
    let mut reality = store.reality(reality_id)?;
    if reality.execution_boundary.provider != "container" {
        return Err(Error::Intervention(
            "Capability revocation requires an isolated Reality provider".into(),
        ));
    }
    let previous = store.effective_capability_manifest(reality_id)?;
    let mut next = previous.clone();
    next.id = CapabilityManifestId::new();
    next.revision += 1;
    next.created_at = Utc::now();
    match capability {
        "network" => {
            next.network.mode = NetworkMode::None;
            next.network.allow.clear();
        }
        "process" => next.process.allow_exec = false,
        "credentials" => next.credentials.clear(),
        "effects" => {
            next.effects.propose = false;
            next.effects.prepare = false;
            next.effects.commit = false;
        }
        _ => return Err(Error::InvalidInput("Unknown capability class".into())),
    }
    next.validate()?;
    if capability == "credentials" {
        StaticTestCredentialBroker::new(store)?.revoke_reality(reality_id)?;
    }
    reality.execution_boundary.manifest_id = Some(next.id.clone());
    reality.execution_boundary.manifest_hash = Some(next.hash()?);
    reality.execution_boundary.manifest_revision = next.revision;
    store.insert_capability_manifest(reality_id, &next)?;
    store.revoke_capability_tokens(reality_id)?;
    store.update_reality(&reality)?;
    store.append_capability_event(&CapabilityEvent {
        id: crate::core::CapabilityEventId::new(),
        reality_id: reality_id.clone(),
        manifest_id: next.id.clone(),
        kind: CapabilityEventKind::ManifestRevised,
        request: None,
        reason: format!("{capability} capability revoked by local user"),
        created_at: Utc::now(),
    })?;
    let path = reality_control_directory(store, reality_id).join("capability-token.json");
    let _ = fs::remove_file(path);
    Ok(json!({
        "reality":reality_id,
        "revoked":capability,
        "manifest":next.id,
        "revision":next.revision,
        "new_token_required":true
    }))
}

fn profile_diff(left: &CapabilityManifest, right: &CapabilityManifest) -> Value {
    json!({
        "left":left.profile,
        "right":right.profile,
        "changes":{
            "network": if left.network == right.network { Value::Null } else { json!({"from":left.network,"to":right.network}) },
            "credentials": if left.credentials == right.credentials { Value::Null } else { json!({"from":left.credentials,"to":right.credentials}) },
            "effects": if left.effects == right.effects { Value::Null } else { json!({"from":left.effects,"to":right.effects}) },
            "filesystem": if left.filesystem == right.filesystem { Value::Null } else { json!({"from":left.filesystem,"to":right.filesystem}) }
        }
    })
}
