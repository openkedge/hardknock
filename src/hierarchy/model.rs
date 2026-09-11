// SPDX-License-Identifier: Apache-2.0
use super::{KnowledgeApplicability, KnowledgeScope};
pub use crate::abstraction::{
    AbstractionFreshnessStatus as FreshnessStatus, KnowledgeArtifactKind, KnowledgeArtifactRef,
    KnowledgeMaturity, KnowledgeProvenance,
};
pub use crate::epistemic::ExperienceActivationState as KnowledgeActivationState;
use crate::{core::*, epistemic::EvidenceRef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeHierarchy {
    pub id: KnowledgeHierarchyId,
    pub name: String,
    pub root_nodes: Vec<KnowledgeNodeId>,
    pub nodes: BTreeMap<KnowledgeNodeId, KnowledgeHierarchyNode>,
    pub edges: Vec<KnowledgeHierarchyEdge>,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeHierarchyNode {
    pub id: KnowledgeNodeId,
    pub artifact: KnowledgeArtifactRef,
    pub scope: KnowledgeScope,
    pub maturity: KnowledgeMaturity,
    pub freshness: FreshnessStatus,
    pub activation: KnowledgeActivationState,
    pub provenance: KnowledgeProvenance,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeHierarchyEdge {
    pub id: KnowledgeHierarchyEdgeId,
    pub parent: KnowledgeNodeId,
    pub child: KnowledgeNodeId,
    pub relation: KnowledgeHierarchyRelation,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeHierarchyRelation {
    /// Narrower child becomes primary; parent remains supporting context.
    Specializes,
    /// Child adds independent detail; both remain effective.
    Refines,
    /// Supported child changes the parent directive within an overlapping scope.
    Excepts,
    /// Explicit replacement: parent is OLD, child is NEW. Time alone has no effect.
    Supersedes,
    /// Child depends on parent's health; never establishes inheritance precedence.
    DependsOn,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeResolutionPolicy {
    pub minimum_primary_maturity: KnowledgeMaturity,
    pub allow_stale_advisory: bool,
    pub allow_candidate_advisory: bool,
    /// Reserved for compatibility. Pass 1 never permits stale exceptions to override.
    pub stale_exception_can_override: bool,
}
impl Default for KnowledgeResolutionPolicy {
    fn default() -> Self {
        Self {
            minimum_primary_maturity: KnowledgeMaturity::Validated,
            allow_stale_advisory: true,
            allow_candidate_advisory: true,
            stale_exception_can_override: false,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectiveKnowledge {
    pub applied: Vec<AppliedKnowledge>,
    /// Visible but never authoritative and never used for precedence.
    pub advisory: Vec<AppliedKnowledge>,
    pub suppressed: Vec<SuppressedKnowledge>,
    pub unknown: Vec<UnknownKnowledge>,
    pub conflicts: Vec<KnowledgeConflict>,
    pub trace: Vec<KnowledgeResolutionStep>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppliedKnowledge {
    pub artifact: KnowledgeArtifactRef,
    pub node: KnowledgeNodeId,
    pub role: AppliedKnowledgeRole,
    pub applicability: KnowledgeApplicability,
    pub lineage: Vec<KnowledgeNodeId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedKnowledgeRole {
    Primary,
    Refinement,
    Exception,
    SupportingContext,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuppressedKnowledge {
    pub artifact: KnowledgeArtifactRef,
    pub node: KnowledgeNodeId,
    pub reason: SuppressionReason,
    pub suppressed_by: Option<KnowledgeNodeId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    ExplicitException,
    MoreSpecificSpecialization,
    Superseded,
    StaleOverride,
    Contradicted,
    Inactive,
    Inapplicable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnknownKnowledge {
    pub artifact: KnowledgeArtifactRef,
    pub node: KnowledgeNodeId,
    pub reason: UnknownKnowledgeReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownKnowledgeReason {
    MissingContext,
    UnsupportedPredicate,
    IncompleteScopeMatch,
    FreshnessUnknown,
    EvidenceStateUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeConflict {
    pub artifacts: Vec<KnowledgeArtifactRef>,
    pub nodes: Vec<KnowledgeNodeId>,
    pub kind: KnowledgeConflictKind,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeConflictKind {
    CompetingExceptions,
    CompetingSpecializations,
    ContradictoryGuidance,
    AmbiguousSupersession,
    ScopeOverlap,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeResolutionStep {
    pub sequence: u64,
    pub node: KnowledgeNodeId,
    pub action: ResolutionAction,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionAction {
    CandidateFound,
    ScopeMatched,
    ScopeRejected,
    ScopeUnknown,
    LifecycleRejected,
    Applied,
    AppliedAsRefinement,
    AppliedAsException,
    Suppressed,
    Superseded,
    ConflictRaised,
}
