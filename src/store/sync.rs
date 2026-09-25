// SPDX-License-Identifier: Apache-2.0

use super::EpistemicStore;
use super::Store;
use crate::{
    Error, Result,
    core::*,
    epistemic::{EvidenceOutcome, EvidenceSource},
    federation::*,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

fn event(
    connection: &rusqlite::Connection,
    kind: &str,
    subject: Option<&str>,
    detail: serde_json::Value,
) -> Result<()> {
    connection.execute(
        "INSERT INTO sync_events(event,subject,created_at,data) VALUES(?1,?2,?3,?4)",
        params![kind, subject, Utc::now().to_rfc3339(), detail.to_string()],
    )?;
    Ok(())
}

fn state_for(
    artifact: &SyncArtifact,
    compatibility: EnvironmentCompatibilityStatus,
    peer: &SyncPeer,
) -> RemoteArtifactState {
    if compatibility == EnvironmentCompatibilityStatus::Incompatible {
        return RemoteArtifactState::Quarantined;
    }
    if !peer.trust_policy.allow_advisory_retrieval {
        return RemoteArtifactState::Quarantined;
    }
    if artifact.critical
        || matches!(
            artifact.artifact_type,
            SyncArtifactType::Constraint | SyncArtifactType::KnowledgeException
        )
    {
        RemoteArtifactState::ReproductionRequired
    } else {
        RemoteArtifactState::Advisory
    }
}

fn persist_revocation(
    connection: &rusqlite::Connection,
    revocation: &ArtifactRevocation,
) -> Result<Option<RemoteKnowledgeRecord>> {
    let previous: Option<String> = connection
        .query_row(
            "SELECT data FROM artifact_revocations WHERE origin_node=?1 AND artifact_hash=?2",
            params![
                revocation.origin.to_string(),
                revocation.artifact.content_hash
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let stored: ArtifactRevocation = serde_json::from_str(&previous)?;
        if stored.id != revocation.id || stored.signature != revocation.signature {
            return Err(Error::InvalidInput(
                "Conflicting revocation for the same origin artifact".into(),
            ));
        }
    } else {
        connection.execute(
            "INSERT INTO artifact_revocations(id,origin_node,artifact_hash,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![
                revocation.id.to_string(),
                revocation.origin.to_string(),
                revocation.artifact.content_hash,
                revocation.created_at.to_rfc3339(),
                serde_json::to_string(revocation)?
            ],
        )?;
    }

    let target: Option<String> = connection
        .query_row(
            "SELECT data FROM remote_knowledge_records WHERE json_extract(data,'$.remote_artifact.content_hash')=?1",
            [&revocation.artifact.content_hash],
            |row| row.get(0),
        )
        .optional()?;
    let Some(target) = target else {
        event(
            connection,
            "artifact_revocation_pending",
            Some(&revocation.id.to_string()),
            serde_json::json!({"target":revocation.artifact}),
        )?;
        return Ok(None);
    };
    let mut record: RemoteKnowledgeRecord = serde_json::from_str(&target)?;
    if record.remote_artifact != revocation.artifact {
        return Err(Error::InvalidInput(
            "Revocation target does not match stored artifact identity".into(),
        ));
    }
    let has_local_support = !record.local_evidence.is_empty();
    record.local_state = if has_local_support {
        RemoteArtifactState::LocallySupported
    } else {
        RemoteArtifactState::Revoked
    };
    record.review_required = has_local_support;
    record.origin_revoked = true;
    connection.execute(
        "UPDATE remote_knowledge_records SET local_state=?2,updated_at=?3,data=?4 WHERE json_extract(data,'$.remote_artifact.content_hash')=?1",
        params![
            revocation.artifact.content_hash,
            serde_json::to_value(record.local_state)?.as_str(),
            Utc::now().to_rfc3339(),
            serde_json::to_string(&record)?
        ],
    )?;
    event(
        connection,
        "artifact_revoked",
        Some(&revocation.id.to_string()),
        serde_json::json!({"origin":revocation.origin,"local_support_preserved":has_local_support}),
    )?;
    let target_artifact: String = connection.query_row(
        "SELECT data FROM sync_artifacts WHERE content_hash=?1 AND origin_node=?2",
        params![
            revocation.artifact.content_hash,
            revocation.origin.to_string()
        ],
        |row| row.get(0),
    )?;
    let target_artifact: SyncArtifact = serde_json::from_str(&target_artifact)?;
    if target_artifact.artifact_type == SyncArtifactType::Lesson
        && let Some(payload) = target_artifact.payload
        && let Ok(signed) = serde_json::from_value::<SignedExperienceBundle>(payload)
    {
        let bundle_id = signed.manifest.bundle_id.to_string();
        let mut statement = connection.prepare(
            "SELECT data FROM federated_objects WHERE origin_bundle=?1 AND origin_node=?2",
        )?;
        let objects = statement
            .query_map(params![bundle_id, revocation.origin.to_string()], |row| {
                row.get::<_, String>(0)
            })?;
        let objects = objects.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for data in objects {
            let mut object: FederatedObject = serde_json::from_str(&data)?;
            object.origin_revoked = true;
            if !matches!(
                object.state,
                FederatedExperienceState::LocallySupported
                    | FederatedExperienceState::LocallyValidated
            ) {
                object.state = FederatedExperienceState::Retired;
            }
            connection.execute(
                "UPDATE federated_objects SET state=?2,data=?3 WHERE id=?1",
                params![
                    object.id.to_string(),
                    serde_json::to_value(object.state)?.as_str(),
                    serde_json::to_string(&object)?,
                ],
            )?;
            event(
                connection,
                "federated_object_origin_revoked",
                Some(&object.id.to_string()),
                serde_json::json!({"revocation":revocation.id,"local_evidence_preserved":matches!(object.state,FederatedExperienceState::LocallySupported | FederatedExperienceState::LocallyValidated)}),
            )?;
        }
    }
    Ok(Some(record))
}

impl Store {
    pub fn initialize_hardknock_node(
        &self,
        identity: &NodeIdentity,
        environment: EnvironmentIdentity,
    ) -> Result<HardknockNode> {
        let node = HardknockNode {
            id: identity.node.id.clone(),
            identity: identity.node.public_identity.clone(),
            label: Some(identity.node.name.clone()),
            key_revision: 1,
            environment,
            capabilities: identity.node.capabilities.clone(),
            trust_policy: NodeTrustPolicy::default(),
            sync_policy: NodeSyncPolicy::default(),
            created_at: identity.node.created_at,
        };
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM hardknock_nodes WHERE id=?1",
                [node.id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(data) = existing {
            return Ok(serde_json::from_str(&data)?);
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO hardknock_nodes(id,key_revision,created_at,data) VALUES(?1,1,?2,?3)",
            params![
                node.id.to_string(),
                node.created_at.to_rfc3339(),
                serde_json::to_string(&node)?
            ],
        )?;
        let key = NodeKeyRevision {
            node: node.id.clone(),
            revision: 1,
            public_key: node.identity.public_key.clone(),
            valid_from: node.created_at,
            valid_until: None,
            signed_by_previous_key: None,
        };
        tx.execute(
            "INSERT INTO node_key_revisions(node_id,revision,valid_from,data) VALUES(?1,1,?2,?3)",
            params![
                node.id.to_string(),
                node.created_at.to_rfc3339(),
                serde_json::to_string(&key)?
            ],
        )?;
        event(
            &tx,
            "node_initialized",
            Some(&node.id.to_string()),
            serde_json::json!({"key_revision":1}),
        )?;
        tx.commit()?;
        Ok(node)
    }

    pub fn hardknock_node(&self) -> Result<Option<HardknockNode>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM hardknock_nodes ORDER BY created_at,id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn save_sync_peer(&self, peer: &SyncPeer) -> Result<()> {
        if peer.name.trim().is_empty() || peer.name.len() > 120 {
            return Err(Error::InvalidInput("Invalid sync peer name".into()));
        }
        let key = parse_public_key(&peer.public_key)?;
        if node_id(key.as_bytes())? != peer.node {
            return Err(Error::InvalidInput(
                "Sync peer public key does not identify declared node".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO sync_peers(node_id,name,trust,status,data) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(node_id) DO UPDATE SET name=excluded.name,trust=excluded.trust,status=excluded.status,data=excluded.data",
            params![
                peer.node.to_string(),
                peer.name,
                serde_json::to_value(peer.trust)?.as_str(),
                serde_json::to_value(peer.status)?.as_str(),
                serde_json::to_string(peer)?
            ],
        )?;
        event(
            &self.connection,
            "sync_peer_configured",
            Some(&peer.node.to_string()),
            serde_json::json!({"trust":peer.trust}),
        )
    }

    pub fn sync_peers(&self) -> Result<Vec<SyncPeer>> {
        self.list("SELECT data FROM sync_peers ORDER BY name,node_id")
    }

    pub fn remove_sync_peer(&self, node: &NodeId) -> Result<()> {
        let changed = self.connection.execute(
            "DELETE FROM sync_peers WHERE node_id=?1",
            [node.to_string()],
        )?;
        if changed != 0 {
            event(
                &self.connection,
                "sync_peer_removed",
                Some(&node.to_string()),
                serde_json::json!({}),
            )?;
        }
        Ok(())
    }

    pub fn sync_peer(&self, selector: &str) -> Result<SyncPeer> {
        self.get(
            "SELECT data FROM sync_peers WHERE node_id=?1 OR name=?1",
            selector,
        )
    }

    pub fn save_sync_cursor(&self, cursor: &SyncCursor) -> Result<()> {
        self.connection.execute(
            "INSERT INTO sync_cursors(peer_id,stream,position,updated_at,data) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(peer_id,stream) DO UPDATE SET position=excluded.position,updated_at=excluded.updated_at,data=excluded.data",
            params![
                cursor.peer.to_string(),
                serde_json::to_value(cursor.stream)?.as_str(),
                cursor.position,
                cursor.updated_at.to_rfc3339(),
                serde_json::to_string(cursor)?
            ],
        )?;
        Ok(())
    }

    pub fn sync_cursor(&self, peer: &NodeId, stream: SyncStreamKind) -> Result<Option<SyncCursor>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM sync_cursors WHERE peer_id=?1 AND stream=?2",
                params![peer.to_string(), serde_json::to_value(stream)?.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn receive_sync_envelope(
        &self,
        envelope: &SyncEnvelope,
        local_environment: &EnvironmentIdentity,
    ) -> Result<SyncSession> {
        let peer = self.sync_peer(&envelope.sender.to_string())?;
        if peer.trust == PeerTrust::Blocked || peer.status == PeerStatus::Blocked {
            return Err(Error::Intervention("Blocked sync peer".into()));
        }
        if !peer.trust_policy.accept_signed_artifacts {
            return Err(Error::Intervention(
                "Peer policy rejects signed artifacts".into(),
            ));
        }
        envelope.verify(&peer.public_key)?;
        if envelope.key_revision != 1 {
            return Err(Error::InvalidInput(
                "Unconfigured sync key revision; key rotation is not active".into(),
            ));
        }
        let now = Utc::now();
        let replay: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM sync_envelopes WHERE id=?1 OR (sender=?2 AND nonce=?3)",
                params![
                    envelope.id.to_string(),
                    envelope.sender.to_string(),
                    envelope.nonce
                ],
                |row| row.get(0),
            )
            .optional()?;
        if replay.is_some() {
            let session = SyncSession {
                id: SyncSessionId::new(),
                peer: envelope.sender.clone(),
                direction: SyncDirection::Pull,
                started_at: now,
                completed_at: Some(now),
                received: envelope.artifacts.len(),
                accepted: 0,
                quarantined: 0,
                rejected: 0,
                deduplicated: envelope.artifacts.len(),
                status: SyncSessionStatus::ReplayRejected,
                reasons: vec!["duplicate envelope id or sender nonce".into()],
            };
            self.save_sync_session(&session)?;
            return Ok(session);
        }

        let mut session = SyncSession {
            id: SyncSessionId::new(),
            peer: envelope.sender.clone(),
            direction: SyncDirection::Pull,
            started_at: now,
            completed_at: None,
            received: envelope.artifacts.len(),
            accepted: 0,
            quarantined: 0,
            rejected: 0,
            deduplicated: 0,
            status: SyncSessionStatus::Running,
            reasons: Vec::new(),
        };
        // Relay signatures authenticate delivery. Only an independent signature
        // from a configured, trusted origin authenticates the artifact itself.
        let known_origins = self.sync_peers()?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO sync_envelopes(id,sender,nonce,created_at,received_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                envelope.id.to_string(),
                envelope.sender.to_string(),
                envelope.nonce,
                envelope.created_at.to_rfc3339(),
                now.to_rfc3339(),
                serde_json::to_string(envelope)?
            ],
        )?;
        event(
            &tx,
            "sync_started",
            Some(&session.id.to_string()),
            serde_json::json!({"peer":envelope.sender,"received":envelope.artifacts.len()}),
        )?;

        for artifact in &envelope.artifacts {
            let filtered = (!peer.filters.artifact_types.is_empty()
                && !peer
                    .filters
                    .artifact_types
                    .contains(&artifact.artifact_type)
                && !(peer.filters.include_revocations
                    && artifact.artifact_type == SyncArtifactType::Revocation))
                || (!peer.filters.task_families.is_empty()
                    && artifact
                        .task_family
                        .as_ref()
                        .is_none_or(|family| !peer.filters.task_families.contains(family)))
                || (!peer.filters.environments.is_empty()
                    && !peer
                        .filters
                        .environments
                        .contains(&artifact.environment.kind));
            if filtered || artifact.artifact_ref.schema_version != SYNC_ARTIFACT_SCHEMA_V1 {
                session.rejected += 1;
                session.reasons.push(if filtered {
                    format!("{} excluded by peer filter", artifact.id)
                } else {
                    format!("{} uses unsupported artifact schema", artifact.id)
                });
                event(
                    &tx,
                    "remote_artifact_rejected",
                    Some(&artifact.id.to_string()),
                    serde_json::json!({"filtered":filtered}),
                )?;
                continue;
            }
            artifact.verify_hash()?;
            let origin_authenticated = if artifact.origin == envelope.sender {
                if artifact.origin_signature.is_some() {
                    artifact.verify_origin(&peer.public_key)?;
                }
                true
            } else {
                known_origins.iter().any(|origin| {
                    origin.node == artifact.origin
                        && origin.status == PeerStatus::Active
                        && origin.trust == PeerTrust::TrustedForAdvisoryEvidence
                        && artifact.verify_origin(&origin.public_key).is_ok()
                })
            };
            let revocation =
                if artifact.artifact_type == SyncArtifactType::Revocation && origin_authenticated {
                    let payload = artifact.payload.clone().ok_or_else(|| {
                        Error::InvalidInput("Signed revocation has no payload".into())
                    })?;
                    let revocation: ArtifactRevocation = serde_json::from_value(payload)?;
                    if revocation.origin != artifact.origin
                        || artifact.artifact_ref.artifact_id != revocation.id.to_string()
                    {
                        return Err(Error::InvalidInput(
                            "Revocation payload does not match artifact origin or identity".into(),
                        ));
                    }
                    let origin_key = if artifact.origin == envelope.sender {
                        &peer.public_key
                    } else {
                        &known_origins
                            .iter()
                            .find(|origin| origin.node == artifact.origin)
                            .ok_or_else(|| Error::InvalidInput("Unknown revocation origin".into()))?
                            .public_key
                    };
                    revocation.verify(origin_key)?;
                    Some(revocation)
                } else {
                    None
                };
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM sync_artifacts WHERE root_node=?1 AND root_artifact=?2 AND root_revision=?3 AND content_hash=?4",
                    params![
                        artifact.root_origin.node.to_string(),
                        artifact.root_origin.artifact_id,
                        artifact.root_origin.revision as i64,
                        artifact.content_hash
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            let stored_id = if let Some(id) = existing {
                session.deduplicated += 1;
                if origin_authenticated {
                    let record_data: Option<String> = tx
                        .query_row(
                            "SELECT data FROM remote_knowledge_records WHERE artifact_id=?1",
                            [&id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(data) = record_data {
                        let mut record: RemoteKnowledgeRecord = serde_json::from_str(&data)?;
                        if record.local_state == RemoteArtifactState::Quarantined
                            && !record.origin_revoked
                        {
                            let next = state_for(artifact, record.compatibility.status, &peer);
                            if next != RemoteArtifactState::Quarantined {
                                record.local_state = next;
                                record.trust = peer.trust;
                                record.review_required = peer.trust_policy.require_manual_review;
                                tx.execute(
                                    "UPDATE remote_knowledge_records SET local_state=?2,updated_at=?3,data=?4 WHERE artifact_id=?1",
                                    params![
                                        id,
                                        serde_json::to_value(next)?.as_str(),
                                        now.to_rfc3339(),
                                        serde_json::to_string(&record)?
                                    ],
                                )?;
                                session.accepted += 1;
                                event(
                                    &tx,
                                    "remote_artifact_verified",
                                    Some(&id),
                                    serde_json::json!({"state":next,"deduplicated":true}),
                                )?;
                            }
                        }
                    }
                }
                id
            } else {
                tx.execute(
                    "INSERT INTO sync_artifacts(id,origin_node,origin_artifact,revision,root_node,root_artifact,root_revision,content_hash,artifact_type,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        artifact.id.to_string(),
                        artifact.origin.to_string(),
                        artifact.artifact_ref.artifact_id,
                        artifact.lineage.revision as i64,
                        artifact.root_origin.node.to_string(),
                        artifact.root_origin.artifact_id,
                        artifact.root_origin.revision as i64,
                        artifact.content_hash,
                        serde_json::to_value(&artifact.artifact_type)?.as_str(),
                        artifact.created_at.to_rfc3339(),
                        serde_json::to_string(artifact)?
                    ],
                )?;
                let compatibility =
                    assess_environment_compatibility(&artifact.environment, local_environment);
                let local_state = if !origin_authenticated {
                    RemoteArtifactState::Quarantined
                } else {
                    state_for(artifact, compatibility.status, &peer)
                };
                if local_state == RemoteArtifactState::Quarantined {
                    session.quarantined += 1;
                } else {
                    session.accepted += 1;
                }
                let record = RemoteKnowledgeRecord {
                    remote_artifact: artifact.reference(),
                    artifact_type: artifact.artifact_type.clone(),
                    origin_status: artifact.origin_maturity,
                    local_state,
                    compatibility,
                    trust: peer.trust,
                    local_evidence: Vec::new(),
                    availability: artifact.availability,
                    reproducibility: artifact.reproducibility,
                    freshness: RemoteFreshness {
                        artifact_created_at: artifact.created_at,
                        received_at: now,
                        origin_last_seen: Some(envelope.created_at),
                        stale: false,
                    },
                    review_required: peer.trust_policy.require_manual_review,
                    origin_certified: artifact.artifact_type == SyncArtifactType::Certification,
                    locally_certified: false,
                    origin_revoked: false,
                };
                tx.execute(
                    "INSERT INTO remote_knowledge_records(artifact_id,local_state,received_at,updated_at,data) VALUES(?1,?2,?3,?3,?4)",
                    params![
                        artifact.id.to_string(),
                        serde_json::to_value(local_state)?.as_str(),
                        now.to_rfc3339(),
                        serde_json::to_string(&record)?
                    ],
                )?;
                event(
                    &tx,
                    if local_state == RemoteArtifactState::Quarantined {
                        "remote_artifact_quarantined"
                    } else {
                        "remote_artifact_verified"
                    },
                    Some(&artifact.id.to_string()),
                    serde_json::json!({"state":local_state}),
                )?;
                artifact.id.to_string()
            };
            tx.execute(
                "INSERT OR IGNORE INTO sync_artifact_receipts(artifact_id,envelope_id,received_from,received_at,relay_path) VALUES(?1,?2,?3,?4,?5)",
                params![
                    stored_id,
                    envelope.id.to_string(),
                    envelope.sender.to_string(),
                    now.to_rfc3339(),
                    serde_json::to_string(&artifact.relay_nodes)?
                ],
            )?;
            if let Some(revocation) = &revocation {
                persist_revocation(&tx, revocation)?;
            } else if artifact.artifact_type != SyncArtifactType::Revocation {
                let pending: Option<String> = tx
                    .query_row(
                        "SELECT data FROM artifact_revocations WHERE origin_node=?1 AND artifact_hash=?2",
                        params![artifact.origin.to_string(), artifact.content_hash],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(pending) = pending {
                    let pending: ArtifactRevocation = serde_json::from_str(&pending)?;
                    persist_revocation(&tx, &pending)?;
                }
            }
        }
        session.status = SyncSessionStatus::Completed;
        session.completed_at = Some(Utc::now());
        tx.execute(
            "INSERT INTO sync_sessions(id,peer_id,direction,status,started_at,completed_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                session.id.to_string(),
                session.peer.to_string(),
                serde_json::to_value(session.direction)?.as_str(),
                serde_json::to_value(session.status)?.as_str(),
                session.started_at.to_rfc3339(),
                session.completed_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(&session)?
            ],
        )?;
        event(
            &tx,
            "sync_completed",
            Some(&session.id.to_string()),
            serde_json::to_value(&session)?,
        )?;
        tx.commit()?;
        Ok(session)
    }

    fn save_sync_session(&self, session: &SyncSession) -> Result<()> {
        self.connection.execute(
            "INSERT INTO sync_sessions(id,peer_id,direction,status,started_at,completed_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                session.id.to_string(),
                session.peer.to_string(),
                serde_json::to_value(session.direction)?.as_str(),
                serde_json::to_value(session.status)?.as_str(),
                session.started_at.to_rfc3339(),
                session.completed_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(session)?
            ],
        )?;
        Ok(())
    }

    pub fn record_sync_publication(
        &self,
        peer: &NodeId,
        envelope: &SyncEnvelope,
        managed_lesson: bool,
    ) -> Result<SyncSession> {
        let now = Utc::now();
        self.connection.execute(
            "INSERT OR IGNORE INTO sync_envelopes(id,sender,nonce,created_at,received_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                envelope.id.to_string(),
                envelope.sender.to_string(),
                envelope.nonce,
                envelope.created_at.to_rfc3339(),
                now.to_rfc3339(),
                serde_json::to_string(envelope)?
            ],
        )?;
        let session = SyncSession {
            id: SyncSessionId::new(),
            peer: peer.clone(),
            direction: SyncDirection::Push,
            started_at: now,
            completed_at: Some(now),
            received: 0,
            accepted: envelope.artifacts.len(),
            quarantined: 0,
            rejected: 0,
            deduplicated: 0,
            status: SyncSessionStatus::Completed,
            reasons: Vec::new(),
        };
        self.save_sync_session(&session)?;
        event(
            &self.connection,
            if managed_lesson {
                "sync_lesson_published"
            } else {
                "sync_published"
            },
            Some(&session.id.to_string()),
            serde_json::json!({"peer":peer,"envelope":envelope.id,"artifacts":envelope.artifacts.len()}),
        )?;
        Ok(session)
    }

    pub fn published_lesson_sync_artifact(
        &self,
        sender: &NodeId,
        content_hash: &str,
    ) -> Result<Option<SyncArtifact>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM sync_envelopes
             WHERE sender=?1 AND json_array_length(data,'$.artifacts')=1
               AND json_extract(data,'$.artifacts[0].content_hash')=?2
               AND EXISTS (SELECT 1 FROM sync_events
                           WHERE event='sync_lesson_published'
                             AND json_extract(sync_events.data,'$.envelope')=sync_envelopes.id)
             ORDER BY created_at,id LIMIT 1",
                params![sender.to_string(), content_hash],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| {
            let envelope: SyncEnvelope = serde_json::from_str(&value)?;
            Ok(envelope
                .artifacts
                .into_iter()
                .next()
                .expect("single artifact"))
        })
        .transpose()
    }

    pub fn published_revocation_envelope(
        &self,
        sender: &NodeId,
        target_hash: &str,
    ) -> Result<Option<SyncEnvelope>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM sync_envelopes
             WHERE sender=?1 AND json_extract(data,'$.artifacts[0].artifact_type')='revocation'
               AND EXISTS (SELECT 1 FROM sync_events
                           WHERE event='sync_revocation_published'
                             AND json_extract(sync_events.data,'$.envelope')=sync_envelopes.id
                             AND json_extract(sync_events.data,'$.target_hash')=?2)
             ORDER BY created_at,id LIMIT 1",
                params![sender.to_string(), target_hash],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn record_sync_revocation_publication(
        &self,
        peer: &NodeId,
        envelope: &SyncEnvelope,
        target_hash: &str,
    ) -> Result<SyncSession> {
        let session = self.record_sync_publication(peer, envelope, false)?;
        event(
            &self.connection,
            "sync_revocation_published",
            Some(&session.id.to_string()),
            serde_json::json!({"peer":peer,"envelope":envelope.id,"target_hash":target_hash}),
        )?;
        Ok(session)
    }

    pub fn published_sync_envelope(
        &self,
        sender: &NodeId,
        artifact_id: &str,
        revision: u64,
    ) -> Result<Option<SyncEnvelope>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM sync_envelopes
                 WHERE sender=?1 AND json_array_length(data,'$.artifacts')=1
                   AND json_extract(data,'$.artifacts[0].artifact_ref.artifact_id')=?2
                   AND json_extract(data,'$.artifacts[0].lineage.revision')=?3
                   AND EXISTS (SELECT 1 FROM sync_events
                               WHERE event='sync_lesson_published'
                                 AND json_extract(sync_events.data,'$.envelope')=sync_envelopes.id)
                 ORDER BY created_at,id LIMIT 1",
                params![sender.to_string(), artifact_id, revision as i64],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn sync_sessions(&self) -> Result<Vec<SyncSession>> {
        self.list("SELECT data FROM sync_sessions ORDER BY started_at DESC,id")
    }

    pub fn sync_session(&self, id: &SyncSessionId) -> Result<SyncSession> {
        self.get(
            "SELECT data FROM sync_sessions WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn sync_artifacts(&self) -> Result<Vec<SyncArtifact>> {
        self.list("SELECT data FROM sync_artifacts ORDER BY created_at,id")
    }

    pub fn remote_knowledge_records(&self) -> Result<Vec<RemoteKnowledgeRecord>> {
        self.list("SELECT data FROM remote_knowledge_records ORDER BY received_at,artifact_id")
    }

    pub fn advisory_remote_knowledge(&self) -> Result<Vec<RemoteKnowledgeRecord>> {
        Ok(self
            .remote_knowledge_records()?
            .into_iter()
            .filter(|record| {
                record.artifact_type != SyncArtifactType::Revocation
                    && matches!(
                        record.local_state,
                        RemoteArtifactState::Advisory
                            | RemoteArtifactState::ReproductionRequired
                            | RemoteArtifactState::LocallySupported
                    )
                    && !record.origin_revoked
            })
            .collect())
    }

    pub fn save_remote_contradiction(&self, contradiction: &RemoteContradiction) -> Result<String> {
        let data = serde_json::to_vec(contradiction)?;
        let id = format!("remote-contradiction:{}", blake3::hash(&data).to_hex());
        self.connection.execute(
            "INSERT OR IGNORE INTO remote_contradictions(id,target_hash,received_at,data) VALUES(?1,?2,?3,?4)",
            params![
                id,
                contradiction.target.content_hash,
                contradiction.received_at.to_rfc3339(),
                serde_json::to_string(contradiction)?
            ],
        )?;
        event(
            &self.connection,
            "remote_contradiction_received",
            Some(&id),
            serde_json::json!({"target":contradiction.target}),
        )?;
        Ok(id)
    }

    pub fn remote_knowledge_record(
        &self,
        artifact: &SyncArtifactRef,
    ) -> Result<RemoteKnowledgeRecord> {
        self.get(
            "SELECT data FROM remote_knowledge_records WHERE json_extract(data,'$.remote_artifact.content_hash')=?1",
            &artifact.content_hash,
        )
    }

    pub fn support_remote_artifact(
        &self,
        artifact: &SyncArtifactRef,
        local_evidence: Vec<EvidencePathId>,
    ) -> Result<RemoteKnowledgeRecord> {
        if local_evidence.is_empty() {
            return Err(Error::InvalidInput(
                "Local promotion requires independent local evidence".into(),
            ));
        }
        let mut record = self.remote_knowledge_record(artifact)?;
        if record.origin_revoked
            || matches!(
                record.local_state,
                RemoteArtifactState::Quarantined
                    | RemoteArtifactState::Rejected
                    | RemoteArtifactState::Revoked
            )
        {
            return Err(Error::Intervention(
                "Remote artifact is not eligible for local support".into(),
            ));
        }
        let stored: SyncArtifact = self.get(
            "SELECT data FROM sync_artifacts WHERE content_hash=?1",
            &artifact.content_hash,
        )?;
        if stored.critical
            || matches!(
                stored.artifact_type,
                SyncArtifactType::Constraint | SyncArtifactType::KnowledgeException
            )
        {
            return Err(Error::Intervention(
                "Critical remote support requires a controlled reproduction integration".into(),
            ));
        }
        for evidence_id in &local_evidence {
            let path = EpistemicStore::evidence_path(self, evidence_id)?;
            if path.outcome != EvidenceOutcome::Supports
                || matches!(path.source, EvidenceSource::Federation { .. })
                || path
                    .dependencies
                    .originating_federated_nodes
                    .contains(&artifact.origin)
                || !path.evidence_refs.iter().any(|reference| {
                    reference.kind == "remote_sync_artifact"
                        && reference.id == artifact.content_hash
                })
            {
                return Err(Error::InvalidInput(
                    "Local support requires a stored, independent supporting path bound to this remote artifact".into(),
                ));
            }
        }
        record.local_evidence = local_evidence;
        record.local_state = RemoteArtifactState::LocallySupported;
        record.review_required = false;
        let changed = self.connection.execute(
            "UPDATE remote_knowledge_records SET local_state='locally_supported',updated_at=?2,data=?3 WHERE json_extract(data,'$.remote_artifact.content_hash')=?1",
            params![
                artifact.content_hash,
                Utc::now().to_rfc3339(),
                serde_json::to_string(&record)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(artifact.content_hash.clone()));
        }
        event(
            &self.connection,
            "remote_artifact_locally_supported",
            Some(&artifact.content_hash),
            serde_json::json!({"local_evidence":record.local_evidence}),
        )?;
        Ok(record)
    }

    pub fn enqueue_remote_reproduction(
        &self,
        record: &RemoteKnowledgeRecord,
        target_context: EnvironmentIdentity,
        priority: Option<ExperienceOpportunityId>,
        relevance: String,
    ) -> Result<ReproductionQueueItem> {
        if relevance.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Remote reproduction requires explicit local relevance".into(),
            ));
        }
        let item = ReproductionQueueItem {
            id: ReproductionQueueItemId::new(),
            remote_artifact: record.remote_artifact.clone(),
            target_context,
            priority,
            relevance,
            status: ReproductionQueueStatus::Pending,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO reproduction_queue(id,artifact_hash,status,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![
                item.id.to_string(),
                item.remote_artifact.content_hash,
                serde_json::to_value(item.status)?.as_str(),
                item.created_at.to_rfc3339(),
                serde_json::to_string(&item)?
            ],
        )?;
        event(
            &self.connection,
            "remote_artifact_reproduction_requested",
            Some(&item.id.to_string()),
            serde_json::json!({"artifact":item.remote_artifact}),
        )?;
        Ok(item)
    }

    pub fn reproduction_queue(&self) -> Result<Vec<ReproductionQueueItem>> {
        self.list("SELECT data FROM reproduction_queue ORDER BY created_at,id")
    }

    pub fn apply_artifact_revocation(
        &self,
        revocation: &ArtifactRevocation,
    ) -> Result<RemoteKnowledgeRecord> {
        let peer = self.sync_peer(&revocation.origin.to_string())?;
        revocation.verify(&peer.public_key)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let record = persist_revocation(&tx, revocation)?
            .ok_or_else(|| Error::NotFound(revocation.artifact.content_hash.clone()))?;
        tx.commit()?;
        Ok(record)
    }

    pub fn mark_sync_origin_compromised(&self, origin: &NodeId) -> Result<usize> {
        let mut peer = self.sync_peer(&origin.to_string())?;
        peer.status = PeerStatus::Blocked;
        peer.trust = PeerTrust::Blocked;
        self.save_sync_peer(&peer)?;
        let artifacts: Vec<SyncArtifact> = self
            .sync_artifacts()?
            .into_iter()
            .filter(|artifact| &artifact.origin == origin)
            .collect();
        let mut changed = 0;
        for artifact in artifacts {
            let mut record = self.remote_knowledge_record(&artifact.reference())?;
            record.review_required = true;
            if record.local_evidence.is_empty() {
                record.local_state = RemoteArtifactState::Quarantined;
            }
            changed += self.connection.execute(
                "UPDATE remote_knowledge_records SET local_state=?2,updated_at=?3,data=?4 WHERE json_extract(data,'$.remote_artifact.content_hash')=?1",
                params![
                    artifact.content_hash,
                    serde_json::to_value(record.local_state)?.as_str(),
                    Utc::now().to_rfc3339(),
                    serde_json::to_string(&record)?
                ],
            )?;
        }
        event(
            &self.connection,
            "origin_compromised",
            Some(&origin.to_string()),
            serde_json::json!({"records_marked":changed}),
        )?;
        Ok(changed)
    }

    pub fn save_environment_manifest(
        &self,
        node: &NodeId,
        manifest: &EnvironmentManifest,
    ) -> Result<String> {
        let data = serde_json::to_vec(manifest)?;
        let id = format!("environment-manifest:{}", blake3::hash(&data).to_hex());
        self.connection.execute(
            "INSERT OR IGNORE INTO environment_manifests(id,node_id,captured_at,data) VALUES(?1,?2,?3,?4)",
            params![
                id,
                node.to_string(),
                manifest.captured_at.to_rfc3339(),
                serde_json::to_string(manifest)?
            ],
        )?;
        Ok(id)
    }

    pub fn sync_events(&self) -> Result<Vec<serde_json::Value>> {
        self.list("SELECT json_object('event',event,'subject',subject,'created_at',created_at,'detail',json(data)) FROM sync_events ORDER BY sequence")
    }

    pub fn sync_metrics(&self) -> Result<SyncMetrics> {
        let sessions = self.sync_sessions()?;
        let records = self.remote_knowledge_records()?;
        let events = self.sync_events()?;
        let mut metrics = SyncMetrics::default();
        for session in sessions {
            if session.direction == SyncDirection::Push {
                metrics.artifacts_published += session.accepted as u64;
            } else {
                metrics.artifacts_received += session.received as u64;
                metrics.artifacts_verified += session.accepted as u64;
                metrics.artifacts_rejected += session.rejected as u64;
                metrics.artifacts_quarantined += session.quarantined as u64;
                metrics.artifacts_deduplicated += session.deduplicated as u64;
            }
        }
        metrics.remote_artifacts_promoted = records
            .iter()
            .filter(|record| record.local_state == RemoteArtifactState::LocallySupported)
            .count() as u64;
        metrics.remote_artifacts_reproduced = records
            .iter()
            .filter(|record| !record.local_evidence.is_empty())
            .count() as u64;
        metrics.remote_contradictions_received = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|v| v.as_str()) == Some("remote_contradiction_received")
            })
            .count() as u64;
        metrics.revocations_received =
            self.connection
                .query_row("SELECT COUNT(*) FROM artifact_revocations", [], |row| {
                    row.get::<_, i64>(0)
                })? as u64;
        metrics.blind_critical_promotions = records
            .iter()
            .filter(|record| {
                record.local_state == RemoteArtifactState::LocallySupported
                    && record.local_evidence.is_empty()
                    && matches!(record.artifact_type, SyncArtifactType::Constraint)
            })
            .count() as u64;
        Ok(metrics)
    }
}
