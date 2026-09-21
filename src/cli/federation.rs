// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands, warning};
use crate::{
    Error, Result, bridge::config::Config, cancellation::Cancellation, core::*,
    dojo::capture_state, experience::ExperienceContext, federation::*, retrieval::QueryContext,
    store::Store,
};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf};

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    Show,
    Identity,
    Environment,
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    Pull {
        peer: String,
    },
    Push {
        peer: String,
        #[arg(long)]
        envelope: PathBuf,
    },
    Status,
    History,
    Inspect {
        session: SyncSessionId,
    },
}

#[derive(Debug, Subcommand)]
pub enum PeerCommand {
    List,
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        public_key: PathBuf,
        #[arg(long)]
        sync_dir: Option<PathBuf>,
    },
    Show {
        peer: String,
    },
    Trust {
        peer: String,
    },
    Block {
        peer: String,
    },
    Remove {
        peer: String,
    },
}
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("object").required(true).multiple(false).args(["lesson","skill","reflex","abstract_knowledge","external"])))]
pub struct ExportArgs {
    #[arg(long)]
    pub lesson: Option<LessonId>,
    #[arg(long)]
    pub skill: Option<String>,
    #[arg(long)]
    pub reflex: Option<ReflexId>,
    #[arg(long)]
    pub abstract_knowledge: Option<AbstractKnowledgeId>,
    #[arg(long)]
    pub external: Option<FederatedObjectId>,
    #[arg(long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long = "label")]
    pub labels: Vec<String>,
    #[arg(long)]
    pub include_artifacts: bool,
}
#[derive(Debug, Subcommand)]
pub enum FederateCommand {
    Status,
    Export(ExportArgs),
    Import {
        bundle: PathBuf,
    },
    Test {
        id: FederatedObjectId,
        #[arg(long = "check")]
        checks: Vec<String>,
    },
    Promote {
        id: FederatedObjectId,
        #[arg(long)]
        experience: ExperienceId,
    },
    Publish {
        object: String,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        namespace: Option<String>,
    },
    Search {
        #[arg(long)]
        producer: Option<String>,
        #[arg(long)]
        task_family: Option<String>,
        #[arg(long)]
        marker: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        repository: Option<PathBuf>,
        #[arg(long)]
        kind: Option<String>,
    },
    Backlog,
    Audit,
    Reproduce {
        artifact_hash: String,
        #[arg(long)]
        relevance: String,
    },
    Compare {
        left: FederatedObjectId,
        right: FederatedObjectId,
    },
}
#[derive(Debug, Subcommand)]
pub enum ConflictCommand {
    List,
    Show {
        id: FederatedConflictId,
    },
    Test {
        id: FederatedConflictId,
        #[arg(long = "check")]
        checks: Vec<String>,
    },
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Node { .. }
            | Commands::Sync { .. }
            | Commands::Peer { .. }
            | Commands::Federate { .. }
            | Commands::Provenance { .. }
            | Commands::Conflict { .. }
    )
}
fn context(cli: &Cli) -> Result<(StateRef, QueryContext)> {
    let state = capture_state(&cli.repo)?;
    let experience =
        ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled)?;
    let query = QueryContext::new(&experience, "federation compatibility", vec![]);
    Ok((state, query))
}
fn bundle(
    service: &LocalFederationService<'_>,
    args: &ExportArgs,
) -> Result<SignedExperienceBundle> {
    if args.include_artifacts {
        return Err(Error::Intervention(
            "Raw artifact federation remains disabled; V0.7 exports hashes and summaries only"
                .into(),
        ));
    }
    if let Some(id) = &args.lesson {
        service.export_lesson(id, args.labels.clone())
    } else if let Some(name) = &args.skill {
        service.export_skill(name, args.labels.clone())
    } else if let Some(id) = &args.reflex {
        service.export_reflex(id, args.labels.clone())
    } else if let Some(id) = &args.abstract_knowledge {
        service.export_abstraction(id, args.labels.clone())
    } else if let Some(id) = &args.external {
        service.reexport(id, args.labels.clone())
    } else {
        Err(Error::InvalidInput("Choose one export object".into()))
    }
}
fn publish_bundle(
    service: &LocalFederationService<'_>,
    object: &str,
    namespace: Option<&str>,
) -> Result<SignedExperienceBundle> {
    let labels = namespace
        .map(|v| vec![format!("namespace:{v}")])
        .unwrap_or_default();
    if let Ok(id) = object.parse::<LessonId>() {
        service.export_lesson(&id, labels)
    } else if let Ok(id) = object.parse::<AbstractKnowledgeId>() {
        service.export_abstraction(&id, labels)
    } else if let Ok(id) = object.parse::<FederatedObjectId>() {
        service.reexport(&id, labels)
    } else {
        service.export_skill(object, labels)
    }
}

pub async fn execute(cli: &Cli, store: &Store, cancel: &Cancellation) -> Result<Value> {
    let config = Config::load(&store.home)?;
    let service = LocalFederationService {
        store,
        config: &config,
    };
    Ok(match &cli.command {
        Commands::Node { command } => {
            let identity = service.identity()?;
            let node = if let Some(node) = store.hardknock_node()? {
                node
            } else {
                store.initialize_hardknock_node(&identity, EnvironmentIdentity::default())?
            };
            match command {
                NodeCommand::Show => json!({"kind":"hardknock_node","node":node}),
                NodeCommand::Identity => {
                    json!({"kind":"node_identity","node":node.id,"identity":node.identity,"key_revision":node.key_revision,"label":node.label})
                }
                NodeCommand::Environment => {
                    json!({"kind":"node_environment","node":node.id,"environment":node.environment})
                }
            }
        }
        Commands::Sync { command } => match command {
            SyncCommand::Pull { peer } => {
                let configured = store.sync_peer(peer)?;
                let transport =
                    FilesystemSyncTransport::new(config.federation.limits.max_bundle_bytes)?;
                let cursor = store.sync_cursor(&configured.node, SyncStreamKind::Knowledge)?;
                let envelopes = transport.pull(&configured, cursor.as_ref())?;
                let local = store.hardknock_node()?.ok_or_else(|| {
                    Error::InvalidInput(
                        "Initialize the local node with `hardknock node show`".into(),
                    )
                })?;
                let mut sessions = Vec::new();
                for envelope in &envelopes {
                    let session = store.receive_sync_envelope(envelope, &local.environment)?;
                    let position = format!(
                        "{}-{}.hksync",
                        envelope.created_at.timestamp_micros(),
                        envelope.id
                    );
                    store.save_sync_cursor(&SyncCursor {
                        peer: configured.node.clone(),
                        stream: SyncStreamKind::Knowledge,
                        position,
                        updated_at: chrono::Utc::now(),
                    })?;
                    sessions.push(session);
                }
                json!({"kind":"sync_pull","peer":configured.node,"sessions":sessions,"envelopes":envelopes.len()})
            }
            SyncCommand::Push { peer, envelope } => {
                let configured = store.sync_peer(peer)?;
                let metadata = std::fs::symlink_metadata(envelope)?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > config.federation.limits.max_bundle_bytes
                {
                    return Err(Error::InvalidInput(
                        "Sync envelope must be a bounded regular file".into(),
                    ));
                }
                let bytes = std::fs::read(envelope)?;
                if bytes.len() as u64 > config.federation.limits.max_bundle_bytes {
                    return Err(Error::InvalidInput(
                        "Sync envelope size limit exceeded".into(),
                    ));
                }
                let signed: SyncEnvelope = serde_json::from_slice(&bytes)?;
                let local = store.hardknock_node()?.ok_or_else(|| {
                    Error::InvalidInput(
                        "Initialize the local node with `hardknock node show`".into(),
                    )
                })?;
                if signed.sender != local.id {
                    return Err(Error::InvalidInput(
                        "Only locally signed envelopes may be pushed".into(),
                    ));
                }
                if signed.key_revision != local.key_revision {
                    return Err(Error::InvalidInput(
                        "Sync envelope key revision differs from local node".into(),
                    ));
                }
                signed.verify(&local.identity.public_key)?;
                let transport =
                    FilesystemSyncTransport::new(config.federation.limits.max_bundle_bytes)?;
                json!({"kind":"sync_push","receipt":transport.push(&configured,&signed)?})
            }
            SyncCommand::Status => {
                json!({"kind":"sync_status","node":store.hardknock_node()?,"peers":store.sync_peers()?,"remote":store.remote_knowledge_records()?,"metrics":store.sync_metrics()?})
            }
            SyncCommand::History => {
                json!({"kind":"sync_history","sessions":store.sync_sessions()?})
            }
            SyncCommand::Inspect { session } => {
                json!({"kind":"sync_session","session":store.sync_session(session)?})
            }
        },
        Commands::Peer { command } => match command {
            PeerCommand::List => json!({"kind":"peers","peers":store.peers()?}),
            PeerCommand::Add {
                name,
                public_key,
                sync_dir,
            } => {
                let key = read_public_key(public_key)?;
                let node = node_id(key.as_bytes())?;
                let directory = sync_dir
                    .as_ref()
                    .map(|path| {
                        path.to_str().map(str::to_owned).ok_or_else(|| {
                            Error::InvalidInput("Sync directory must be valid UTF-8".into())
                        })
                    })
                    .transpose()?;
                let peer = store.add_peer(name, &public_key_hex(&key), &node)?;
                if let Some(directory) = directory {
                    store.save_sync_peer(&SyncPeer {
                        node,
                        name: name.clone(),
                        public_key: public_key_hex(&key),
                        endpoint: PeerEndpoint::Filesystem(directory),
                        trust: PeerTrust::from(peer.trust),
                        trust_policy: PeerTrustPolicy::default(),
                        filters: SyncFilter::default(),
                        status: PeerStatus::Active,
                        last_sync: None,
                    })?;
                }
                json!({"kind":"peer","peer":peer})
            }
            PeerCommand::Show { peer } => json!({"kind":"peer","peer":store.peer(peer)?}),
            PeerCommand::Trust { peer } => {
                let updated = store.set_peer_trust(peer, ProducerTrust::Trusted)?;
                if let Some(mut sync_peer) = store
                    .sync_peers()?
                    .into_iter()
                    .find(|candidate| candidate.node == updated.node_id)
                {
                    sync_peer.trust = PeerTrust::from(updated.trust);
                    sync_peer.status = PeerStatus::Active;
                    store.save_sync_peer(&sync_peer)?;
                }
                json!({"kind":"peer","peer":updated})
            }
            PeerCommand::Block { peer } => {
                let updated = store.set_peer_trust(peer, ProducerTrust::Blocked)?;
                if let Some(mut sync_peer) = store
                    .sync_peers()?
                    .into_iter()
                    .find(|candidate| candidate.node == updated.node_id)
                {
                    sync_peer.trust = PeerTrust::Blocked;
                    sync_peer.status = PeerStatus::Blocked;
                    store.save_sync_peer(&sync_peer)?;
                }
                json!({"kind":"peer","peer":updated})
            }
            PeerCommand::Remove { peer } => {
                let removed = store.remove_peer(peer)?;
                store.remove_sync_peer(&removed.node_id)?;
                json!({"kind":"peer_removed","peer":removed})
            }
        },
        Commands::Federate { command } => match command {
            FederateCommand::Status => {
                service.identity()?;
                json!({"kind":"federation_status","status":store.federation_status()?,"distributed":{"node":store.hardknock_node()?,"remote":store.remote_knowledge_records()?,"metrics":store.sync_metrics()?}})
            }
            FederateCommand::Export(args) => {
                let signed = bundle(&service, args)?;
                if args.dry_run {
                    json!({"kind":"federation_dry_run","published":false,"bundle":signed,"included":["normalized evidence","context markers","evaluation summaries","hashes","provenance"],"redacted":["absolute home paths","repository path","authorization headers","secret values","raw stdout","environment secrets"],"artifacts":"none"})
                } else {
                    let output = args.output.as_ref().ok_or_else(|| {
                        Error::InvalidInput("--output is required unless --dry-run".into())
                    })?;
                    let path = service.write_bundle(&signed, output)?;
                    json!({"kind":"federation_export","bundle_id":signed.manifest.bundle_id,"path":path,"signed_by":signed.signer,"objects":signed.bundle.object_count()})
                }
            }
            FederateCommand::Import { bundle: path } => {
                let (_, query) = context(cli)?;
                let signed = service.read_bundle(path)?;
                json!({"kind":"federation_import","report":service.import(signed,&query)?})
            }
            FederateCommand::Test { id, checks } => {
                warning(cli.json)?;
                let (state, _) = context(cli)?;
                json!({"kind":"federation_reproduction","reproduction":service.reproduce(id,state,checks.clone(),cancel).await?,"object":store.federated_object(id)?})
            }
            FederateCommand::Promote { id, experience } => {
                json!({"kind":"federation_promotion","object":service.promote(id,experience)?})
            }
            FederateCommand::Publish {
                object,
                target,
                dry_run,
                namespace,
            } => {
                let signed = publish_bundle(&service, object, namespace.as_deref())?;
                if *dry_run {
                    json!({"kind":"federation_dry_run","published":false,"destination":target,"bundle":signed,"artifacts":"none","redacted":["absolute paths","secrets","raw output"]})
                } else {
                    let path = service.publish(&signed, target)?;
                    json!({"kind":"federation_publish","bundle_id":signed.manifest.bundle_id,"path":path})
                }
            }
            FederateCommand::Search {
                producer,
                task_family,
                marker,
                label,
                repository,
                kind,
            } => {
                if let Some(root) = repository {
                    let transport =
                        FilesystemTransport::new(root, config.federation.limits.max_bundle_bytes)?;
                    json!({"kind":"federation_repository_search","results":transport.search(&FederationSelector{producer:producer.clone(),task_family:task_family.clone(),marker:marker.clone(),label:label.clone()})?})
                } else {
                    json!({"kind":"federated_search","results":store.search_federated(kind.as_deref(),marker.as_deref())?})
                }
            }
            FederateCommand::Backlog => {
                json!({"kind":"federation_backlog","items":store.federated_objects()?.into_iter().filter(|o|matches!(o.state,FederatedExperienceState::ContextMatched|FederatedExperienceState::ReproductionRecommended)).collect::<Vec<_>>(),"remote":store.reproduction_queue()?})
            }
            FederateCommand::Audit => {
                json!({"kind":"federation_audit","events":store.federation_audit()?,"sync_events":store.sync_events()?})
            }
            FederateCommand::Reproduce {
                artifact_hash,
                relevance,
            } => {
                let record = store
                    .remote_knowledge_records()?
                    .into_iter()
                    .find(|record| record.remote_artifact.content_hash == *artifact_hash)
                    .ok_or_else(|| Error::NotFound(artifact_hash.clone()))?;
                let node = store.hardknock_node()?.ok_or_else(|| {
                    Error::InvalidInput(
                        "Initialize the local node with `hardknock node show`".into(),
                    )
                })?;
                json!({"kind":"remote_reproduction_queued","item":store.enqueue_remote_reproduction(&record,node.environment,None,relevance.clone())?})
            }
            FederateCommand::Compare { left, right } => {
                let a = store.federated_object(left)?;
                let b = store.federated_object(right)?;
                json!({"kind":"federation_compare","left":a,"right":b,"same_lineage":a.identity.lineage_hash==b.identity.lineage_hash,"context_score_delta":a.trust.context_compatibility.score-b.trust.context_compatibility.score,"policy":"Retain team-specific evidence; never average operating envelopes or vote on claims"})
            }
        },
        Commands::Provenance { object } => {
            json!({"kind":"provenance","graph":store.provenance_graph(object)?})
        }
        Commands::Conflict { command } => match command {
            ConflictCommand::List => {
                json!({"kind":"conflicts","conflicts":store.federated_conflicts()?})
            }
            ConflictCommand::Show { id } => {
                json!({"kind":"conflict","conflict":store.federated_conflict(id)?})
            }
            ConflictCommand::Test { id, checks } => {
                let conflict = store.federated_conflict(id)?;
                warning(cli.json)?;
                let (state, _) = context(cli)?;
                json!({"kind":"conflict_test","conflict":conflict,"reproduction":service.reproduce(&conflict.external_object,state,checks.clone(),cancel).await?})
            }
        },
        _ => return Err(Error::InvalidInput("Federation dispatch failed".into())),
    })
}

pub fn print(value: &Value, out: &mut impl Write) -> Result<()> {
    match value["kind"].as_str() {
        Some("federation_import") => {
            let r = &value["report"];
            writeln!(
                out,
                "Bundle {}",
                r["bundle_id"].as_str().unwrap_or("unknown")
            )?;
            writeln!(
                out,
                "Producer  {}",
                r["producer"].as_str().unwrap_or("unknown")
            )?;
            writeln!(out, "Signature {:?}", r["authenticity"])?;
            writeln!(
                out,
                "Local status  {}",
                r["state"].as_str().unwrap_or("advisory")
            )?;
            if let Some(action) = r["recommended_action"].as_str() {
                writeln!(out, "Recommended next action\n  {action}")?
            }
        }
        Some("federation_reproduction") | Some("conflict_test") => {
            writeln!(out, "{}", serde_json::to_string_pretty(value)?)?
        }
        Some("federation_dry_run") => writeln!(
            out,
            "Federation dry run\n\nNo data published (--dry-run)\n{}",
            serde_json::to_string_pretty(value)?
        )?,
        _ => writeln!(out, "{}", serde_json::to_string_pretty(value)?)?,
    }
    Ok(())
}
