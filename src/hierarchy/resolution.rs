// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Error, Result, core::KnowledgeNodeId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub trait KnowledgeResolver {
    fn resolve(
        &self,
        hierarchy: &KnowledgeHierarchy,
        context: &KnowledgeContext,
        policy: &KnowledgeResolutionPolicy,
    ) -> Result<EffectiveKnowledge>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicKnowledgeResolver;
fn maturity_rank(m: KnowledgeMaturity) -> Option<u8> {
    use KnowledgeMaturity::*;
    match m {
        Candidate => Some(0),
        TransferTestable => Some(1),
        Supported => Some(2),
        Validated => Some(3),
        _ => None,
    }
}
fn step(
    out: &mut EffectiveKnowledge,
    id: &KnowledgeNodeId,
    action: ResolutionAction,
    reason: impl Into<String>,
) {
    out.trace.push(KnowledgeResolutionStep {
        sequence: out.trace.len() as u64 + 1,
        node: id.clone(),
        action,
        reason: reason.into(),
    });
}
fn suppress(
    out: &mut EffectiveKnowledge,
    n: &KnowledgeHierarchyNode,
    reason: SuppressionReason,
    by: Option<KnowledgeNodeId>,
) {
    step(
        out,
        &n.id,
        if reason == SuppressionReason::Superseded {
            ResolutionAction::Superseded
        } else {
            ResolutionAction::Suppressed
        },
        format!("{reason:?}; suppressor={by:?}"),
    );
    out.suppressed.push(SuppressedKnowledge {
        artifact: n.artifact.clone(),
        node: n.id.clone(),
        reason,
        suppressed_by: by,
    });
}
fn unknown(
    out: &mut EffectiveKnowledge,
    n: &KnowledgeHierarchyNode,
    reason: UnknownKnowledgeReason,
) {
    step(
        out,
        &n.id,
        ResolutionAction::LifecycleRejected,
        format!("{reason:?}; cannot override known guidance"),
    );
    out.unknown.push(UnknownKnowledge {
        artifact: n.artifact.clone(),
        node: n.id.clone(),
        reason,
    });
}
fn applied(
    n: &KnowledgeHierarchyNode,
    a: &KnowledgeApplicability,
    role: AppliedKnowledgeRole,
    lineage: Vec<KnowledgeNodeId>,
) -> AppliedKnowledge {
    AppliedKnowledge {
        artifact: n.artifact.clone(),
        node: n.id.clone(),
        role,
        applicability: a.clone(),
        lineage,
    }
}
impl KnowledgeResolver for DeterministicKnowledgeResolver {
    fn resolve(
        &self,
        h: &KnowledgeHierarchy,
        context: &KnowledgeContext,
        policy: &KnowledgeResolutionPolicy,
    ) -> Result<EffectiveKnowledge> {
        let validation = validate_hierarchy(h);
        if !validation.valid {
            return Err(Error::InvalidInput(format!(
                "Invalid knowledge hierarchy: {}",
                serde_json::to_string(&validation)?
            )));
        }
        let threshold = maturity_rank(policy.minimum_primary_maturity).ok_or_else(|| {
            Error::InvalidInput(
                "Minimum maturity must be a support stage, not a terminal state".into(),
            )
        })?;
        if policy.stale_exception_can_override {
            return Err(Error::InvalidInput(
                "Stale exception overrides are prohibited in Pass 1".into(),
            ));
        }
        let index = KnowledgeHierarchyIndex::new(h);
        let order = index.topological(h, |r| r != KnowledgeHierarchyRelation::DependsOn);
        let mut out = EffectiveKnowledge::default();
        let mut scopes = BTreeMap::new();
        let mut eligible = BTreeSet::new();
        // B/C: applicability and lifecycle, independently of precedence.
        for (id, n) in &h.nodes {
            step(
                &mut out,
                id,
                ResolutionAction::CandidateFound,
                format!(
                    "Candidate {} revision {}",
                    n.artifact.id, n.artifact.revision
                ),
            );
            let a = DeterministicApplicabilityEvaluator.evaluate(&n.scope, context);
            match a.status {
                ApplicabilityStatus::Inapplicable => {
                    step(
                        &mut out,
                        id,
                        ResolutionAction::ScopeRejected,
                        "At least one required predicate failed",
                    );
                    suppress(&mut out, n, SuppressionReason::Inapplicable, None);
                }
                ApplicabilityStatus::Unknown | ApplicabilityStatus::PartiallyKnown => {
                    step(
                        &mut out,
                        id,
                        ResolutionAction::ScopeUnknown,
                        "Incomplete applicability cannot override known parent guidance",
                    );
                    let reason = if a
                        .unknown
                        .iter()
                        .any(|p| matches!(p, ScopePredicate::Custom { .. }))
                    {
                        UnknownKnowledgeReason::UnsupportedPredicate
                    } else if !a.matched.is_empty() {
                        UnknownKnowledgeReason::IncompleteScopeMatch
                    } else {
                        UnknownKnowledgeReason::MissingContext
                    };
                    out.unknown.push(UnknownKnowledge {
                        artifact: n.artifact.clone(),
                        node: id.clone(),
                        reason,
                    });
                }
                ApplicabilityStatus::Applicable => {
                    step(
                        &mut out,
                        id,
                        ResolutionAction::ScopeMatched,
                        "All required predicates matched",
                    );
                    if n.freshness == FreshnessStatus::Contradicted
                        || n.maturity == KnowledgeMaturity::Contradicted
                    {
                        suppress(&mut out, n, SuppressionReason::Contradicted, None)
                    } else if matches!(
                        n.activation,
                        KnowledgeActivationState::Disabled | KnowledgeActivationState::Quarantined
                    ) || matches!(
                        n.maturity,
                        KnowledgeMaturity::Retired | KnowledgeMaturity::Overgeneralized
                    ) {
                        suppress(&mut out, n, SuppressionReason::Inactive, None)
                    } else if n.freshness == FreshnessStatus::Unknown {
                        unknown(&mut out, n, UnknownKnowledgeReason::FreshnessUnknown)
                    } else {
                        let fresh = n.freshness == FreshnessStatus::Fresh
                            && n.maturity != KnowledgeMaturity::Stale;
                        let mature =
                            maturity_rank(n.maturity).is_some_and(|rank| rank >= threshold);
                        let supported = !n.provenance.evidence.is_empty();
                        if n.activation == KnowledgeActivationState::Active
                            && fresh
                            && mature
                            && supported
                        {
                            eligible.insert(id.clone());
                            step(
                                &mut out,
                                id,
                                ResolutionAction::ScopeMatched,
                                "Active, fresh, sufficiently mature, evidence-backed, non-contradicted",
                            );
                        } else {
                            step(
                                &mut out,
                                id,
                                ResolutionAction::LifecycleRejected,
                                "Not eligible for authoritative precedence",
                            );
                            if !supported {
                                unknown(&mut out, n, UnknownKnowledgeReason::EvidenceStateUnknown)
                            } else if (fresh || policy.allow_stale_advisory)
                                && (mature
                                    || policy.allow_candidate_advisory
                                    || n.maturity == KnowledgeMaturity::Stale)
                            {
                                out.advisory.push(applied(
                                    n,
                                    &a,
                                    AppliedKnowledgeRole::SupportingContext,
                                    vec![],
                                ));
                            } else {
                                suppress(
                                    &mut out,
                                    n,
                                    if !fresh {
                                        SuppressionReason::StaleOverride
                                    } else {
                                        SuppressionReason::Inactive
                                    },
                                    None,
                                )
                            }
                        }
                    }
                }
            }
            scopes.insert(id.clone(), a);
        }
        // Health dependencies: seed unhealthy prerequisites, then propagate once.
        // Dependency cycles have no independent health proof and fail closed.
        let health_order = index.topological(h, |r| r == KnowledgeHierarchyRelation::DependsOn);
        let health_visited: BTreeSet<_> = health_order.into_iter().collect();
        let mut queue: VecDeque<_> = h
            .nodes
            .keys()
            .filter(|id| !eligible.contains(*id) || !health_visited.contains(*id))
            .cloned()
            .collect();
        let mut unhealthy = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            if !unhealthy.insert(id.clone()) {
                continue;
            }
            if eligible.remove(&id) {
                unknown(
                    &mut out,
                    &h.nodes[&id],
                    UnknownKnowledgeReason::EvidenceStateUnknown,
                )
            }
            if let Some(edges) = index.children.get(&id) {
                for e in edges {
                    if e.relation == KnowledgeHierarchyRelation::DependsOn {
                        queue.push_back(e.child.clone())
                    }
                }
            }
        }
        // Only proven, evidence-backed relationships can change precedence.
        let usable = |e: &&KnowledgeHierarchyEdge| {
            if e.relation == KnowledgeHierarchyRelation::DependsOn || e.evidence.is_empty() {
                return false;
            }
            let relation = DeterministicScopeRelationEvaluator
                .compare(&h.nodes[&e.child].scope, &h.nodes[&e.parent].scope);
            match e.relation {
                KnowledgeHierarchyRelation::Specializes | KnowledgeHierarchyRelation::Excepts => {
                    matches!(relation, ScopeRelation::Equal | ScopeRelation::Narrower)
                }
                _ => !matches!(relation, ScopeRelation::Unknown | ScopeRelation::Disjoint),
            }
        };
        let mut roles: BTreeMap<_, _> = eligible
            .iter()
            .map(|id| (id.clone(), AppliedKnowledgeRole::Primary))
            .collect();
        // E-H: reverse topological processing prevents an already replaced child
        // from suppressing its own parent. Conflicting alternatives cannot override.
        let mut conflicted = BTreeSet::new();
        for relation in [
            KnowledgeHierarchyRelation::Supersedes,
            KnowledgeHierarchyRelation::Excepts,
            KnowledgeHierarchyRelation::Specializes,
            KnowledgeHierarchyRelation::Refines,
        ] {
            // Discover all competing siblings before any mutation in this phase.
            // A multi-parent child must not override one parent before its conflict
            // under another parent is discovered.
            let conflict_relations: &[KnowledgeHierarchyRelation] = match relation {
                KnowledgeHierarchyRelation::Supersedes => &[KnowledgeHierarchyRelation::Supersedes],
                KnowledgeHierarchyRelation::Excepts => &[
                    KnowledgeHierarchyRelation::Excepts,
                    KnowledgeHierarchyRelation::Specializes,
                ],
                _ => &[],
            };
            for &conflict_relation in conflict_relations {
                for parent in &order {
                    if !eligible.contains(parent) {
                        continue;
                    }
                    let edges: Vec<_> = index
                        .children
                        .get(parent)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|e| e.relation == conflict_relation && eligible.contains(&e.child))
                        .filter(usable)
                        .collect();
                    if edges.len() <= 1 {
                        continue;
                    }
                    let kind = match conflict_relation {
                        KnowledgeHierarchyRelation::Supersedes => {
                            KnowledgeConflictKind::AmbiguousSupersession
                        }
                        KnowledgeHierarchyRelation::Excepts => {
                            KnowledgeConflictKind::CompetingExceptions
                        }
                        _ => KnowledgeConflictKind::CompetingSpecializations,
                    };
                    let nodes: Vec<_> = edges.iter().map(|e| e.child.clone()).collect();
                    for id in &nodes {
                        conflicted.insert(id.clone());
                        step(
                            &mut out,
                            id,
                            ResolutionAction::ConflictRaised,
                            "Competing applicable siblings; parent retained and no arbitrary winner",
                        );
                        roles.insert(id.clone(), AppliedKnowledgeRole::SupportingContext);
                    }
                    out.conflicts.push(KnowledgeConflict {
                        artifacts: nodes.iter().map(|id| h.nodes[id].artifact.clone()).collect(), nodes, kind,
                        reason: format!("Multiple eligible {conflict_relation:?} children of {parent}; explicit precedence required"),
                    });
                }
            }
            let phase_eligible = eligible.clone();
            let mut replacements = BTreeMap::<KnowledgeNodeId, KnowledgeNodeId>::new();
            for parent in order.iter().rev() {
                if !eligible.contains(parent) {
                    continue;
                }
                let edges: Vec<_> = index
                    .children
                    .get(parent)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|e| {
                        e.relation == relation
                            && !conflicted.contains(&e.child)
                            && (eligible.contains(&e.child)
                                || (relation == KnowledgeHierarchyRelation::Supersedes
                                    && phase_eligible.contains(&e.child)
                                    && replacements.contains_key(&e.child)))
                    })
                    .filter(usable)
                    .collect();
                for e in edges {
                    match relation {
                        KnowledgeHierarchyRelation::Supersedes
                        | KnowledgeHierarchyRelation::Excepts => {
                            // A child already marked ambiguous must not suppress another parent.
                            let suppressor = replacements.get(&e.child).unwrap_or(&e.child).clone();
                            replacements.insert(parent.clone(), suppressor.clone());
                            eligible.remove(parent);
                            suppress(
                                &mut out,
                                &h.nodes[parent],
                                if relation == KnowledgeHierarchyRelation::Supersedes {
                                    SuppressionReason::Superseded
                                } else {
                                    SuppressionReason::ExplicitException
                                },
                                Some(suppressor),
                            );
                            if relation == KnowledgeHierarchyRelation::Excepts {
                                roles.insert(e.child.clone(), AppliedKnowledgeRole::Exception);
                            }
                        }
                        KnowledgeHierarchyRelation::Specializes => {
                            roles.insert(parent.clone(), AppliedKnowledgeRole::SupportingContext);
                        }
                        KnowledgeHierarchyRelation::Refines
                            if roles[&e.child] == AppliedKnowledgeRole::Primary =>
                        {
                            roles.insert(e.child.clone(), AppliedKnowledgeRole::Refinement);
                        }
                        _ => {}
                    }
                }
            }
        }
        // Materialize lineage only for output; graph processing uses indexed edges.
        for id in &eligible {
            let mut ancestors = BTreeSet::new();
            let mut pending = vec![id.clone()];
            while let Some(current) = pending.pop() {
                if let Some(edges) = index.parents.get(&current) {
                    for e in edges {
                        if e.relation != KnowledgeHierarchyRelation::DependsOn
                            && ancestors.insert(e.parent.clone())
                        {
                            pending.push(e.parent.clone());
                        }
                    }
                }
            }
            let role = roles[id];
            step(
                &mut out,
                id,
                match role {
                    AppliedKnowledgeRole::Refinement => ResolutionAction::AppliedAsRefinement,
                    AppliedKnowledgeRole::Exception => ResolutionAction::AppliedAsException,
                    _ => ResolutionAction::Applied,
                },
                format!("Final role: {role:?}"),
            );
            out.applied.push(applied(
                &h.nodes[id],
                &scopes[id],
                role,
                ancestors.into_iter().collect(),
            ));
        }
        out.suppressed.sort_by_key(|s| s.node.clone());
        out.unknown.sort_by_key(|s| s.node.clone());
        out.advisory.sort_by_key(|s| s.node.clone());
        Ok(out)
    }
}
