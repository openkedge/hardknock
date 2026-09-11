// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Error, Result, core::*, hierarchy::*, runtime::RuntimeDecisionContext, store::Store};
use chrono::Utc;
pub trait RuntimeKnowledgeResolver {
    fn resolve_for_runtime(
        &self,
        runtime: &RuntimeDecisionContext,
    ) -> Result<RuntimeKnowledgeResolution>;
}
pub trait HistoricalKnowledgeResolver {
    fn resolve_snapshot(
        &self,
        snapshot: &KnowledgeSnapshot,
        context: &KnowledgeContext,
    ) -> Result<EffectiveKnowledge>;
}
pub struct DefaultRuntimeKnowledgeResolver<'a> {
    pub store: &'a Store,
    pub policy: KnowledgeResolutionPolicy,
    pub budget: KnowledgeContextBudget,
    pub persist: bool,
}
pub struct DefaultHistoricalKnowledgeResolver<'a> {
    pub store: &'a Store,
}
pub fn content_hash(value: &impl serde::Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn combine(
    hierarchies: &[KnowledgeHierarchy],
    context: &KnowledgeContext,
    policy: &KnowledgeResolutionPolicy,
) -> Result<EffectiveKnowledge> {
    let mut combined = EffectiveKnowledge::default();
    let mut identities = std::collections::BTreeSet::new();
    for h in hierarchies {
        if h.nodes.keys().any(|id| !identities.insert(id.clone())) {
            return Err(Error::InvalidInput(
                "Node identity shared by independently stored hierarchies".into(),
            ));
        }
        let (h, _) = health_projection(h);
        let r = DeterministicKnowledgeResolver.resolve(&h, context, policy)?;
        combined.applied.extend(r.applied);
        combined.advisory.extend(r.advisory);
        combined.suppressed.extend(r.suppressed);
        combined.unknown.extend(r.unknown);
        combined.conflicts.extend(r.conflicts);
        combined.trace.extend(r.trace);
    }
    for (i, step) in combined.trace.iter_mut().enumerate() {
        step.sequence = i as u64 + 1;
    }
    Ok(combined)
}
impl HistoricalKnowledgeResolver for DefaultHistoricalKnowledgeResolver<'_> {
    fn resolve_snapshot(
        &self,
        snapshot: &KnowledgeSnapshot,
        context: &KnowledgeContext,
    ) -> Result<EffectiveKnowledge> {
        combine(
            &self.store.snapshot_hierarchies(snapshot)?,
            context,
            &snapshot.policy,
        )
    }
}
impl RuntimeKnowledgeResolver for DefaultRuntimeKnowledgeResolver<'_> {
    fn resolve_for_runtime(
        &self,
        runtime: &RuntimeDecisionContext,
    ) -> Result<RuntimeKnowledgeResolution> {
        let trusted = DefaultKnowledgeContextBuilder.trusted(runtime);
        let (snapshot, hierarchies) = if self.persist {
            let snapshot = self.store.create_knowledge_snapshot(&self.policy)?;
            let hs = self.store.snapshot_hierarchies(&snapshot)?;
            (snapshot, hs)
        } else {
            let hs = self.store.knowledge_hierarchies()?;
            let refs = hs
                .iter()
                .map(|h| {
                    Ok(HierarchyRevisionRef {
                        id: h.id.clone(),
                        revision: h.revision,
                        content_hash: content_hash(h)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let artifacts: std::collections::BTreeSet<_> = hs
                .iter()
                .flat_map(|h| {
                    h.nodes
                        .values()
                        .map(|n| KnowledgeRevisionRef::from(&n.artifact))
                })
                .collect();
            let artifact_revisions: Vec<_> = artifacts.into_iter().collect();
            let fingerprint = content_hash(&(
                RESOLUTION_POLICY_VERSION,
                &refs,
                &artifact_revisions,
                &self.policy,
            ))?;
            (
                KnowledgeSnapshot {
                    id: KnowledgeSnapshotId::new(),
                    created_at: Utc::now(),
                    hierarchies: refs,
                    artifact_revisions,
                    resolution_policy_version: RESOLUTION_POLICY_VERSION.into(),
                    policy: self.policy.clone(),
                    fingerprint,
                },
                hs,
            )
        };
        let effective = combine(&hierarchies, &trusted.context, &self.policy)?;
        let resolution_id = KnowledgeResolutionId::new();
        let mut conflict_ids = vec![];
        for conflict in &effective.conflicts {
            let digest = content_hash(&(conflict, &snapshot.fingerprint))?;
            let uuid = uuid::Uuid::parse_str(&digest[..32])
                .map_err(|e| Error::InvalidInput(e.to_string()))?;
            let id: KnowledgeConflictId = format!("knowledge-conflict-{uuid}").parse()?;
            conflict_ids.push(id.clone());
            if self.persist {
                self.store
                    .save_knowledge_conflict(&StoredKnowledgeConflict {
                        id,
                        conflict: conflict.clone(),
                        snapshot: snapshot.id.clone(),
                        context: trusted.context.clone(),
                        resolved: false,
                    })?;
            }
        }
        let provenance = RuntimeKnowledgeProvenance {
            snapshot_id: snapshot.id.clone(),
            hierarchy_revisions: snapshot.hierarchies.clone(),
            resolution_policy_version: snapshot.resolution_policy_version.clone(),
            applied_artifacts: effective
                .applied
                .iter()
                .map(|a| KnowledgeRevisionRef::from(&a.artifact))
                .collect(),
            suppressed_artifacts: effective
                .suppressed
                .iter()
                .map(|a| KnowledgeRevisionRef::from(&a.artifact))
                .collect(),
            conflicts: conflict_ids,
            resolution_id: resolution_id.clone(),
        };
        let mut result = RuntimeKnowledgeResolution {
            skills: vec![],
            lessons: vec![],
            constraints: vec![],
            antipatterns: vec![],
            recoveries: vec![],
            unresolved_conflicts: effective.conflicts.clone(),
            snapshot: KnowledgeSnapshotRef {
                id: snapshot.id.clone(),
                fingerprint: snapshot.fingerprint,
            },
            context: trusted.context.clone(),
            context_conflicts: trusted.conflicts.clone(),
            provenance: provenance.clone(),
            validity: KnowledgeGuidanceValidity {
                snapshot: snapshot.id.clone(),
                hierarchy_revisions: snapshot.hierarchies,
                context_hash: content_hash(&trusted.context)?,
                expires_at: None,
            },
            bundle: HierarchyContextBundle {
                provenance: Some(provenance),
                ..Default::default()
            },
            effective,
        };
        for a in &result.effective.applied {
            if a.role == AppliedKnowledgeRole::SupportingContext {
                continue;
            }
            let knowledge = KnowledgeRevisionRef::from(&a.artifact);
            let body = self.store.operational_revision(&knowledge).ok();
            let r = ResolvedKnowledgeRef {
                knowledge,
                node: a.node.clone(),
                role: a.role,
                lineage: a.lineage.clone(),
                statement: body
                    .as_ref()
                    .map(|b| b.statement.clone())
                    .unwrap_or_else(|| a.artifact.id.clone()),
            };
            match a.role {
                AppliedKnowledgeRole::Primary => result.bundle.primary_knowledge.push(r.clone()),
                AppliedKnowledgeRole::Refinement => result.bundle.refinements.push(r.clone()),
                AppliedKnowledgeRole::Exception => result.bundle.exceptions.push(r.clone()),
                _ => {}
            }
            if a.role == AppliedKnowledgeRole::Exception {
                continue;
            }
            use crate::abstraction::AbstractKnowledgeKind::*;
            use KnowledgeArtifactKind::*;
            match a.artifact.kind {
                Skill | AbstractKnowledge(AbstractSkill) => result.skills.push(r),
                Lesson | AbstractKnowledge(AbstractLesson) => result.lessons.push(r),
                Constraint | AbstractKnowledge(AbstractConstraint) => result.constraints.push(r),
                AntiPattern | AbstractKnowledge(AbstractAntiPattern) => result.antipatterns.push(r),
                Recovery | AbstractKnowledge(AbstractRecovery) => {
                    result.recoveries.push(ResolvedRecovery {
                        knowledge: r,
                        executable: body.and_then(|b| b.recovery),
                    })
                }
                _ => {}
            }
        }
        result
            .bundle
            .primary_knowledge
            .truncate(self.budget.primary);
        result.bundle.refinements.truncate(self.budget.refinements);
        result.bundle.exceptions.truncate(self.budget.exceptions);
        result.bundle.constraints = result
            .constraints
            .iter()
            .take(self.budget.constraints)
            .cloned()
            .collect();
        result.bundle.recoveries = result
            .recoveries
            .iter()
            .take(self.budget.recoveries)
            .cloned()
            .collect();
        result.bundle.antipatterns = result
            .antipatterns
            .iter()
            .take(self.budget.antipatterns)
            .cloned()
            .collect();
        result.bundle.known_unknowns = result
            .effective
            .unknown
            .iter()
            .take(self.budget.unknowns)
            .cloned()
            .collect();
        result.bundle.conflicts = result
            .effective
            .conflicts
            .iter()
            .take(self.budget.conflicts)
            .cloned()
            .collect();
        // Display budgets never truncate the persisted resolution or controller input.
        for item in result
            .bundle
            .primary_knowledge
            .iter_mut()
            .chain(&mut result.bundle.refinements)
            .chain(&mut result.bundle.exceptions)
            .chain(&mut result.bundle.constraints)
            .chain(&mut result.bundle.antipatterns)
        {
            item.statement = item
                .statement
                .chars()
                .take(self.budget.statement_chars)
                .collect();
            item.lineage.truncate(self.budget.lineage);
        }
        for item in &mut result.bundle.recoveries {
            item.knowledge.statement = item
                .knowledge
                .statement
                .chars()
                .take(self.budget.statement_chars)
                .collect();
            item.knowledge.lineage.truncate(self.budget.lineage);
        }
        for conflict in &mut result.bundle.conflicts {
            conflict.reason = conflict
                .reason
                .chars()
                .take(self.budget.statement_chars)
                .collect();
            conflict.nodes.truncate(self.budget.lineage);
            conflict.artifacts.truncate(self.budget.lineage);
        }
        if let Some(p) = &mut result.bundle.provenance {
            p.applied_artifacts.truncate(
                self.budget.primary
                    + self.budget.refinements
                    + self.budget.exceptions
                    + self.budget.constraints
                    + self.budget.recoveries
                    + self.budget.antipatterns,
            );
            p.suppressed_artifacts.truncate(self.budget.lineage);
            p.conflicts.truncate(self.budget.conflicts);
        }
        if self.persist {
            self.store.save_resolution(&KnowledgeResolutionRecord {
                id: resolution_id,
                snapshot: snapshot.id,
                context_hash: result.validity.context_hash.clone(),
                context: trusted.context,
                context_conflicts: trusted.conflicts,
                effective: result.effective.clone(),
                created_at: Utc::now(),
            })?;
        }
        Ok(result)
    }
}
