// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    core::{AgentIdentity, FederatedConflictId, FederatedObjectId, FederationReproductionId},
    experience::Outcome,
    lesson::{ActionPattern, ConfidenceScore, ContextSelector, LessonStatus},
    resilience::{RecoveryStatus, ReflexResponse, ReflexStatus, SkillStatus},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, str::FromStr};

pub const BUNDLE_SCHEMA_V1: &str = "hardknock.bundle.v1";
pub const SIGNING_DOMAIN: &[u8] = b"hardknock.signed-experience.v1\0";

macro_rules! text_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub(crate) fn from_digest(digest: &str) -> Result<Self> {
                format!("{}{}", $prefix, digest).parse()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self> {
                let digest = value.strip_prefix($prefix).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "Expected {}<64 lowercase hex characters>",
                        $prefix
                    ))
                })?;
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(Error::InvalidInput(format!(
                        "Expected {}<64 lowercase hex characters>",
                        $prefix
                    )));
                }
                Ok(Self(value.into()))
            }
        }
        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(v: String) -> Result<Self> {
                v.parse()
            }
        }
        impl From<$name> for String {
            fn from(v: $name) -> Self {
                v.0
            }
        }
    };
}
text_id!(ExperienceNodeId, "hk-node:");
text_id!(BundleId, "hk-bundle:");
text_id!(ProvenanceNodeId, "hk-provenance:");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceNodeType {
    LocalDeveloper,
    Repository,
    Team,
    Organization,
    Ci,
    AgentRuntime,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePublicIdentity {
    pub algorithm: String,
    pub public_key: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub schemas: Vec<String>,
    pub transports: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceNode {
    pub id: ExperienceNodeId,
    pub name: String,
    pub node_type: ExperienceNodeType,
    pub public_identity: NodePublicIdentity,
    pub capabilities: NodeCapabilities,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationVisibility {
    #[default]
    Private,
    Team,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "objects", rename_all = "snake_case")]
pub enum ExportScope {
    Object,
    SkillPackage,
    Repository,
    TaskFamily,
    Selected(Vec<ExperienceObjectRef>),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceObjectRef {
    pub kind: String,
    pub id: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleAncestry {
    pub parent_bundles: Vec<BundleId>,
    pub source_nodes: Vec<ExperienceNodeId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceBundleManifest {
    pub bundle_id: BundleId,
    pub schema_version: String,
    pub producer: ExperienceNodeId,
    pub created_at: DateTime<Utc>,
    pub scope: ExportScope,
    pub evidence_count: usize,
    pub minimum_hardknock_version: Option<String>,
    pub labels: Vec<String>,
    pub visibility: FederationVisibility,
    pub ancestry: BundleAncestry,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicDependencies {
    pub model_family: Option<String>,
    pub agent_runtime: Option<String>,
    pub evaluator: Option<String>,
    pub environment_family: Option<String>,
    pub external_sources: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDiversity {
    pub node_count: usize,
    pub agent_count: usize,
    pub repository_count: usize,
    pub context_count: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceContext {
    pub repository_family: Option<String>,
    pub markers: Vec<String>,
    pub tags: Vec<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub versions: BTreeMap<String, String>,
    pub environment_fingerprint: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedObjectIdentity {
    pub origin_node: ExperienceNodeId,
    pub origin_object_id: String,
    pub lineage_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableEvidenceSummary {
    pub support_count: usize,
    pub contradiction_count: usize,
    pub experiment_count: usize,
    pub application_count: usize,
    pub evaluation_summaries: Vec<String>,
    pub evidence_hashes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableExperience {
    pub identity: FederatedObjectIdentity,
    pub created_at: DateTime<Utc>,
    pub goal: String,
    pub context: EvidenceContext,
    pub starting_state_hash: String,
    pub outcome: Outcome,
    pub evaluation_summary: String,
    pub originating_agent: AgentIdentity,
    pub dependencies: EpistemicDependencies,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableLesson {
    pub identity: FederatedObjectIdentity,
    pub claim: String,
    pub context: EvidenceContext,
    pub avoid: Option<ActionPattern>,
    pub prefer: Option<ActionPattern>,
    pub evaluation_checks: Vec<String>,
    pub source_status: LessonStatus,
    pub source_confidence: ConfidenceScore,
    pub evidence_summary: PortableEvidenceSummary,
    pub originating_agents: Vec<AgentIdentity>,
    pub freshness: String,
    pub dependencies: EpistemicDependencies,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableSkill {
    pub identity: FederatedObjectIdentity,
    pub name: String,
    pub description: String,
    pub context: EvidenceContext,
    pub procedure: Vec<ActionPattern>,
    pub source_status: SkillStatus,
    pub evidence_hashes: Vec<String>,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableExperimentSummary {
    pub identity: FederatedObjectIdentity,
    pub conclusion: String,
    pub trial_outcomes: Vec<Outcome>,
    pub starting_state_hash: String,
    pub evidence_hashes: Vec<String>,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableReflex {
    pub identity: FederatedObjectIdentity,
    pub trigger_context: EvidenceContext,
    pub proposed_action: ActionPattern,
    pub requested_response: ReflexResponse,
    pub effective_response: ReflexResponse,
    pub source_status: ReflexStatus,
    pub confidence: ConfidenceScore,
    pub evidence_hashes: Vec<String>,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableRecovery {
    pub identity: FederatedObjectIdentity,
    pub failure_signature: String,
    pub context: EvidenceContext,
    pub steps: Vec<String>,
    pub source_status: RecoveryStatus,
    pub confidence: ConfidenceScore,
    pub evidence_hashes: Vec<String>,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableOperatingEnvelope {
    pub identity: FederatedObjectIdentity,
    pub context: EvidenceContext,
    pub source_summary: String,
    pub tested_points: Vec<String>,
    pub evidence_hashes: Vec<String>,
    pub provenance_ref: ProvenanceNodeId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceNodeKind {
    Experience,
    Experiment,
    Lesson,
    Skill,
    Reflex,
    Recovery,
    Bundle,
    Node,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvenanceNode {
    pub id: ProvenanceNodeId,
    pub kind: ProvenanceNodeKind,
    pub external_id: String,
    pub node: ExperienceNodeId,
    pub lineage_hash: Option<String>,
    pub summary: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceRelationship {
    DerivedFrom,
    Supports,
    Contradicts,
    ExportedAs,
    ImportedFrom,
    ReproducedBy,
    Supersedes,
    Narrows,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvenanceEdge {
    pub source: ProvenanceNodeId,
    pub target: ProvenanceNodeId,
    pub relationship: ProvenanceRelationship,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProvenanceGraph {
    pub nodes: Vec<ProvenanceNode>,
    pub edges: Vec<ProvenanceEdge>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceBundle {
    pub manifest: ExperienceBundleManifest,
    pub experiences: Vec<PortableExperience>,
    pub lessons: Vec<PortableLesson>,
    pub skills: Vec<PortableSkill>,
    pub experiments: Vec<PortableExperimentSummary>,
    pub reflexes: Vec<PortableReflex>,
    pub recoveries: Vec<PortableRecovery>,
    pub envelopes: Vec<PortableOperatingEnvelope>,
    pub provenance: ProvenanceGraph,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedExperienceBundle {
    pub manifest: ExperienceBundleManifest,
    pub payload_hash: String,
    pub signer: ExperienceNodeId,
    pub signer_public_key: String,
    pub signature: String,
    pub bundle: ExperienceBundle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Untrusted,
    SignedUnknown,
    KnownPeer,
    TrustedPeer,
    LocallySupported,
    LocallyValidated,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticityStatus {
    Unsigned,
    SignatureValid,
    SignatureInvalid,
    UnknownKey,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerTrust {
    Unknown,
    Known,
    Trusted,
    Blocked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionStatus {
    NotAttempted,
    Supports,
    Contradicts,
    Inconclusive,
    CannotReproduce,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionStatus {
    None,
    LocalContradiction,
    LocalConflict,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextMatch {
    pub field: String,
    pub remote: String,
    pub local: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextMismatch {
    pub field: String,
    pub remote: String,
    pub local: String,
    pub severity: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextCompatibility {
    pub score: f64,
    pub matches: Vec<ContextMatch>,
    pub mismatches: Vec<ContextMismatch>,
    pub unknowns: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceTrust {
    pub authenticity: AuthenticityStatus,
    pub producer_trust: ProducerTrust,
    pub local_reproduction: ReproductionStatus,
    pub context_compatibility: ContextCompatibility,
    pub contradiction_status: ContradictionStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederatedExperienceState {
    Received,
    ContextMatched,
    ReproductionRecommended,
    LocallySupported,
    LocallyContradicted,
    LocallyValidated,
    Rejected,
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionResult {
    Supports,
    Contradicts,
    Inconclusive,
    CannotReproduce,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederatedObject {
    pub id: FederatedObjectId,
    pub identity: FederatedObjectIdentity,
    pub origin_bundle: BundleId,
    pub object_type: String,
    pub state: FederatedExperienceState,
    pub trust: ExperienceTrust,
    pub object: serde_json::Value,
    pub received_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederatedConflictType {
    ClaimConflict,
    ActionConflict,
    EnvelopeConflict,
    RecoveryConflict,
    ScopeConflict,
    VersionConflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStatus {
    Open,
    ExperimentRecommended,
    SupportedRemote,
    SupportedLocal,
    Inconclusive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederatedConflict {
    pub id: FederatedConflictId,
    pub external_object: FederatedObjectId,
    pub local: ExperienceObjectRef,
    pub remote: ExperienceObjectRef,
    pub conflict_type: FederatedConflictType,
    pub status: ConflictStatus,
    pub local_evidence: Vec<String>,
    pub remote_evidence: Vec<String>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationReproduction {
    pub id: FederationReproductionId,
    pub object_id: FederatedObjectId,
    pub experiment_id: Option<String>,
    pub result: ReproductionResult,
    pub experience_ids: Vec<String>,
    pub explanation: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub node_id: ExperienceNodeId,
    pub name: String,
    pub public_key: String,
    pub trust: ProducerTrust,
    pub added_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleLimits {
    pub max_bundle_bytes: u64,
    pub max_objects: usize,
    pub max_artifact_bytes: u64,
    pub max_nesting_depth: usize,
}
impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_bundle_bytes: 50 * 1024 * 1024,
            max_objects: 10_000,
            max_artifact_bytes: 10 * 1024 * 1024,
            max_nesting_depth: 32,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FederationConfig {
    pub auto_publish: bool,
    pub node_name: String,
    pub limits: BundleLimits,
    pub minimum_context_match: f64,
    pub allow_raw_artifacts: bool,
}
impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            auto_publish: false,
            node_name: "local-developer".into(),
            limits: Default::default(),
            minimum_context_match: 0.70,
            allow_raw_artifacts: false,
        }
    }
}
impl FederationConfig {
    pub fn validate(&self) -> Result<()> {
        if self.auto_publish || self.allow_raw_artifacts {
            return Err(Error::InvalidInput(
                "V0.7 requires explicit publication and excludes raw artifacts".into(),
            ));
        }
        if self.node_name.trim().is_empty()
            || self.node_name.len() > 120
            || !(1024..=100 * 1024 * 1024).contains(&self.limits.max_bundle_bytes)
            || !(1..=100_000).contains(&self.limits.max_objects)
            || !(1..=128).contains(&self.limits.max_nesting_depth)
            || !self.minimum_context_match.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_context_match)
        {
            return Err(Error::InvalidInput(
                "Federation configuration is out of range".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportReport {
    pub bundle_id: BundleId,
    pub producer: ExperienceNodeId,
    pub authenticity: AuthenticityStatus,
    pub producer_trust: ProducerTrust,
    pub imported: usize,
    pub duplicates: usize,
    pub state: String,
    pub objects: Vec<FederatedObjectId>,
    pub recommended_action: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FederationStatus {
    pub node: Option<ExperienceNode>,
    pub peers_known: usize,
    pub peers_trusted: usize,
    pub peers_blocked: usize,
    pub published_bundles: usize,
    pub received_bundles: usize,
    pub external_advisory: usize,
    pub locally_supported: usize,
    pub locally_validated: usize,
    pub contradicted: usize,
    pub reproduction_backlog: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationAuditEntry {
    pub id: String,
    pub event: String,
    pub at: DateTime<Utc>,
    pub subject: Option<String>,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationFeedback {
    pub target_origin: FederatedObjectIdentity,
    pub reporting_node: ExperienceNodeId,
    pub result: ReproductionResult,
    pub context: EvidenceContext,
    pub evidence_refs: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleRevocation {
    pub bundle_id: BundleId,
    pub reason: String,
    pub signer: ExperienceNodeId,
    pub created_at: DateTime<Utc>,
    pub signature: String,
}

pub trait ContextCompatibilityPolicy {
    fn compare(
        &self,
        remote: &EvidenceContext,
        local: &crate::retrieval::QueryContext,
    ) -> ContextCompatibility;
}
pub struct DeterministicContextCompatibility;
impl ContextCompatibilityPolicy for DeterministicContextCompatibility {
    fn compare(
        &self,
        remote: &EvidenceContext,
        local: &crate::retrieval::QueryContext,
    ) -> ContextCompatibility {
        let mut matches = vec![];
        let mut mismatches = vec![];
        let mut unknowns = vec![];
        let mut earned = 0.0;
        let mut possible = 0.0;
        // Keeping every output accumulator explicit makes the scoring mutation visible here.
        #[allow(clippy::too_many_arguments)]
        fn field(
            name: &str,
            remote: Option<&str>,
            local: Option<&str>,
            weight: f64,
            version: bool,
            earned: &mut f64,
            possible: &mut f64,
            matches: &mut Vec<ContextMatch>,
            mismatches: &mut Vec<ContextMismatch>,
            unknowns: &mut Vec<String>,
        ) {
            *possible += weight;
            match (remote, local) {
                (Some(r), Some(l)) if r == l => {
                    *earned += weight;
                    matches.push(ContextMatch {
                        field: name.into(),
                        remote: r.into(),
                        local: l.into(),
                    })
                }
                (Some(r), Some(l)) => {
                    if version && r.split('.').next() == l.split('.').next() {
                        *earned += weight * 0.55;
                    }
                    mismatches.push(ContextMismatch {
                        field: name.into(),
                        remote: r.into(),
                        local: l.into(),
                        severity: if version {
                            "version_difference"
                        } else {
                            "different"
                        }
                        .into(),
                    })
                }
                _ => unknowns.push(name.into()),
            }
        }
        field(
            "os",
            remote.os.as_deref(),
            Some(&local.environment.os),
            0.20,
            false,
            &mut earned,
            &mut possible,
            &mut matches,
            &mut mismatches,
            &mut unknowns,
        );
        field(
            "arch",
            remote.arch.as_deref(),
            Some(&local.environment.arch),
            0.10,
            false,
            &mut earned,
            &mut possible,
            &mut matches,
            &mut mismatches,
            &mut unknowns,
        );
        field(
            "repository_family",
            remote.repository_family.as_deref(),
            Some(&local.repository.name),
            0.10,
            false,
            &mut earned,
            &mut possible,
            &mut matches,
            &mut mismatches,
            &mut unknowns,
        );
        for marker in &remote.markers {
            possible += 0.35 / remote.markers.len().max(1) as f64;
            if local.detected_markers.contains(marker) {
                earned += 0.35 / remote.markers.len().max(1) as f64;
                matches.push(ContextMatch {
                    field: "marker".into(),
                    remote: marker.clone(),
                    local: marker.clone(),
                })
            } else {
                mismatches.push(ContextMismatch {
                    field: "marker".into(),
                    remote: marker.clone(),
                    local: "missing".into(),
                    severity: "missing_required".into(),
                })
            }
        }
        let scoped_tags: Vec<_> = remote
            .tags
            .iter()
            .filter(|tag| {
                tag.starts_with("fixture-family:")
                    || tag.starts_with("environment-family:")
                    || tag.starts_with("runtime-family:")
            })
            .collect();
        for tag in &scoped_tags {
            let weight = 0.20 / scoped_tags.len().max(1) as f64;
            possible += weight;
            if local.tags.contains(tag) {
                earned += weight;
                matches.push(ContextMatch {
                    field: "environment_family".into(),
                    remote: (*tag).clone(),
                    local: (*tag).clone(),
                })
            } else {
                mismatches.push(ContextMismatch {
                    field: "environment_family".into(),
                    remote: (*tag).clone(),
                    local: "missing or different".into(),
                    severity: "context_difference".into(),
                })
            }
        }
        for (name, value) in &remote.versions {
            let local_value = local.environment.facts.get(name).map(String::as_str);
            field(
                name,
                Some(value),
                local_value,
                0.25 / remote.versions.len().max(1) as f64,
                true,
                &mut earned,
                &mut possible,
                &mut matches,
                &mut mismatches,
                &mut unknowns,
            );
        }
        let score = if possible == 0.0 {
            0.0
        } else {
            f64::clamp(earned / possible, 0.0, 1.0)
        };
        ContextCompatibility {
            score: (score * 100.0).round() / 100.0,
            matches,
            mismatches,
            unknowns,
        }
    }
}

impl ExperienceBundle {
    pub fn object_count(&self) -> usize {
        self.experiences.len()
            + self.lessons.len()
            + self.skills.len()
            + self.experiments.len()
            + self.reflexes.len()
            + self.recoveries.len()
            + self.envelopes.len()
    }
    pub(crate) fn id_material(&self) -> Result<Vec<u8>> {
        let mut clone = self.clone();
        clone.manifest.bundle_id = BundleId::from_digest(&"0".repeat(64))?;
        serde_json::to_vec(&clone).map_err(Into::into)
    }
    pub fn computed_id(&self) -> Result<BundleId> {
        BundleId::from_digest(blake3::hash(&self.id_material()?).to_hex().as_ref())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(Into::into)
    }
    pub fn validate(&self, limits: &BundleLimits) -> Result<()> {
        if self.manifest.schema_version != BUNDLE_SCHEMA_V1 {
            return Err(Error::InvalidInput(
                "Unsupported bundle schema; unsafe downgrade refused".into(),
            ));
        }
        if self.object_count() > limits.max_objects
            || self.manifest.evidence_count > limits.max_objects
        {
            return Err(Error::InvalidInput("Bundle object limit exceeded".into()));
        }
        if self.manifest.bundle_id != self.computed_id()? {
            return Err(Error::InvalidInput(
                "Content-addressed bundle ID mismatch".into(),
            ));
        }
        if self.manifest.producer.to_string().is_empty() {
            return Err(Error::InvalidInput("Bundle producer missing".into()));
        }
        Ok(())
    }
}

pub fn context_from_selector(selector: &ContextSelector) -> EvidenceContext {
    EvidenceContext {
        repository_family: selector
            .repository
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned()),
        markers: selector.required_markers.clone(),
        tags: selector.tags.clone(),
        os: selector.os.clone(),
        arch: selector.arch.clone(),
        ..Default::default()
    }
}
