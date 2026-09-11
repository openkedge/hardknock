// SPDX-License-Identifier: Apache-2.0
use super::KnowledgeRevisionRef;
use crate::{Result, hierarchy::*};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportOrigin {
    Inherited,
    Independent,
    Mixed,
}
pub type KnowledgeHealthStatus = FreshnessStatus;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeHealthChange {
    pub artifact: KnowledgeArtifactRef,
    pub previous: KnowledgeHealthStatus,
    pub next: KnowledgeHealthStatus,
    pub reason: String,
}
pub trait KnowledgeHealthPropagation {
    fn evaluate(
        &self,
        hierarchy: &KnowledgeHierarchy,
        changed: KnowledgeRevisionRef,
    ) -> Result<Vec<KnowledgeHealthChange>>;
}
#[derive(Default)]
pub struct DefaultKnowledgeHealthPropagation;
pub fn support_origin(
    parent: &KnowledgeHierarchyNode,
    child: &KnowledgeHierarchyNode,
) -> SupportOrigin {
    let independent = child
        .provenance
        .evidence
        .iter()
        .any(|e| !parent.provenance.evidence.contains(e));
    let inherited = child
        .provenance
        .evidence
        .iter()
        .any(|e| parent.provenance.evidence.contains(e));
    match (independent, inherited) {
        (true, true) => SupportOrigin::Mixed,
        (true, false) => SupportOrigin::Independent,
        _ => SupportOrigin::Inherited,
    }
}
pub fn health_projection(
    h: &KnowledgeHierarchy,
) -> (KnowledgeHierarchy, Vec<KnowledgeHealthChange>) {
    let mut projected = h.clone();
    let mut changes = vec![];
    let index = KnowledgeHierarchyIndex::new(h);
    for id in index.topological(h, |r| r != KnowledgeHierarchyRelation::DependsOn) {
        if let Some(edges) = index.parents.get(&id) {
            for e in edges {
                let parent = &projected.nodes[&e.parent];
                let child = &projected.nodes[&id];
                if (parent.freshness != FreshnessStatus::Fresh
                    || matches!(
                        parent.maturity,
                        KnowledgeMaturity::Contradicted | KnowledgeMaturity::Retired
                    ))
                    && support_origin(parent, child) == SupportOrigin::Inherited
                    && child.freshness == FreshnessStatus::Fresh
                {
                    changes.push(KnowledgeHealthChange{artifact:child.artifact.clone(),previous:child.freshness,next:FreshnessStatus::Unknown,reason:format!("Inherited-only support requires revalidation after parent {} health changed",parent.id)});
                    projected
                        .nodes
                        .get_mut(&id)
                        .expect("indexed child")
                        .freshness = FreshnessStatus::Unknown;
                }
            }
        }
    }
    (projected, changes)
}
impl KnowledgeHealthPropagation for DefaultKnowledgeHealthPropagation {
    fn evaluate(
        &self,
        h: &KnowledgeHierarchy,
        changed: KnowledgeRevisionRef,
    ) -> Result<Vec<KnowledgeHealthChange>> {
        if !h
            .nodes
            .values()
            .any(|n| n.artifact == changed.artifact && n.artifact.revision == changed.revision)
        {
            return Err(crate::Error::InvalidInput(
                "Changed revision is not in hierarchy".into(),
            ));
        }
        Ok(health_projection(h).1)
    }
}
