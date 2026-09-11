// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    assurance::{EvidenceManifest, PolicyVersions},
    core::*,
    epistemic::EvidenceRef,
    hierarchy::*,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardRef {
    pub id: String,
    pub revision: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceRelevance {
    None,
    Advisory,
    GuardCandidate,
    ExistingGuardDependency,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardRevisionReason {
    NewValidatedConstraint,
    ValidatedException,
    ConstraintScopeNarrowed,
    ConstraintScopeExpanded,
    ConstraintSuperseded,
    ContradictionDiscovered,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardRevisionChange {
    AddGuardCandidate { constraint: KnowledgeRevisionRef },
    NarrowScope { scope: KnowledgeScope },
    ExpandScope { scope: KnowledgeScope },
    AddException { exception: KnowledgeRevisionRef },
    RetireGuard,
    ReviewRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardRevisionCandidateStatus {
    Proposed,
    ReadyForReview,
    AcceptedExternally,
    RejectedExternally,
    Superseded,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuardEvidenceManifest {
    pub manifest: EvidenceManifest,
    pub hierarchy: HierarchyRevisionRef,
    pub knowledge: Vec<KnowledgeRevisionRef>,
    pub evidence: Vec<EvidenceRef>,
    pub scope: KnowledgeScope,
    pub hash: String,
}
impl GuardEvidenceManifest {
    pub fn seal(&mut self) -> Result<()> {
        self.manifest.seal()?;
        self.hash.clear();
        self.hash = content_hash(self)?;
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        self.manifest.verify_hash()?;
        let mut copy = self.clone();
        copy.hash.clear();
        if content_hash(&copy)? != self.hash {
            return Err(Error::InvalidInput(
                "Guard evidence manifest hash mismatch".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuardRevisionCandidate {
    pub id: GuardRevisionCandidateId,
    pub source_guard: Option<GuardRef>,
    pub source_knowledge: Vec<KnowledgeRevisionRef>,
    pub reason: GuardRevisionReason,
    pub proposed_change: GuardRevisionChange,
    pub evidence: GuardEvidenceManifest,
    pub status: GuardRevisionCandidateStatus,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuardRevisionArtifact {
    pub schema: String,
    pub candidate: GuardRevisionCandidate,
    pub enforcement_changed: bool,
    pub hash: String,
}
impl GuardRevisionArtifact {
    pub fn export(candidate: GuardRevisionCandidate) -> Result<Self> {
        let mut artifact = Self {
            schema: "hardknock.guard-revision.v1".into(),
            candidate,
            enforcement_changed: false,
            hash: String::new(),
        };
        artifact.hash = content_hash(&artifact)?;
        Ok(artifact)
    }
    pub fn verify(&self) -> Result<()> {
        self.candidate.evidence.verify()?;
        let mut copy = self.clone();
        copy.hash.clear();
        if self.schema != "hardknock.guard-revision.v1"
            || self.enforcement_changed
            || self.candidate.source_knowledge != self.candidate.evidence.knowledge
            || self.hash != content_hash(&copy)?
        {
            return Err(Error::InvalidInput(
                "Guard revision artifact integrity failure".into(),
            ));
        }
        Ok(())
    }
}
pub fn guard_revision_candidate(
    h: &KnowledgeHierarchy,
    node: &KnowledgeNodeId,
    guard: Option<GuardRef>,
) -> Result<GuardRevisionCandidate> {
    if !validate_hierarchy(h).valid {
        return Err(Error::InvalidInput("Invalid hierarchy".into()));
    }
    let n = h
        .nodes
        .get(node)
        .ok_or_else(|| Error::NotFound("Knowledge node not found".into()))?;
    if !matches!(
        n.artifact.kind,
        KnowledgeArtifactKind::Constraint
            | KnowledgeArtifactKind::AbstractKnowledge(
                crate::abstraction::AbstractKnowledgeKind::AbstractConstraint
            )
    ) {
        return Err(Error::InvalidInput(
            "Guard candidates require Constraint knowledge".into(),
        ));
    }
    let projected = health_projection(h).0;
    if projected
        .nodes
        .get(node)
        .is_some_and(|p| p.freshness == FreshnessStatus::Unknown)
    {
        return Err(Error::InvalidInput(
            "Inherited evidence requires revalidation before Guard review".into(),
        ));
    }
    let contradicted = n.maturity == KnowledgeMaturity::Contradicted
        || n.freshness == FreshnessStatus::Contradicted;
    if !contradicted
        && (n.maturity != KnowledgeMaturity::Validated
            || n.freshness != FreshnessStatus::Fresh
            || n.activation != KnowledgeActivationState::Active)
    {
        return Err(Error::InvalidInput(
            "Guard candidate needs fresh active validated evidence or an explicit contradiction"
                .into(),
        ));
    }
    if n.provenance.evidence.is_empty() {
        return Err(Error::InvalidInput(
            "Guard candidate requires evidence".into(),
        ));
    }
    let knowledge = KnowledgeRevisionRef::from(&n.artifact);
    let exception = h.edges.iter().any(|e| {
        e.child == *node
            && e.relation == KnowledgeHierarchyRelation::Excepts
            && !e.evidence.is_empty()
    });
    let (reason, change) = if contradicted {
        (
            GuardRevisionReason::ContradictionDiscovered,
            GuardRevisionChange::ReviewRequired,
        )
    } else if exception {
        (
            GuardRevisionReason::ValidatedException,
            GuardRevisionChange::AddException {
                exception: knowledge.clone(),
            },
        )
    } else {
        (
            GuardRevisionReason::NewValidatedConstraint,
            GuardRevisionChange::AddGuardCandidate {
                constraint: knowledge.clone(),
            },
        )
    };
    let manifest: EvidenceManifest = serde_json::from_value(
        serde_json::json!({"id":EvidenceManifestId::new(),"subject":{"kind":"knowledge","subject":knowledge},"generated_at":Utc::now(),"policy_versions":PolicyVersions::default(),"summary":crate::assurance::AssuranceEvidenceSummary::default(),"evidence_hash":""}),
    )?;
    let mut evidence = GuardEvidenceManifest {
        manifest,
        hierarchy: HierarchyRevisionRef {
            id: h.id.clone(),
            revision: h.revision,
            content_hash: content_hash(h)?,
        },
        knowledge: vec![knowledge.clone()],
        evidence: n.provenance.evidence.clone(),
        scope: n.scope.clone(),
        hash: String::new(),
    };
    evidence.evidence.extend(
        h.edges
            .iter()
            .filter(|e| e.child == *node)
            .flat_map(|e| e.evidence.clone()),
    );
    evidence.evidence.sort();
    evidence.evidence.dedup();
    evidence.seal()?;
    Ok(GuardRevisionCandidate {
        id: GuardRevisionCandidateId::new(),
        source_guard: guard,
        source_knowledge: vec![knowledge],
        reason,
        proposed_change: change,
        evidence,
        status: GuardRevisionCandidateStatus::ReadyForReview,
        created_at: Utc::now(),
    })
}
