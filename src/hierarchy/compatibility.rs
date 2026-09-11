// SPDX-License-Identifier: Apache-2.0
//! Loss-aware adapters. Unsupported V0.17 set/absence semantics remain Unknown.
use super::*;
use crate::{abstraction::*, core::*, lesson::ContextSelector};
impl From<&ApplicabilityPredicate> for KnowledgeScope {
    fn from(source: &ApplicabilityPredicate) -> Self {
        let predicates = source
            .all_of
            .iter()
            .map(|clause| {
                let value = match &clause.value {
                    Some(VariableValue::Text(v)) => Some(ScopeValue::String(v.clone())),
                    Some(VariableValue::Integer(v)) => Some(ScopeValue::Integer(*v)),
                    Some(VariableValue::Boolean(v)) => Some(ScopeValue::Boolean(*v)),
                    _ => None,
                };
                match (clause.operator, value) {
                    (PredicateOperator::Equals, Some(value)) => ScopePredicate::Equals {
                        key: clause.name.clone(),
                        value,
                    },
                    (PredicateOperator::NotEquals, Some(value)) => ScopePredicate::NotEquals {
                        key: clause.name.clone(),
                        value,
                    },
                    (PredicateOperator::Present, _) => ScopePredicate::Exists {
                        key: clause.name.clone(),
                    },
                    _ => ScopePredicate::Custom {
                        kind: "v017_applicability_clause".into(),
                        payload: serde_json::to_value(clause).expect("clause serialization"),
                    },
                }
            })
            .collect();
        Self { predicates }
    }
}
impl From<&ContextSelector> for KnowledgeScope {
    fn from(source: &ContextSelector) -> Self {
        let mut predicates = vec![];
        for (key, value) in [
            ("os", source.os.clone()),
            ("arch", source.arch.clone()),
            (
                "repository",
                source.repository.as_ref().map(|p| p.display().to_string()),
            ),
        ] {
            if let Some(value) = value {
                predicates.push(ScopePredicate::Equals {
                    key: key.into(),
                    value: ScopeValue::String(value),
                })
            }
        }
        if !source.tags.is_empty() || !source.required_markers.is_empty() {
            predicates.push(ScopePredicate::Custom{kind:"v017_selector_sets".into(),payload:serde_json::json!({"tags":source.tags,"required_markers":source.required_markers})})
        }
        Self { predicates }
    }
}
impl KnowledgeArtifactRef {
    pub fn abstract_knowledge(knowledge: &AbstractKnowledge) -> Self {
        Self {
            kind: KnowledgeArtifactKind::AbstractKnowledge(knowledge.kind),
            id: knowledge.id.to_string(),
            revision: knowledge.revision,
        }
    }
}
impl KnowledgeHierarchyNode {
    /// Caller supplies assessed health; transfer maturity alone never proves freshness.
    pub fn from_abstract(
        id: KnowledgeNodeId,
        knowledge: &AbstractKnowledge,
        freshness: FreshnessStatus,
        activation: KnowledgeActivationState,
    ) -> Self {
        let mut scope = KnowledgeScope::from(&knowledge.applicability);
        scope.predicates.extend(
            KnowledgeScope::from(&ApplicabilityPredicate {
                all_of: knowledge.generalization_boundary.included.clone(),
            })
            .predicates,
        );
        if !knowledge.generalization_boundary.excluded.is_empty()
            || !knowledge.generalization_boundary.unknown.is_empty()
        {
            scope.predicates.push(ScopePredicate::Custom {
                kind: "v017_generalization_boundary".into(),
                payload: serde_json::to_value(&knowledge.generalization_boundary)
                    .expect("boundary serialization"),
            });
        }
        Self {
            id,
            artifact: KnowledgeArtifactRef::abstract_knowledge(knowledge),
            scope,
            maturity: knowledge.maturity,
            freshness,
            activation,
            provenance: knowledge.provenance.clone(),
        }
    }
}
impl KnowledgeHierarchyEdge {
    /// Project the existing relation; callers must use matching artifact revisions.
    pub fn from_specialization(
        id: KnowledgeHierarchyEdgeId,
        source: &KnowledgeSpecialization,
        parent: &KnowledgeHierarchyNode,
        child: &mut KnowledgeHierarchyNode,
    ) -> crate::Result<Self> {
        if parent.artifact.id != source.parent.id.to_string()
            || parent.artifact.revision != source.parent.revision
            || child.artifact.id != source.child.id.to_string()
            || child.artifact.revision != source.child.revision
        {
            return Err(crate::Error::InvalidInput(
                "Specialization artifact revisions do not match".into(),
            ));
        }
        child
            .scope
            .predicates
            .extend(KnowledgeScope::from(&source.additional_scope).predicates);
        Ok(Self {
            id,
            parent: parent.id.clone(),
            child: child.id.clone(),
            relation: KnowledgeHierarchyRelation::Specializes,
            evidence: source.evidence.clone(),
            created_at: source.created_at,
        })
    }
    /// V0.17 exceptions have no independent lifecycle or directive artifact. An
    /// explicit child is required; never invent validated state from the relation.
    pub fn from_exception(
        id: KnowledgeHierarchyEdgeId,
        source: &KnowledgeException,
        parent: &KnowledgeHierarchyNode,
        child: &mut KnowledgeHierarchyNode,
    ) -> crate::Result<Self> {
        if parent.artifact.id != source.parent.id.to_string()
            || parent.artifact.revision != source.parent.revision
        {
            return Err(crate::Error::InvalidInput(
                "Exception parent revision does not match".into(),
            ));
        }
        child
            .scope
            .predicates
            .extend(KnowledgeScope::from(&source.applicability).predicates);
        child
            .scope
            .predicates
            .extend(KnowledgeScope::from(&source.context).predicates);
        Ok(Self {
            id,
            parent: parent.id.clone(),
            child: child.id.clone(),
            relation: KnowledgeHierarchyRelation::Excepts,
            evidence: source.evidence.clone(),
            created_at: source.created_at,
        })
    }
}
