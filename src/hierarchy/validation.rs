// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::KnowledgeNodeId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeHierarchyValidationReport {
    pub valid: bool,
    pub errors: Vec<HierarchyValidationIssue>,
    pub warnings: Vec<HierarchyValidationIssue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HierarchyValidationIssue {
    pub kind: HierarchyValidationIssueKind,
    pub message: String,
    pub nodes: Vec<KnowledgeNodeId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyValidationIssueKind {
    Cycle,
    SelfReference,
    MissingNode,
    InvalidSpecializationScope,
    InvalidExceptionScope,
    SupersessionCycle,
    IncompatibleArtifactKinds,
    OrphanedReference,
    UnknownScopeRelationship,
}
/// Derived ordered indexes; never persisted. Edge order in input is immaterial.
pub struct KnowledgeHierarchyIndex<'a> {
    pub children: BTreeMap<KnowledgeNodeId, Vec<&'a KnowledgeHierarchyEdge>>,
    pub parents: BTreeMap<KnowledgeNodeId, Vec<&'a KnowledgeHierarchyEdge>>,
}
impl<'a> KnowledgeHierarchyIndex<'a> {
    pub fn new(h: &'a KnowledgeHierarchy) -> Self {
        let mut index = Self {
            children: BTreeMap::new(),
            parents: BTreeMap::new(),
        };
        let mut edges: Vec<_> = h.edges.iter().collect();
        edges.sort_by_key(|e| (&e.parent, &e.child, e.relation, &e.id));
        for e in edges {
            index.children.entry(e.parent.clone()).or_default().push(e);
            index.parents.entry(e.child.clone()).or_default().push(e);
        }
        index
    }
    pub fn topological(
        &self,
        h: &KnowledgeHierarchy,
        select: impl Fn(KnowledgeHierarchyRelation) -> bool,
    ) -> Vec<KnowledgeNodeId> {
        let mut counts: BTreeMap<_, usize> = h.nodes.keys().map(|id| (id.clone(), 0)).collect();
        for edge in &h.edges {
            if select(edge.relation)
                && h.nodes.contains_key(&edge.parent)
                && let Some(n) = counts.get_mut(&edge.child)
            {
                *n += 1;
            }
        }
        let mut ready: BTreeSet<_> = counts
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut order = Vec::new();
        while let Some(id) = ready.pop_first() {
            if let Some(edges) = self.children.get(&id) {
                for e in edges {
                    if select(e.relation)
                        && let Some(n) = counts.get_mut(&e.child)
                    {
                        *n -= 1;
                        if *n == 0 {
                            ready.insert(e.child.clone());
                        }
                    }
                }
            }
            order.push(id);
        }
        order
    }
}
fn concrete(kind: KnowledgeArtifactKind) -> KnowledgeArtifactKind {
    use crate::abstraction::AbstractKnowledgeKind::*;
    use KnowledgeArtifactKind::*;
    match kind {
        AbstractKnowledge(k) => match k {
            AbstractLesson => Lesson,
            AbstractSkill => Skill,
            AbstractConstraint => Constraint,
            AbstractAntiPattern => AntiPattern,
            AbstractRecovery => Recovery,
        },
        k => k,
    }
}
pub fn validate_hierarchy(h: &KnowledgeHierarchy) -> KnowledgeHierarchyValidationReport {
    use HierarchyValidationIssueKind::*;
    use KnowledgeHierarchyRelation::*;
    let mut r = KnowledgeHierarchyValidationReport {
        valid: true,
        errors: vec![],
        warnings: vec![],
    };
    let mut issue = |kind, message: String, nodes: Vec<KnowledgeNodeId>, warning| {
        let item = HierarchyValidationIssue {
            kind,
            message,
            nodes,
        };
        if warning {
            r.warnings.push(item)
        } else {
            r.errors.push(item)
        }
    };
    let index = KnowledgeHierarchyIndex::new(h);
    let mut edge_ids = BTreeSet::new();
    let mut relations = BTreeSet::new();
    for (key, node) in &h.nodes {
        if key != &node.id {
            issue(
                OrphanedReference,
                "Node map key differs from node identity".into(),
                vec![key.clone(), node.id.clone()],
                false,
            )
        }
        if node.artifact.id.is_empty() || node.artifact.revision == 0 {
            issue(
                OrphanedReference,
                "Artifact requires identity and positive revision".into(),
                vec![key.clone()],
                false,
            )
        }
    }
    let mut roots = BTreeSet::new();
    for root in &h.root_nodes {
        if !h.nodes.contains_key(root) {
            issue(
                MissingNode,
                "Root not found".into(),
                vec![root.clone()],
                false,
            )
        }
        if !roots.insert(root.clone()) {
            issue(
                OrphanedReference,
                "Duplicate root".into(),
                vec![root.clone()],
                false,
            )
        }
    }
    for edges in index.children.values() {
        for e in edges {
            let ids = vec![e.parent.clone(), e.child.clone()];
            if !edge_ids.insert(e.id.clone())
                || !relations.insert((e.parent.clone(), e.child.clone(), e.relation))
            {
                issue(
                    OrphanedReference,
                    "Duplicate edge identity or relationship".into(),
                    ids.clone(),
                    false,
                )
            }
            if e.parent == e.child {
                issue(
                    SelfReference,
                    "Self relationship is invalid".into(),
                    ids.clone(),
                    false,
                )
            }
            let (Some(parent), Some(child)) = (h.nodes.get(&e.parent), h.nodes.get(&e.child))
            else {
                issue(MissingNode, "Edge endpoint not found".into(), ids, false);
                continue;
            };
            if e.relation == DependsOn {
                continue;
            }
            if concrete(parent.artifact.kind) != concrete(child.artifact.kind) {
                issue(
                    IncompatibleArtifactKinds,
                    "Cross-kind precedence is not modeled; use DependsOn".into(),
                    ids.clone(),
                    false,
                )
            }
            let relation = DeterministicScopeRelationEvaluator.compare(&child.scope, &parent.scope);
            match e.relation {
                Specializes
                    if matches!(
                        relation,
                        ScopeRelation::Broader
                            | ScopeRelation::Disjoint
                            | ScopeRelation::Overlapping
                    ) =>
                {
                    issue(
                        InvalidSpecializationScope,
                        "Specialization must be contained in parent scope".into(),
                        ids,
                        false,
                    )
                }
                Excepts
                    if matches!(
                        relation,
                        ScopeRelation::Disjoint
                            | ScopeRelation::Broader
                            | ScopeRelation::Overlapping
                    ) =>
                {
                    issue(
                        InvalidExceptionScope,
                        "Exception must be contained in parent scope".into(),
                        ids,
                        false,
                    )
                }
                _ if relation == ScopeRelation::Unknown => issue(
                    UnknownScopeRelationship,
                    "Cannot prove scope relationship; edge cannot override".into(),
                    ids,
                    true,
                ),
                Supersedes if relation == ScopeRelation::Disjoint => issue(
                    UnknownScopeRelationship,
                    "Supersession scopes do not overlap".into(),
                    ids,
                    true,
                ),
                _ => {}
            }
        }
    }
    for id in h.nodes.keys() {
        let incoming = index
            .parents
            .get(id)
            .is_some_and(|es| es.iter().any(|e| e.relation != DependsOn));
        if incoming == roots.contains(id) {
            issue(
                OrphanedReference,
                "Roots must exactly identify nodes without incoming precedence edges".into(),
                vec![id.clone()],
                false,
            )
        }
    }
    for (kind, select) in [(SupersessionCycle, 0), (Cycle, 1)] {
        let order = index.topological(h, |r| {
            if select == 0 {
                r == Supersedes
            } else {
                r != DependsOn
            }
        });
        if order.len() != h.nodes.len() {
            let visited: BTreeSet<_> = order.into_iter().collect();
            issue(
                kind,
                "Cycle in precedence graph".into(),
                h.nodes
                    .keys()
                    .filter(|id| !visited.contains(*id))
                    .cloned()
                    .collect(),
                false,
            )
        }
    }
    if index.topological(h, |r| r == DependsOn).len() != h.nodes.len() {
        issue(
            Cycle,
            "Cyclic health dependencies are not independently supported".into(),
            vec![],
            true,
        )
    }
    r.valid = r.errors.is_empty();
    r
}
