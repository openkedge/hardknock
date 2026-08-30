// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{Error, Result, core::*, federation::*};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};

impl Store {
    pub fn save_experience_node(&self, node: &ExperienceNode) -> Result<ExperienceNode> {
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experience_nodes WHERE id=?1",
                [node.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(data) = existing {
            return Ok(serde_json::from_str(&data)?);
        }
        self.connection.execute("INSERT INTO experience_nodes(id,name,node_type,public_key,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",params![node.id.to_string(),node.name,serde_json::to_value(node.node_type)?.as_str(),node.public_identity.public_key,node.created_at.to_rfc3339(),serde_json::to_string(node)?])?;
        Ok(node.clone())
    }
    pub fn local_experience_node(&self) -> Result<Option<ExperienceNode>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experience_nodes ORDER BY created_at,id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|x| Ok(serde_json::from_str(&x)?)).transpose()
    }
    pub fn add_peer(
        &self,
        name: &str,
        public_key: &str,
        node_id: &ExperienceNodeId,
    ) -> Result<Peer> {
        if name.trim().is_empty() || name.len() > 120 || name.contains(['\0', '\n', '\r']) {
            return Err(Error::InvalidInput(
                "Peer name must be 1–120 single-line characters".into(),
            ));
        }
        let id = format!(
            "peer-{}",
            &blake3::hash(node_id.to_string().as_bytes())
                .to_hex()
                .to_string()[..24]
        );
        let peer = Peer {
            id: id.clone(),
            node_id: node_id.clone(),
            name: name.into(),
            public_key: public_key.into(),
            trust: ProducerTrust::Known,
            added_at: Utc::now(),
        };
        self.connection.execute("INSERT INTO federation_peers(id,node_id,name,public_key,trust,added_at,data) VALUES(?1,?2,?3,?4,'known',?5,?6)",params![id,node_id.to_string(),name,public_key,peer.added_at.to_rfc3339(),serde_json::to_string(&peer)?])?;
        self.audit(
            "peer_added",
            Some(&peer.id),
            &format!("Known peer {} added", peer.name),
        )?;
        Ok(peer)
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        self.list("SELECT data FROM federation_peers ORDER BY name,id")
    }
    pub fn peer(&self, selector: &str) -> Result<Peer> {
        self.get(
            "SELECT data FROM federation_peers WHERE id=?1 OR name=?1 OR node_id=?1",
            selector,
        )
    }
    pub fn peer_by_node(&self, node: &ExperienceNodeId) -> Result<Option<Peer>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM federation_peers WHERE node_id=?1",
                [node.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|x| Ok(serde_json::from_str(&x)?)).transpose()
    }
    pub fn set_peer_trust(&self, selector: &str, trust: ProducerTrust) -> Result<Peer> {
        let mut peer = self.peer(selector)?;
        peer.trust = trust;
        let changed = self.connection.execute(
            "UPDATE federation_peers SET trust=?2,data=?3 WHERE id=?1",
            params![
                peer.id,
                serde_json::to_value(trust)?.as_str(),
                serde_json::to_string(&peer)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(selector.into()));
        }
        self.audit(
            "peer_trust_changed",
            Some(&peer.id),
            &format!("Peer {} is now {:?}", peer.name, trust),
        )?;
        Ok(peer)
    }
    pub fn remove_peer(&self, selector: &str) -> Result<Peer> {
        let peer = self.peer(selector)?;
        self.connection
            .execute("DELETE FROM federation_peers WHERE id=?1", [&peer.id])?;
        self.audit(
            "peer_removed",
            Some(&peer.id),
            &format!("Peer {} removed; imported evidence retained", peer.name),
        )?;
        Ok(peer)
    }
    pub fn save_federation_bundle(
        &self,
        signed: &SignedExperienceBundle,
        direction: &str,
        authenticity: AuthenticityStatus,
        path: Option<&str>,
    ) -> Result<bool> {
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT payload_hash FROM federation_bundles WHERE id=?1",
                [signed.manifest.bundle_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(hash) = existing {
            if hash != signed.payload_hash {
                return Err(Error::InvalidInput("Bundle ID collision".into()));
            }
            return Ok(false);
        }
        self.connection.execute("INSERT INTO federation_bundles(id,direction,producer,created_at,recorded_at,payload_hash,authenticity,path,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![signed.manifest.bundle_id.to_string(),direction,signed.manifest.producer.to_string(),signed.manifest.created_at.to_rfc3339(),Utc::now().to_rfc3339(),signed.payload_hash,serde_json::to_value(authenticity)?.as_str(),path,serde_json::to_string(signed)?])?;
        Ok(true)
    }
    pub fn federation_bundle(&self, id: &BundleId) -> Result<SignedExperienceBundle> {
        self.get(
            "SELECT data FROM federation_bundles WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn save_federated_object(
        &self,
        object: &FederatedObject,
    ) -> Result<(FederatedObjectId, bool)> {
        let existing:Option<String>=self.connection.query_row("SELECT id FROM federated_objects WHERE origin_node=?1 AND origin_object_id=?2 AND lineage_hash=?3",params![object.identity.origin_node.to_string(),object.identity.origin_object_id,object.identity.lineage_hash],|r|r.get(0)).optional()?;
        if let Some(id) = existing {
            return Ok((id.parse()?, false));
        }
        self.connection.execute("INSERT INTO federated_objects(id,origin_node,origin_object_id,origin_bundle,object_type,lineage_hash,state,context_score,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![object.id.to_string(),object.identity.origin_node.to_string(),object.identity.origin_object_id,object.origin_bundle.to_string(),object.object_type,object.identity.lineage_hash,serde_json::to_value(object.state)?.as_str(),object.trust.context_compatibility.score,object.received_at.to_rfc3339(),serde_json::to_string(object)?])?;
        Ok((object.id.clone(), true))
    }
    pub fn federated_object(&self, id: &FederatedObjectId) -> Result<FederatedObject> {
        self.get(
            "SELECT data FROM federated_objects WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn federated_objects(&self) -> Result<Vec<FederatedObject>> {
        self.list("SELECT data FROM federated_objects ORDER BY context_score DESC,created_at,id")
    }
    pub fn search_federated(
        &self,
        kind: Option<&str>,
        marker: Option<&str>,
    ) -> Result<Vec<FederatedObject>> {
        Ok(self
            .federated_objects()?
            .into_iter()
            .filter(|o| {
                kind.is_none_or(|k| o.object_type == k)
                    && marker.is_none_or(|m| {
                        o.object
                            .pointer("/context/markers")
                            .and_then(|v| v.as_array())
                            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(m)))
                    })
            })
            .collect())
    }
    pub fn update_federated_object(&self, object: &FederatedObject) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE federated_objects SET state=?2,context_score=?3,data=?4 WHERE id=?1",
            params![
                object.id.to_string(),
                serde_json::to_value(object.state)?.as_str(),
                object.trust.context_compatibility.score,
                serde_json::to_string(object)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(object.id.to_string()));
        }
        Ok(())
    }
    pub fn save_reproduction(&self, reproduction: &FederationReproduction) -> Result<()> {
        self.connection.execute("INSERT INTO federated_reproductions(id,object_id,experiment_id,result,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",params![reproduction.id.to_string(),reproduction.object_id.to_string(),reproduction.experiment_id,serde_json::to_value(reproduction.result)?.as_str(),reproduction.created_at.to_rfc3339(),serde_json::to_string(reproduction)?])?;
        Ok(())
    }
    pub fn reproductions(&self) -> Result<Vec<FederationReproduction>> {
        self.list("SELECT data FROM federated_reproductions ORDER BY created_at,id")
    }
    pub fn save_federated_conflict(
        &self,
        conflict: &FederatedConflict,
        object_id: &FederatedObjectId,
    ) -> Result<()> {
        self.connection.execute("INSERT INTO federated_conflicts(id,object_id,conflict_type,status,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",params![conflict.id.to_string(),object_id.to_string(),serde_json::to_value(conflict.conflict_type)?.as_str(),serde_json::to_value(conflict.status)?.as_str(),conflict.created_at.to_rfc3339(),serde_json::to_string(conflict)?])?;
        Ok(())
    }
    pub fn federated_conflicts(&self) -> Result<Vec<FederatedConflict>> {
        self.list("SELECT data FROM federated_conflicts ORDER BY created_at,id")
    }
    pub fn federated_conflict(&self, id: &FederatedConflictId) -> Result<FederatedConflict> {
        self.get(
            "SELECT data FROM federated_conflicts WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn save_provenance_graph(&self, graph: &ProvenanceGraph) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for node in &graph.nodes {
            tx.execute("INSERT INTO federation_provenance_nodes(id,kind,lineage_hash,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO NOTHING",params![node.id.to_string(),serde_json::to_value(node.kind)?.as_str(),node.lineage_hash,serde_json::to_string(node)?])?;
        }
        for edge in &graph.edges {
            tx.execute("INSERT INTO federation_provenance_edges(source,target,relationship,data) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![edge.source.to_string(),edge.target.to_string(),serde_json::to_value(edge.relationship)?.as_str(),serde_json::to_string(edge)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn provenance_graph(&self, selector: &str) -> Result<ProvenanceGraph> {
        let start:Option<String>=self.connection.query_row("SELECT id FROM federation_provenance_nodes WHERE id=?1 OR json_extract(data,'$.external_id')=?1 ORDER BY id LIMIT 1",[selector],|r|r.get(0)).optional()?;
        let Some(start) = start else {
            return Err(Error::NotFound(format!(
                "Provenance object {selector} not found"
            )));
        };
        let mut ids = std::collections::BTreeSet::from([start]);
        loop {
            let before = ids.len();
            let edges = self.provenance_edges()?;
            for edge in edges {
                if ids.contains(&edge.source.to_string()) || ids.contains(&edge.target.to_string())
                {
                    ids.insert(edge.source.to_string());
                    ids.insert(edge.target.to_string());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        let all: Vec<ProvenanceNode> =
            self.list("SELECT data FROM federation_provenance_nodes ORDER BY id")?;
        let edges = self.provenance_edges()?;
        Ok(ProvenanceGraph {
            nodes: all
                .into_iter()
                .filter(|n| ids.contains(&n.id.to_string()))
                .collect(),
            edges: edges
                .into_iter()
                .filter(|e| {
                    ids.contains(&e.source.to_string()) && ids.contains(&e.target.to_string())
                })
                .collect(),
        })
    }
    fn provenance_edges(&self) -> Result<Vec<ProvenanceEdge>> {
        self.list(
            "SELECT data FROM federation_provenance_edges ORDER BY source,target,relationship",
        )
    }
    pub fn audit(
        &self,
        event: &str,
        subject: Option<&str>,
        detail: &str,
    ) -> Result<FederationAuditEntry> {
        let entry = FederationAuditEntry {
            id: FederationAuditId::new().to_string(),
            event: event.into(),
            at: Utc::now(),
            subject: subject.map(Into::into),
            detail: detail.into(),
        };
        self.connection.execute(
            "INSERT INTO federation_audit(id,event,created_at,data) VALUES(?1,?2,?3,?4)",
            params![
                entry.id,
                event,
                entry.at.to_rfc3339(),
                serde_json::to_string(&entry)?
            ],
        )?;
        Ok(entry)
    }
    pub fn federation_audit(&self) -> Result<Vec<FederationAuditEntry>> {
        self.list("SELECT data FROM federation_audit ORDER BY sequence")
    }
    pub fn federation_status(&self) -> Result<FederationStatus> {
        let peers = self.peers()?;
        let objects = self.federated_objects()?;
        let count =
            |state: FederatedExperienceState| objects.iter().filter(|o| o.state == state).count();
        Ok(FederationStatus {
            node: self.local_experience_node()?,
            peers_known: peers
                .iter()
                .filter(|p| p.trust == ProducerTrust::Known)
                .count(),
            peers_trusted: peers
                .iter()
                .filter(|p| p.trust == ProducerTrust::Trusted)
                .count(),
            peers_blocked: peers
                .iter()
                .filter(|p| p.trust == ProducerTrust::Blocked)
                .count(),
            published_bundles: self.connection.query_row(
                "SELECT count(*) FROM federation_bundles WHERE direction='published'",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize,
            received_bundles: self.connection.query_row(
                "SELECT count(*) FROM federation_bundles WHERE direction='received'",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize,
            external_advisory: objects
                .iter()
                .filter(|o| {
                    matches!(
                        o.state,
                        FederatedExperienceState::Received
                            | FederatedExperienceState::ContextMatched
                            | FederatedExperienceState::ReproductionRecommended
                    )
                })
                .count(),
            locally_supported: count(FederatedExperienceState::LocallySupported),
            locally_validated: count(FederatedExperienceState::LocallyValidated),
            contradicted: count(FederatedExperienceState::LocallyContradicted),
            reproduction_backlog: objects
                .iter()
                .filter(|o| {
                    matches!(
                        o.state,
                        FederatedExperienceState::ContextMatched
                            | FederatedExperienceState::ReproductionRecommended
                    )
                })
                .count(),
        })
    }
    pub fn save_federation_benchmark(
        &self,
        result: &crate::federation::benchmark::FederationBenchmarkResult,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO federation_benchmark_runs(id,created_at,status,data) VALUES(?1,?2,?3,?4)",
            params![
                result.id.to_string(),
                result.created_at.to_rfc3339(),
                result.status,
                serde_json::to_string(result)?
            ],
        )?;
        Ok(())
    }
    pub fn federation_benchmarks(
        &self,
    ) -> Result<Vec<crate::federation::benchmark::FederationBenchmarkResult>> {
        self.list("SELECT data FROM federation_benchmark_runs ORDER BY created_at,id")
    }
}
