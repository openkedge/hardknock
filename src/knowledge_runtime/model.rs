// SPDX-License-Identifier: Apache-2.0
use crate::{core::*, hierarchy::*, runtime::RecoveryRef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KnowledgeRevisionRef {
    pub artifact: KnowledgeArtifactRef,
    pub revision: u64,
}
impl From<&KnowledgeArtifactRef> for KnowledgeRevisionRef {
    fn from(a: &KnowledgeArtifactRef) -> Self {
        Self {
            artifact: a.clone(),
            revision: a.revision,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyRevisionRef {
    pub id: KnowledgeHierarchyId,
    pub revision: u64,
    pub content_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeSnapshot {
    pub id: KnowledgeSnapshotId,
    pub created_at: DateTime<Utc>,
    pub hierarchies: Vec<HierarchyRevisionRef>,
    pub artifact_revisions: Vec<KnowledgeRevisionRef>,
    pub resolution_policy_version: String,
    pub policy: KnowledgeResolutionPolicy,
    pub fingerprint: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeSnapshotRef {
    pub id: KnowledgeSnapshotId,
    pub fingerprint: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedKnowledgeRef {
    pub knowledge: KnowledgeRevisionRef,
    pub node: KnowledgeNodeId,
    pub role: AppliedKnowledgeRole,
    pub lineage: Vec<KnowledgeNodeId>,
    pub statement: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedRecovery {
    pub knowledge: ResolvedKnowledgeRef,
    pub executable: Option<RecoveryRef>,
}
pub type ResolvedSkill = ResolvedKnowledgeRef;
pub type ResolvedLesson = ResolvedKnowledgeRef;
pub type ResolvedConstraint = ResolvedKnowledgeRef;
pub type ResolvedAntiPattern = ResolvedKnowledgeRef;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationalKnowledgeRevision {
    pub knowledge: KnowledgeRevisionRef,
    pub statement: String,
    pub recovery: Option<RecoveryRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeKnowledgeResolution {
    pub effective: EffectiveKnowledge,
    pub skills: Vec<ResolvedSkill>,
    pub lessons: Vec<ResolvedLesson>,
    pub constraints: Vec<ResolvedConstraint>,
    pub antipatterns: Vec<ResolvedAntiPattern>,
    pub recoveries: Vec<ResolvedRecovery>,
    pub unresolved_conflicts: Vec<KnowledgeConflict>,
    pub snapshot: KnowledgeSnapshotRef,
    pub context: KnowledgeContext,
    pub context_conflicts: Vec<ContextConflict>,
    pub provenance: RuntimeKnowledgeProvenance,
    pub validity: KnowledgeGuidanceValidity,
    pub bundle: HierarchyContextBundle,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeKnowledgeProvenance {
    pub snapshot_id: KnowledgeSnapshotId,
    pub hierarchy_revisions: Vec<HierarchyRevisionRef>,
    pub resolution_policy_version: String,
    pub applied_artifacts: Vec<KnowledgeRevisionRef>,
    pub suppressed_artifacts: Vec<KnowledgeRevisionRef>,
    pub conflicts: Vec<KnowledgeConflictId>,
    pub resolution_id: KnowledgeResolutionId,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HierarchyContextBundle {
    pub primary_knowledge: Vec<ResolvedKnowledgeRef>,
    pub refinements: Vec<ResolvedKnowledgeRef>,
    pub exceptions: Vec<ResolvedKnowledgeRef>,
    pub constraints: Vec<ResolvedConstraint>,
    pub recoveries: Vec<ResolvedRecovery>,
    pub antipatterns: Vec<ResolvedAntiPattern>,
    pub known_unknowns: Vec<UnknownKnowledge>,
    pub conflicts: Vec<KnowledgeConflict>,
    pub provenance: Option<RuntimeKnowledgeProvenance>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeContextBudget {
    pub statement_chars: usize,
    pub lineage: usize,
    pub primary: usize,
    pub refinements: usize,
    pub exceptions: usize,
    pub constraints: usize,
    pub recoveries: usize,
    pub antipatterns: usize,
    pub conflicts: usize,
    pub unknowns: usize,
}
impl Default for KnowledgeContextBudget {
    fn default() -> Self {
        Self {
            statement_chars: 1024,
            lineage: 8,
            primary: 3,
            refinements: 3,
            exceptions: 3,
            constraints: 3,
            recoveries: 2,
            antipatterns: 2,
            conflicts: 2,
            unknowns: 3,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeGuidanceValidity {
    pub snapshot: KnowledgeSnapshotId,
    pub hierarchy_revisions: Vec<HierarchyRevisionRef>,
    pub context_hash: String,
    pub expires_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeResolutionRecord {
    pub id: KnowledgeResolutionId,
    pub snapshot: KnowledgeSnapshotId,
    pub context_hash: String,
    pub context: KnowledgeContext,
    pub context_conflicts: Vec<ContextConflict>,
    pub effective: EffectiveKnowledge,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeApplication {
    pub id: KnowledgeApplicationId,
    pub knowledge: KnowledgeRevisionRef,
    pub resolution: KnowledgeResolutionId,
    pub role: AppliedKnowledgeRole,
    pub runtime_decision: RuntimeDecisionId,
    pub outcome: Option<KnowledgeApplicationOutcome>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeApplicationOutcome {
    Helpful,
    Harmful,
    Neutral,
    Inconclusive,
    FalseConstraint,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeKnowledgeMetrics {
    pub constraint_applications: u64,
    pub antipattern_applications: u64,
    pub recovery_applications: u64,
    pub exception_applications: u64,
    pub parent_fallbacks: u64,
    pub knowledge_conflicts: u64,
    pub unknown_overrides_prevented: u64,
    pub false_constraint_applications: u64,
    pub harmful_exception_applications: u64,
    pub stale_knowledge_prevented: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextValueSource {
    Imported,
    AgentReported,
    UserProvided,
    AdapterObserved,
    RuntimeObserved,
    EffectAdapterObserved,
    ToolAttestation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextValue {
    pub value: ScopeValue,
    pub source: ContextValueSource,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextConflict {
    pub key: String,
    pub values: Vec<ContextValue>,
    pub effective: Option<ContextValue>,
}
pub type ContextObservations = BTreeMap<String, Vec<ContextValue>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustedKnowledgeContext {
    pub context: KnowledgeContext,
    pub sources: BTreeMap<String, ContextValueSource>,
    pub conflicts: Vec<ContextConflict>,
    pub unverified: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredKnowledgeConflict {
    pub id: KnowledgeConflictId,
    pub conflict: KnowledgeConflict,
    pub snapshot: KnowledgeSnapshotId,
    pub context: KnowledgeContext,
    pub resolved: bool,
}
