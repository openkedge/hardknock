// SPDX-License-Identifier: Apache-2.0
//! Persistent distributed experience synchronization with local trust semantics.

use super::{
    ExperienceNodeId, NodeCapabilities, NodeIdentity, NodePublicIdentity, ProducerTrust,
    verify_detached,
};
use crate::{
    Error, Result,
    abstraction::KnowledgeMaturity,
    core::{
        ArtifactRevocationId, EvidencePathId, ExecutionPlanId, ExperienceOpportunityId,
        ReproductionQueueItemId, SyncArtifactId, SyncEnvelopeId, SyncSessionId,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SYNC_PROTOCOL_V1: &str = "hardknock.sync.v1";
pub const SYNC_ARTIFACT_SCHEMA_V1: &str = "hardknock.sync-artifact.v1";
pub const SYNC_SIGNING_DOMAIN: &[u8] = b"hardknock.sync-envelope.v1\0";
pub const REVOCATION_SIGNING_DOMAIN: &[u8] = b"hardknock.artifact-revocation.v1\0";
pub type NodeId = ExperienceNodeId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Developer,
    Ci,
    Integration,
    Staging,
    ProductionShadow,
    Production,
    Research,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub kind: EnvironmentKind,
    pub organization: Option<String>,
    pub team: Option<String>,
    pub environment: Option<String>,
    pub region: Option<String>,
    pub account_scope: Option<String>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

impl Default for EnvironmentIdentity {
    fn default() -> Self {
        Self {
            kind: EnvironmentKind::Developer,
            organization: None,
            team: None,
            environment: None,
            region: None,
            account_scope: None,
            tags: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishPolicy {
    pub experience: bool,
    pub lessons: bool,
    pub skills: bool,
    pub constraints: bool,
    pub recoveries: bool,
    pub abstractions: bool,
    pub certifications: bool,
    pub raw_logs: bool,
    pub sensitive_artifacts: bool,
}

impl Default for PublishPolicy {
    fn default() -> Self {
        Self {
            experience: true,
            lessons: true,
            skills: true,
            constraints: true,
            recoveries: true,
            abstractions: true,
            certifications: true,
            raw_logs: false,
            sensitive_artifacts: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivePolicy {
    pub require_signature: bool,
    pub allow_advisory: bool,
    pub require_local_evidence_for_critical: bool,
    pub quarantine_incompatible: bool,
    pub supported_protocols: BTreeSet<String>,
}

impl Default for ReceivePolicy {
    fn default() -> Self {
        Self {
            require_signature: true,
            allow_advisory: true,
            require_local_evidence_for_critical: true,
            quarantine_incompatible: true,
            supported_protocols: BTreeSet::from([SYNC_PROTOCOL_V1.into()]),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRetentionPolicy {
    pub retain_history: bool,
    pub retain_revoked: bool,
    pub max_records: Option<usize>,
}

impl Default for RemoteRetentionPolicy {
    fn default() -> Self {
        Self {
            retain_history: true,
            retain_revoked: true,
            max_records: Some(100_000),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteConflictPolicy {
    PreserveSeparate,
    Quarantine,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSyncPolicy {
    pub publish: PublishPolicy,
    pub receive: ReceivePolicy,
    pub retention: RemoteRetentionPolicy,
    pub conflict_policy: RemoteConflictPolicy,
}

impl Default for NodeSyncPolicy {
    fn default() -> Self {
        Self {
            publish: PublishPolicy::default(),
            receive: ReceivePolicy::default(),
            retention: RemoteRetentionPolicy::default(),
            conflict_policy: RemoteConflictPolicy::PreserveSeparate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrustPolicy {
    pub require_configured_peers: bool,
    pub default_manual_review: bool,
}

impl Default for NodeTrustPolicy {
    fn default() -> Self {
        Self {
            require_configured_peers: true,
            default_manual_review: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HardknockNode {
    pub id: NodeId,
    /// Public cryptographic identity only. Trust and correctness are separate fields.
    pub identity: NodePublicIdentity,
    pub label: Option<String>,
    pub key_revision: u64,
    pub environment: EnvironmentIdentity,
    pub capabilities: NodeCapabilities,
    pub trust_policy: NodeTrustPolicy,
    pub sync_policy: NodeSyncPolicy,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerTrust {
    Unknown,
    Known,
    TrustedForAuthenticity,
    TrustedForAdvisoryEvidence,
    Blocked,
}

impl From<ProducerTrust> for PeerTrust {
    fn from(value: ProducerTrust) -> Self {
        match value {
            ProducerTrust::Unknown => Self::Unknown,
            ProducerTrust::Known => Self::Known,
            ProducerTrust::Trusted => Self::TrustedForAdvisoryEvidence,
            ProducerTrust::Blocked => Self::Blocked,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerTrustPolicy {
    pub accept_signed_artifacts: bool,
    pub allow_advisory_retrieval: bool,
    pub allow_automatic_reproduction_queue: bool,
    pub allow_certification_import: bool,
    pub require_manual_review: bool,
}

impl Default for PeerTrustPolicy {
    fn default() -> Self {
        Self {
            accept_signed_artifacts: true,
            allow_advisory_retrieval: true,
            allow_automatic_reproduction_queue: false,
            allow_certification_import: true,
            require_manual_review: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PeerEndpoint {
    Filesystem(String),
    Http(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    Active,
    Offline,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncArtifactType {
    Experience,
    Lesson,
    Skill,
    Constraint,
    AntiPattern,
    Recovery,
    OperatingEnvelope,
    AbstractKnowledge,
    KnowledgeException,
    Certification,
    GuardCandidateEvidence,
    Contradiction,
    Revocation,
    EnvironmentManifest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFilter {
    pub artifact_types: BTreeSet<SyncArtifactType>,
    pub task_families: Vec<String>,
    pub environments: Vec<EnvironmentKind>,
    pub minimum_origin_maturity: Option<KnowledgeMaturity>,
    pub include_revocations: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncPeer {
    pub node: NodeId,
    pub name: String,
    pub public_key: String,
    pub endpoint: PeerEndpoint,
    pub trust: PeerTrust,
    pub trust_policy: PeerTrustPolicy,
    pub filters: SyncFilter,
    pub status: PeerStatus,
    pub last_sync: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortableArtifactRef {
    pub artifact_id: String,
    pub schema_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactLineage {
    pub artifact_id: String,
    pub origin: NodeId,
    pub revision: u64,
    pub parent_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootEvidenceOrigin {
    pub node: NodeId,
    pub artifact_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SyncArtifactRef {
    pub origin: NodeId,
    pub artifact_id: String,
    pub revision: u64,
    pub content_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAvailability {
    Full,
    MetadataOnly,
    Missing,
    Redacted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReproducibility {
    FullyReproducible,
    PartiallyReproducible,
    NotReproducible,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncArtifact {
    pub id: SyncArtifactId,
    pub artifact_type: SyncArtifactType,
    pub artifact_ref: PortableArtifactRef,
    pub lineage: ArtifactLineage,
    pub origin: NodeId,
    pub root_origin: RootEvidenceOrigin,
    pub relay_nodes: Vec<NodeId>,
    pub environment: EnvironmentIdentity,
    pub dependencies: Vec<SyncArtifactRef>,
    pub content_hash: String,
    pub task_family: Option<String>,
    pub origin_maturity: Option<KnowledgeMaturity>,
    pub critical: bool,
    pub availability: ArtifactAvailability,
    pub reproducibility: RemoteReproducibility,
    pub payload: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct ArtifactContent<'a> {
    artifact_type: &'a SyncArtifactType,
    artifact_ref: &'a PortableArtifactRef,
    lineage: &'a ArtifactLineage,
    origin: &'a NodeId,
    root_origin: &'a RootEvidenceOrigin,
    environment: &'a EnvironmentIdentity,
    dependencies: &'a [SyncArtifactRef],
    task_family: &'a Option<String>,
    origin_maturity: &'a Option<KnowledgeMaturity>,
    critical: bool,
    availability: ArtifactAvailability,
    reproducibility: RemoteReproducibility,
    payload: &'a Option<serde_json::Value>,
    created_at: DateTime<Utc>,
}

impl SyncArtifact {
    pub fn computed_content_hash(&self) -> Result<String> {
        let content = ArtifactContent {
            artifact_type: &self.artifact_type,
            artifact_ref: &self.artifact_ref,
            lineage: &self.lineage,
            origin: &self.origin,
            root_origin: &self.root_origin,
            environment: &self.environment,
            dependencies: &self.dependencies,
            task_family: &self.task_family,
            origin_maturity: &self.origin_maturity,
            critical: self.critical,
            availability: self.availability,
            reproducibility: self.reproducibility,
            payload: &self.payload,
            created_at: self.created_at,
        };
        Ok(blake3::hash(&serde_json::to_vec(&content)?)
            .to_hex()
            .to_string())
    }

    pub fn verify_hash(&self) -> Result<()> {
        if let Some(payload) = &self.payload {
            super::validate_safe_payload(payload, 32)?;
        }
        if self.content_hash != self.computed_content_hash()? {
            return Err(Error::InvalidInput(
                "Sync artifact content hash mismatch".into(),
            ));
        }
        if self.origin != self.lineage.origin
            || self.origin != self.root_origin.node
                && self.relay_nodes.is_empty()
                && self.dependencies.is_empty()
        {
            return Err(Error::InvalidInput(
                "Derivative or relayed artifact must preserve explicit provenance".into(),
            ));
        }
        Ok(())
    }

    pub fn reference(&self) -> SyncArtifactRef {
        SyncArtifactRef {
            origin: self.origin.clone(),
            artifact_id: self.artifact_ref.artifact_id.clone(),
            revision: self.lineage.revision,
            content_hash: self.content_hash.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncEnvelope {
    pub id: SyncEnvelopeId,
    pub protocol: String,
    pub sender: NodeId,
    pub key_revision: u64,
    pub artifacts: Vec<SyncArtifact>,
    pub created_at: DateTime<Utc>,
    pub nonce: String,
    pub signature: String,
}

#[derive(Serialize)]
struct EnvelopeBody<'a> {
    id: &'a SyncEnvelopeId,
    protocol: &'a str,
    sender: &'a NodeId,
    key_revision: u64,
    artifacts: &'a [SyncArtifact],
    created_at: DateTime<Utc>,
    nonce: &'a str,
}

impl SyncEnvelope {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&EnvelopeBody {
            id: &self.id,
            protocol: &self.protocol,
            sender: &self.sender,
            key_revision: self.key_revision,
            artifacts: &self.artifacts,
            created_at: self.created_at,
            nonce: &self.nonce,
        })?)
    }

    pub fn sign(&mut self, identity: &NodeIdentity) -> Result<()> {
        if self.sender != identity.node.id {
            return Err(Error::InvalidInput(
                "Sync envelope sender does not match signing node".into(),
            ));
        }
        for artifact in &self.artifacts {
            artifact.verify_hash()?;
        }
        self.signature = identity.sign_detached(SYNC_SIGNING_DOMAIN, &self.signing_bytes()?);
        Ok(())
    }

    pub fn verify(&self, public_key: &str) -> Result<()> {
        if self.protocol != SYNC_PROTOCOL_V1 {
            return Err(Error::InvalidInput("Unsupported sync protocol".into()));
        }
        if self.nonce.is_empty() || self.artifacts.len() > 100_000 {
            return Err(Error::InvalidInput("Invalid sync envelope bounds".into()));
        }
        let key = super::parse_public_key(public_key)?;
        if super::node_id(key.as_bytes())? != self.sender {
            return Err(Error::InvalidInput(
                "Sync envelope key does not identify declared sender".into(),
            ));
        }
        verify_detached(
            public_key,
            SYNC_SIGNING_DOMAIN,
            &self.signing_bytes()?,
            &self.signature,
        )?;
        for artifact in &self.artifacts {
            artifact.verify_hash()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStreamKind {
    Experience,
    Knowledge,
    Assurance,
    Revocations,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncCursor {
    pub peer: NodeId,
    pub stream: SyncStreamKind,
    pub position: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    Push,
    Pull,
    Bidirectional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncSessionStatus {
    Running,
    Completed,
    Failed,
    ReplayRejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncSession {
    pub id: SyncSessionId,
    pub peer: NodeId,
    pub direction: SyncDirection,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub received: usize,
    pub accepted: usize,
    pub quarantined: usize,
    pub rejected: usize,
    pub deduplicated: usize,
    pub status: SyncSessionStatus,
    pub reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteArtifactState {
    Received,
    Verified,
    Quarantined,
    Advisory,
    ReproductionRequired,
    LocallySupported,
    Rejected,
    Revoked,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCompatibilityStatus {
    Compatible,
    PartiallyCompatible,
    Incompatible,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentCompatibilityAssessment {
    pub source: EnvironmentIdentity,
    pub target: EnvironmentIdentity,
    pub matched_dimensions: Vec<String>,
    pub mismatched_dimensions: Vec<String>,
    pub unknown_dimensions: Vec<String>,
    pub status: EnvironmentCompatibilityStatus,
}

pub fn assess_environment_compatibility(
    source: &EnvironmentIdentity,
    target: &EnvironmentIdentity,
) -> EnvironmentCompatibilityAssessment {
    let mut matched = Vec::new();
    let mut mismatched = Vec::new();
    let mut unknown = Vec::new();
    if source.kind == target.kind {
        matched.push("kind".into());
    } else {
        mismatched.push("kind".into());
    }
    for (name, left, right) in [
        ("organization", &source.organization, &target.organization),
        ("team", &source.team, &target.team),
        ("environment", &source.environment, &target.environment),
        ("region", &source.region, &target.region),
        (
            "account_scope",
            &source.account_scope,
            &target.account_scope,
        ),
    ] {
        match (left, right) {
            (Some(a), Some(b)) if a == b => matched.push(name.into()),
            (Some(_), Some(_)) => mismatched.push(name.into()),
            _ => unknown.push(name.into()),
        }
    }
    let keys: BTreeSet<_> = source.tags.keys().chain(target.tags.keys()).collect();
    for key in keys {
        match (source.tags.get(key), target.tags.get(key)) {
            (Some(a), Some(b)) if a == b => matched.push(format!("tag:{key}")),
            (Some(_), Some(_)) => mismatched.push(format!("tag:{key}")),
            _ => unknown.push(format!("tag:{key}")),
        }
    }
    let status = if matched.is_empty() && !mismatched.is_empty() {
        EnvironmentCompatibilityStatus::Incompatible
    } else if mismatched.is_empty() && unknown.is_empty() {
        EnvironmentCompatibilityStatus::Compatible
    } else if !matched.is_empty() {
        EnvironmentCompatibilityStatus::PartiallyCompatible
    } else {
        EnvironmentCompatibilityStatus::Unknown
    };
    EnvironmentCompatibilityAssessment {
        source: source.clone(),
        target: target.clone(),
        matched_dimensions: matched,
        mismatched_dimensions: mismatched,
        unknown_dimensions: unknown,
        status,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteFreshness {
    pub artifact_created_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub origin_last_seen: Option<DateTime<Utc>>,
    pub stale: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteKnowledgeRecord {
    pub remote_artifact: SyncArtifactRef,
    pub artifact_type: SyncArtifactType,
    pub origin_status: Option<KnowledgeMaturity>,
    pub local_state: RemoteArtifactState,
    pub compatibility: EnvironmentCompatibilityAssessment,
    pub trust: PeerTrust,
    pub local_evidence: Vec<EvidencePathId>,
    pub availability: ArtifactAvailability,
    pub reproducibility: RemoteReproducibility,
    pub freshness: RemoteFreshness,
    pub review_required: bool,
    pub origin_certified: bool,
    pub locally_certified: bool,
    #[serde(default)]
    pub origin_revoked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemotePromotionDecision {
    KeepAdvisory,
    ReproduceLocally,
    PromoteWithLocalEvidence,
    NarrowScope,
    Reject,
}

pub fn conservative_promotion_decision(
    record: &RemoteKnowledgeRecord,
    critical: bool,
) -> RemotePromotionDecision {
    if record.origin_revoked
        || record.local_state == RemoteArtifactState::Revoked
        || record.compatibility.status == EnvironmentCompatibilityStatus::Incompatible
    {
        return RemotePromotionDecision::Reject;
    }
    if !record.local_evidence.is_empty() {
        return if record.compatibility.status == EnvironmentCompatibilityStatus::Compatible {
            RemotePromotionDecision::PromoteWithLocalEvidence
        } else {
            RemotePromotionDecision::NarrowScope
        };
    }
    if critical
        || matches!(
            record.artifact_type,
            SyncArtifactType::Constraint | SyncArtifactType::KnowledgeException
        )
    {
        RemotePromotionDecision::ReproduceLocally
    } else {
        RemotePromotionDecision::KeepAdvisory
    }
}

pub fn remote_support_allows_act(record: &RemoteKnowledgeRecord, critical: bool) -> bool {
    !critical
        && record.local_state == RemoteArtifactState::LocallySupported
        && !record.local_evidence.is_empty()
}

pub fn remote_exception_can_relax_local_constraint(
    remote: &RemoteKnowledgeRecord,
    local_exception_evidence: &[EvidencePathId],
) -> bool {
    remote.artifact_type == SyncArtifactType::KnowledgeException
        && remote.local_state == RemoteArtifactState::LocallySupported
        && !remote.local_evidence.is_empty()
        && !local_exception_evidence.is_empty()
}

pub fn prioritize_remote_artifacts<'a>(
    artifacts: &'a [SyncArtifact],
    active_task_family: &str,
    critical_only: bool,
) -> Vec<&'a SyncArtifact> {
    artifacts
        .iter()
        .filter(|artifact| {
            artifact.task_family.as_deref() == Some(active_task_family)
                && (!critical_only || artifact.critical)
                && artifact.artifact_type != SyncArtifactType::Revocation
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionQueueStatus {
    Pending,
    Planned,
    Running,
    Supported,
    Contradicted,
    Inconclusive,
    Deferred,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReproductionQueueItem {
    pub id: ReproductionQueueItemId,
    pub remote_artifact: SyncArtifactRef,
    pub target_context: EnvironmentIdentity,
    pub priority: Option<ExperienceOpportunityId>,
    pub relevance: String,
    pub status: ReproductionQueueStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DistributedEvidencePath {
    pub evidence: Option<EvidencePathId>,
    pub origin_node: NodeId,
    pub origin_environment: EnvironmentIdentity,
    pub root_origin: RootEvidenceOrigin,
    pub relay_nodes: Vec<NodeId>,
    pub local_reproduction: Option<EvidencePathId>,
}

pub fn distinct_root_origins<'a>(
    paths: impl IntoIterator<Item = &'a DistributedEvidencePath>,
) -> BTreeSet<(NodeId, String, u64)> {
    paths
        .into_iter()
        .map(|path| {
            (
                path.root_origin.node.clone(),
                path.root_origin.artifact_id.clone(),
                path.root_origin.revision,
            )
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteContradiction {
    pub target: SyncArtifactRef,
    pub evidence: Vec<EvidencePathId>,
    pub context: EnvironmentIdentity,
    pub origin: NodeId,
    pub received_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationReason {
    Contradicted,
    SecurityIssue,
    CorruptEvidence,
    Superseded,
    ScopeIncorrect,
    OriginCompromised,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactRevocation {
    pub id: ArtifactRevocationId,
    pub artifact: SyncArtifactRef,
    pub origin: NodeId,
    pub reason: RevocationReason,
    pub evidence: Vec<EvidencePathId>,
    pub created_at: DateTime<Utc>,
    pub signature: String,
}

impl ArtifactRevocation {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        #[derive(Serialize)]
        struct Body<'a> {
            id: &'a ArtifactRevocationId,
            artifact: &'a SyncArtifactRef,
            origin: &'a NodeId,
            reason: &'a RevocationReason,
            evidence: &'a [EvidencePathId],
            created_at: DateTime<Utc>,
        }
        Ok(serde_json::to_vec(&Body {
            id: &self.id,
            artifact: &self.artifact,
            origin: &self.origin,
            reason: &self.reason,
            evidence: &self.evidence,
            created_at: self.created_at,
        })?)
    }

    pub fn sign(&mut self, identity: &NodeIdentity) -> Result<()> {
        if self.origin != identity.node.id || self.origin != self.artifact.origin {
            return Err(Error::InvalidInput(
                "Only an artifact origin may sign its revocation".into(),
            ));
        }
        self.signature = identity.sign_detached(REVOCATION_SIGNING_DOMAIN, &self.signing_bytes()?);
        Ok(())
    }

    pub fn verify(&self, public_key: &str) -> Result<()> {
        if self.origin != self.artifact.origin {
            return Err(Error::InvalidInput(
                "Revocation origin does not own artifact lineage".into(),
            ));
        }
        verify_detached(
            public_key,
            REVOCATION_SIGNING_DOMAIN,
            &self.signing_bytes()?,
            &self.signature,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeKeyRevision {
    pub node: NodeId,
    pub revision: u64,
    pub public_key: String,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub signed_by_previous_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentManifest {
    pub environment: EnvironmentIdentity,
    pub software_versions: BTreeMap<String, String>,
    pub tool_versions: BTreeMap<String, String>,
    pub runtime_versions: BTreeMap<String, String>,
    pub captured_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncBudget {
    pub max_artifacts: Option<usize>,
    pub max_bytes: Option<u64>,
    pub max_artifact_age_seconds: Option<i64>,
}

impl Default for SyncBudget {
    fn default() -> Self {
        Self {
            max_artifacts: Some(10_000),
            max_bytes: Some(100 * 1024 * 1024),
            max_artifact_age_seconds: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemotePlanImpact {
    NoChange,
    VerificationRequired,
}

pub fn remote_contradiction_plan_impact(
    plan: Option<&ExecutionPlanId>,
    context_overlaps: bool,
    next_action_irreversible: bool,
) -> RemotePlanImpact {
    if plan.is_some() && context_overlaps && next_action_irreversible {
        RemotePlanImpact::VerificationRequired
    } else {
        RemotePlanImpact::NoChange
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncMetrics {
    pub artifacts_published: u64,
    pub artifacts_received: u64,
    pub artifacts_verified: u64,
    pub artifacts_rejected: u64,
    pub artifacts_quarantined: u64,
    pub artifacts_deduplicated: u64,
    pub remote_artifacts_promoted: u64,
    pub remote_artifacts_reproduced: u64,
    pub remote_contradictions_received: u64,
    pub revocations_received: u64,
    pub sync_lag_seconds: Vec<i64>,
    pub blind_critical_promotions: u64,
}
