// SPDX-License-Identifier: Apache-2.0
use super::{RuntimeStore, Store};
use crate::{Error, Result, core::*, hierarchy::*, knowledge_runtime::*, runtime::*};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
fn sql_revision(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidInput("Revision overflow".into()))
}
pub(crate) fn hash(value: &impl serde::Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
pub(crate) fn revision_key(reference: &KnowledgeRevisionRef) -> Result<String> {
    serde_json::to_string(reference).map_err(Into::into)
}
impl Store {
    pub fn register_operational_revision(
        &self,
        artifact: &OperationalKnowledgeRevision,
    ) -> Result<()> {
        if artifact.knowledge.revision != artifact.knowledge.artifact.revision
            || artifact.knowledge.revision == 0
        {
            return Err(Error::InvalidInput("Inconsistent artifact revision".into()));
        }
        let key = revision_key(&artifact.knowledge)?;
        let data = serde_json::to_string(artifact)?;
        self.connection.execute("INSERT INTO knowledge_artifact_revisions(key,data) VALUES(?1,?2) ON CONFLICT DO NOTHING",params![key,data])?;
        let stored: String = self.connection.query_row(
            "SELECT data FROM knowledge_artifact_revisions WHERE key=?1",
            [key],
            |r| r.get(0),
        )?;
        if stored != data {
            return Err(Error::InvalidInput(
                "Immutable artifact revision already has different contents".into(),
            ));
        }
        Ok(())
    }
    pub fn operational_revision(
        &self,
        reference: &KnowledgeRevisionRef,
    ) -> Result<OperationalKnowledgeRevision> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM knowledge_artifact_revisions WHERE key=?1",
                [revision_key(reference)?],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound("Operational artifact revision unavailable".into())
        })?)?)
    }
    pub fn knowledge_event(&self, kind: &str, value: &impl serde::Serialize) -> Result<()> {
        self.connection.execute(
            "INSERT INTO knowledge_events(kind,data) VALUES(?1,?2)",
            params![kind, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn create_knowledge_snapshot(
        &self,
        policy: &KnowledgeResolutionPolicy,
    ) -> Result<KnowledgeSnapshot> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let hierarchies = self.knowledge_hierarchies()?;
        let concrete = self.knowledge_artifacts_for_abstraction()?;
        let mut refs = vec![];
        let mut artifacts = std::collections::BTreeSet::new();
        for h in hierarchies {
            if !validate_hierarchy(&h).valid {
                return Err(Error::InvalidInput(
                    "Cannot snapshot invalid hierarchy".into(),
                ));
            }
            let digest = hash(&h)?;
            let data = serde_json::to_string(&h)?;
            tx.execute("INSERT INTO knowledge_hierarchy_revisions(id,revision,hash,data) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![h.id.to_string(),sql_revision(h.revision)?,digest,data])?;
            let old: String = tx.query_row(
                "SELECT hash FROM knowledge_hierarchy_revisions WHERE id=?1 AND revision=?2",
                params![h.id.to_string(), sql_revision(h.revision)?],
                |r| r.get(0),
            )?;
            if old != digest {
                return Err(Error::InvalidInput(
                    "Hierarchy revision changed without increment".into(),
                ));
            }
            refs.push(HierarchyRevisionRef {
                id: h.id,
                revision: h.revision,
                content_hash: digest,
            });
            for node in h.nodes.values() {
                let reference = KnowledgeRevisionRef::from(&node.artifact);
                if self.operational_revision(&reference).is_err() {
                    // Capture a revision body once. Missing bodies remain explicit reference
                    // guidance; recovery execution is never fabricated.
                    let body =
                        if let KnowledgeArtifactKind::AbstractKnowledge(_) = node.artifact.kind {
                            node.artifact
                                .id
                                .parse()
                                .ok()
                                .and_then(|id| self.abstract_knowledge(&id).ok())
                                .filter(|k| k.revision == reference.revision)
                                .map(|k| k.statement)
                        } else {
                            concrete
                                .iter()
                                .find(|a| a.artifact == node.artifact)
                                .map(|a| a.statement.clone())
                        };
                    self.register_operational_revision(&OperationalKnowledgeRevision {
                        knowledge: reference.clone(),
                        statement: body.unwrap_or_else(|| {
                            format!("Operational guidance: {}", node.artifact.id)
                        }),
                        recovery: None,
                    })?;
                }
                artifacts.insert(reference);
            }
        }
        let artifact_revisions: Vec<_> = artifacts.into_iter().collect();
        let fingerprint = hash(&(
            RESOLUTION_POLICY_VERSION,
            &refs,
            &artifact_revisions,
            policy,
        ))?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT data FROM knowledge_snapshots WHERE fingerprint=?1",
                [&fingerprint],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(data) = existing {
            tx.commit()?;
            return Ok(serde_json::from_str(&data)?);
        }
        let snapshot = KnowledgeSnapshot {
            id: KnowledgeSnapshotId::new(),
            created_at: Utc::now(),
            hierarchies: refs,
            artifact_revisions,
            resolution_policy_version: RESOLUTION_POLICY_VERSION.into(),
            policy: policy.clone(),
            fingerprint,
        };
        tx.execute(
            "INSERT INTO knowledge_snapshots(id,fingerprint,data) VALUES(?1,?2,?3)",
            params![
                snapshot.id.to_string(),
                snapshot.fingerprint,
                serde_json::to_string(&snapshot)?
            ],
        )?;
        self.knowledge_event("knowledge_snapshot_created", &snapshot.id)?;
        tx.commit()?;
        Ok(snapshot)
    }
    pub fn knowledge_snapshot(&self, id: &KnowledgeSnapshotId) -> Result<KnowledgeSnapshot> {
        let data: String = self.connection.query_row(
            "SELECT data FROM knowledge_snapshots WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn snapshot_hierarchies(&self, s: &KnowledgeSnapshot) -> Result<Vec<KnowledgeHierarchy>> {
        if s.resolution_policy_version != RESOLUTION_POLICY_VERSION
            || hash(&(
                RESOLUTION_POLICY_VERSION,
                &s.hierarchies,
                &s.artifact_revisions,
                &s.policy,
            ))? != s.fingerprint
        {
            return Err(Error::InvalidInput(
                "Snapshot policy or fingerprint mismatch".into(),
            ));
        }
        s.hierarchies
            .iter()
            .map(|r| {
                let data: String = self.connection.query_row(
                    "SELECT data FROM knowledge_hierarchy_revisions WHERE id=?1 AND revision=?2",
                    params![r.id.to_string(), sql_revision(r.revision)?],
                    |row| row.get(0),
                )?;
                let h: KnowledgeHierarchy = serde_json::from_str(&data)?;
                if hash(&h)? != r.content_hash {
                    return Err(Error::InvalidInput(
                        "Historical hierarchy content hash mismatch".into(),
                    ));
                }
                Ok(h)
            })
            .collect()
    }
    pub fn save_resolution(&self, r: &KnowledgeResolutionRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO knowledge_resolution_records(id,snapshot,data) VALUES(?1,?2,?3)",
            params![
                r.id.to_string(),
                r.snapshot.to_string(),
                serde_json::to_string(r)?
            ],
        )?;
        self.knowledge_event("knowledge_resolution_recorded", &r.id)
    }
    pub fn knowledge_resolution_record(
        &self,
        id: &KnowledgeResolutionId,
    ) -> Result<KnowledgeResolutionRecord> {
        let data: String = self.connection.query_row(
            "SELECT data FROM knowledge_resolution_records WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn save_knowledge_conflict(&self, c: &StoredKnowledgeConflict) -> Result<()> {
        self.connection.execute("INSERT INTO operational_knowledge_conflicts(id,data) VALUES(?1,?2) ON CONFLICT DO NOTHING",params![c.id.to_string(),serde_json::to_string(c)?])?;
        Ok(())
    }
    pub fn knowledge_conflicts(&self) -> Result<Vec<StoredKnowledgeConflict>> {
        self.connection
            .prepare("SELECT data FROM operational_knowledge_conflicts ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn knowledge_conflict(&self, id: &KnowledgeConflictId) -> Result<StoredKnowledgeConflict> {
        self.knowledge_conflicts()?
            .into_iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| Error::NotFound("Knowledge conflict not found".into()))
    }
    pub fn record_context_observation(
        &self,
        session: &HardknockSessionId,
        key: &str,
        value: &ContextValue,
    ) -> Result<()> {
        self.connection.execute("INSERT INTO knowledge_context_observations(session,key,source,data) VALUES(?1,?2,?3,?4) ON CONFLICT(session,key,source) DO UPDATE SET data=excluded.data",params![session.to_string(),key,serde_json::to_string(&value.source)?,serde_json::to_string(value)?])?;
        Ok(())
    }
    pub fn runtime_context_observations(
        &self,
        session: &HardknockSessionId,
    ) -> Result<ContextObservations> {
        let mut result = ContextObservations::new();
        for row in self.connection.prepare("SELECT key,data FROM knowledge_context_observations WHERE session=?1 ORDER BY key,source")?.query_map([session.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))? {let (key,data)=row?;result.entry(key).or_default().push(serde_json::from_str(&data)?);}
        Ok(result)
    }
    pub fn guidance_is_current(
        &self,
        v: &KnowledgeGuidanceValidity,
        context: &KnowledgeContext,
    ) -> Result<bool> {
        if v.expires_at.is_some_and(|t| t <= Utc::now()) || hash(context)? != v.context_hash {
            return Ok(false);
        }
        let current = self.knowledge_hierarchies()?;
        Ok(current.len() == v.hierarchy_revisions.len()
            && current.iter().all(|h| {
                v.hierarchy_revisions.iter().any(|r| {
                    r.id == h.id
                        && r.revision == h.revision
                        && hash(h).is_ok_and(|hash| hash == r.content_hash)
                })
            }))
    }
    pub fn attach_runtime_knowledge(&self, context: &mut RuntimeDecisionContext) -> Result<()> {
        if self.knowledge_hierarchies()?.is_empty() {
            context.operational_knowledge = None;
            return Ok(());
        }
        for (key, values) in self.runtime_context_observations(&context.session_id)? {
            context
                .context_observations
                .entry(key)
                .or_default()
                .extend(values);
        }
        let resolution = DefaultRuntimeKnowledgeResolver {
            store: self,
            policy: KnowledgeResolutionPolicy::default(),
            budget: KnowledgeContextBudget::default(),
            persist: true,
        }
        .resolve_for_runtime(context)?;
        context.operational_knowledge = Some(resolution);
        Ok(())
    }
    pub(crate) fn persist_knowledge_applications(
        &self,
        record: &RuntimeDecisionRecord,
    ) -> Result<()> {
        let Some(k) = &record.context.operational_knowledge else {
            return Ok(());
        };
        for applied in &k.effective.applied {
            let a = KnowledgeApplication {
                id: KnowledgeApplicationId::new(),
                knowledge: KnowledgeRevisionRef::from(&applied.artifact),
                resolution: k.provenance.resolution_id.clone(),
                role: applied.role,
                runtime_decision: record.id.clone(),
                outcome: None,
            };
            self.connection.execute("INSERT INTO knowledge_applications(id,resolution,decision,data) VALUES(?1,?2,?3,?4)",params![a.id.to_string(),a.resolution.to_string(),a.runtime_decision.to_string(),serde_json::to_string(&a)?])?;
            self.knowledge_event("knowledge_applied", &a)?;
        }
        Ok(())
    }
    pub fn knowledge_applications(&self) -> Result<Vec<KnowledgeApplication>> {
        self.connection
            .prepare("SELECT data FROM knowledge_applications ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    /// Explicit controlled evidence is required; ordinary success never labels guidance helpful.
    pub fn classify_knowledge_application(
        &self,
        id: &KnowledgeApplicationId,
        outcome: KnowledgeApplicationOutcome,
        evidence: &[crate::epistemic::EvidenceRef],
    ) -> Result<()> {
        if evidence.is_empty() {
            return Err(Error::InvalidInput(
                "Controlled evidence required for knowledge outcome attribution".into(),
            ));
        }
        for reference in evidence {
            let experiment: ExperimentId = reference.id.parse().map_err(|_| {
                Error::InvalidInput("Attribution requires a stored controlled experiment ID".into())
            })?;
            let e = crate::store::ExperimentStore::get(self, &experiment)?
                .ok_or_else(|| Error::NotFound("Attribution experiment not found".into()))?;
            if e.status != crate::experimentation::ExperimentStatus::Completed
                || !e.result.as_ref().is_some_and(|r| {
                    r.quality == crate::experimentation::ExperimentQuality::Controlled
                })
            {
                return Err(Error::InvalidInput(
                    "Attribution requires a completed controlled comparison".into(),
                ));
            }
        }
        let mut a = self
            .knowledge_applications()?
            .into_iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| Error::NotFound("Application not found".into()))?;
        a.outcome = Some(outcome);
        self.connection.execute(
            "UPDATE knowledge_applications SET data=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&a)?],
        )?;
        self.knowledge_event(
            "knowledge_application_outcome",
            &serde_json::json!({"application":a,"evidence":evidence}),
        )
    }
}
impl Store {
    pub fn replay_knowledge_decision(
        &self,
        id: &RuntimeDecisionId,
        config: RuntimePolicyConfig,
    ) -> Result<serde_json::Value> {
        let original = self.runtime_decision(id)?;
        let historical = if let Some(k) = &original.context.operational_knowledge {
            let snapshot = self.knowledge_snapshot(&k.snapshot.id)?;
            let effective = DefaultHistoricalKnowledgeResolver { store: self }
                .resolve_snapshot(&snapshot, &k.context)?;
            if serde_json::to_value(&effective)? != serde_json::to_value(&k.effective)? {
                return Err(Error::InvalidInput(
                    "Historical resolution does not match recorded knowledge".into(),
                ));
            }
            Some(effective)
        } else {
            None
        };
        let mut current_context = original.context.clone();
        current_context.operational_knowledge = if self.knowledge_hierarchies()?.is_empty() {
            None
        } else {
            Some(
                DefaultRuntimeKnowledgeResolver {
                    store: self,
                    policy: KnowledgeResolutionPolicy::default(),
                    budget: KnowledgeContextBudget::default(),
                    persist: false,
                }
                .resolve_for_runtime(&current_context)?,
            )
        };
        let evaluation =
            DeterministicRuntimeController::with_config(config)?.evaluate(&current_context)?;
        Ok(
            serde_json::json!({"kind":"decision_replay","original":original,"original_knowledge":historical,"current_knowledge":current_context.operational_knowledge,"current_hypothetical_decision":evaluation,"replay":{"id":RuntimeDecisionId::new(),"decision":evaluation.decision,"recorded":false},"original_mutated":false,"context_reused":true,"reason":"Historical context held fixed; current hierarchy revisions and policy compared without rewriting the original"}),
        )
    }
    /// Commit gate checks the decision bound to this exact session/action. It
    /// returns re-resolution required; it never grants external commit authority.
    pub fn check_knowledge_before_commit(&self, session: &str, action_id: &str) -> Result<()> {
        if self.knowledge_hierarchies()?.is_empty() {
            return Ok(());
        }
        let record = self.runtime_decisions()?.into_iter().rev().find(|r| {
            r.session_id.to_string() == session
                && r.context.operational_knowledge.is_some()
                && r.context.knowledge_action_id.as_deref() == Some(action_id)
        });
        let Some(record) = record else {
            return Err(Error::Intervention(format!(
                "Knowledge re-resolution required before committing action {action_id}"
            )));
        };
        let k = record
            .context
            .operational_knowledge
            .as_ref()
            .expect("filtered knowledge");
        let mut context = record.context.clone();
        for (key, values) in self.runtime_context_observations(&record.session_id)? {
            context.context_observations.insert(key, values);
        }
        let now = DefaultKnowledgeContextBuilder.build(&context)?;
        if !self.guidance_is_current(&k.validity, &now)?
            || record.decision.kind() != RuntimeDecisionKind::Act
        {
            return Err(Error::Intervention(
                "Knowledge guidance changed or did not recommend Act; re-resolution required"
                    .into(),
            ));
        }
        Ok(())
    }
    pub fn knowledge_snapshot_diff(
        &self,
        a: &KnowledgeSnapshotId,
        b: &KnowledgeSnapshotId,
    ) -> Result<serde_json::Value> {
        let a = self.knowledge_snapshot(a)?;
        let b = self.knowledge_snapshot(b)?;
        Ok(
            serde_json::json!({"added":b.artifact_revisions.iter().filter(|r|!a.artifact_revisions.contains(r)).collect::<Vec<_>>(),"removed_or_changed":a.artifact_revisions.iter().filter(|r|!b.artifact_revisions.contains(r)).collect::<Vec<_>>(),"before":a.hierarchies,"after":b.hierarchies}),
        )
    }
    pub fn knowledge_metrics(&self) -> Result<RuntimeKnowledgeMetrics> {
        let mut metrics = RuntimeKnowledgeMetrics::default();
        for r in self.runtime_decisions()? {
            if let Some(k) = r.context.operational_knowledge {
                metrics.constraint_applications += k.constraints.len() as u64;
                metrics.antipattern_applications += k.antipatterns.len() as u64;
                metrics.recovery_applications += k.recoveries.len() as u64;
                metrics.exception_applications += k
                    .effective
                    .applied
                    .iter()
                    .filter(|a| a.role == AppliedKnowledgeRole::Exception)
                    .count() as u64;
                metrics.knowledge_conflicts += k.effective.conflicts.len() as u64;
                let snapshot = self.knowledge_snapshot(&k.snapshot.id)?;
                for h in self.snapshot_hierarchies(&snapshot)? {
                    let overrides: std::collections::BTreeSet<_> = h
                        .edges
                        .iter()
                        .filter(|e| {
                            matches!(
                                e.relation,
                                KnowledgeHierarchyRelation::Excepts
                                    | KnowledgeHierarchyRelation::Supersedes
                                    | KnowledgeHierarchyRelation::Specializes
                            )
                        })
                        .map(|e| &e.child)
                        .collect();
                    metrics.unknown_overrides_prevented += k
                        .effective
                        .unknown
                        .iter()
                        .filter(|a| overrides.contains(&a.node))
                        .count() as u64;
                    metrics.stale_knowledge_prevented += k
                        .effective
                        .advisory
                        .iter()
                        .filter(|a| {
                            h.nodes
                                .get(&a.node)
                                .is_some_and(|n| n.freshness == FreshnessStatus::Stale)
                        })
                        .count() as u64;
                    metrics.parent_fallbacks += k
                        .effective
                        .applied
                        .iter()
                        .filter(|a| {
                            a.role != AppliedKnowledgeRole::SupportingContext
                                && h.edges.iter().any(|e| {
                                    e.parent == a.node
                                        && (k.effective.unknown.iter().any(|u| u.node == e.child)
                                            || k.effective
                                                .advisory
                                                .iter()
                                                .any(|u| u.node == e.child))
                                })
                        })
                        .count() as u64;
                }
            }
        }
        for a in self.knowledge_applications()? {
            metrics.false_constraint_applications +=
                u64::from(a.outcome == Some(KnowledgeApplicationOutcome::FalseConstraint));
            metrics.harmful_exception_applications += u64::from(
                a.role == AppliedKnowledgeRole::Exception
                    && a.outcome == Some(KnowledgeApplicationOutcome::Harmful),
            );
        }
        Ok(metrics)
    }
}
impl Store {
    pub fn save_guard_candidate(&self, c: &GuardRevisionCandidate) -> Result<()> {
        if !matches!(
            c.status,
            GuardRevisionCandidateStatus::Proposed | GuardRevisionCandidateStatus::ReadyForReview
        ) {
            return Err(Error::InvalidInput(
                "External acceptance cannot be self-assigned".into(),
            ));
        }
        c.evidence.verify()?;
        self.connection.execute(
            "INSERT INTO guard_revision_candidates(id,data) VALUES(?1,?2)",
            params![c.id.to_string(), serde_json::to_string(c)?],
        )?;
        if let Some(g) = &c.source_guard {
            for r in &c.source_knowledge {
                self.connection.execute("INSERT INTO guard_knowledge_dependencies(guard_id,guard_revision,knowledge_key,data) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![g.id,g.revision,revision_key(r)?,serde_json::to_string(r)?])?;
            }
        }
        self.knowledge_event("guard_revision_candidate_created", c)
    }
    pub fn guard_candidates(&self) -> Result<Vec<GuardRevisionCandidate>> {
        self.connection
            .prepare("SELECT data FROM guard_revision_candidates ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn guard_candidate(&self, id: &GuardRevisionCandidateId) -> Result<GuardRevisionCandidate> {
        self.guard_candidates()?
            .into_iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| Error::NotFound("Guard candidate not found".into()))
    }
    pub fn knowledge_audit(&self) -> Result<serde_json::Value> {
        let mut reports = vec![];
        for h in self.knowledge_hierarchies()? {
            let changes = health_projection(&h).1;
            let exceptions=h.edges.iter().filter(|e|e.relation==KnowledgeHierarchyRelation::Excepts).map(|e|serde_json::json!({"parent":e.parent,"child":h.nodes.get(&e.child),"constraint_relaxing":h.nodes.get(&e.parent).is_some_and(|n|matches!(n.artifact.kind,KnowledgeArtifactKind::Constraint|KnowledgeArtifactKind::AbstractKnowledge(crate::abstraction::AbstractKnowledgeKind::AbstractConstraint)))})).collect::<Vec<_>>();
            let candidates = self.guard_candidates()?;
            let governance = h
                .nodes
                .values()
                .map(|n| {
                    let relevance = if candidates.iter().any(|c| {
                        c.source_guard.is_some()
                            && c.source_knowledge
                                .iter()
                                .any(|r| r.artifact.id == n.artifact.id)
                    }) {
                        GovernanceRelevance::ExistingGuardDependency
                    } else if matches!(
                        n.artifact.kind,
                        KnowledgeArtifactKind::Constraint
                            | KnowledgeArtifactKind::AbstractKnowledge(
                                crate::abstraction::AbstractKnowledgeKind::AbstractConstraint
                            )
                    ) {
                        GovernanceRelevance::GuardCandidate
                    } else {
                        GovernanceRelevance::Advisory
                    };
                    serde_json::json!({"knowledge":n.artifact,"relevance":relevance})
                })
                .collect::<Vec<_>>();
            reports.push(serde_json::json!({"id":h.id,"validation":validate_hierarchy(&h),"exceptions":exceptions,"health_changes":changes,"governance":governance}));
        }
        Ok(
            serde_json::json!({"hierarchies":reports,"runtime":self.knowledge_metrics()?,"conflicts":self.knowledge_conflicts()?,"guard_candidates":self.guard_candidates()?,"enforcement_changed":false}),
        )
    }
}
impl Store {
    pub fn plan_knowledge_conflict(
        &self,
        id: &KnowledgeConflictId,
        template: Option<crate::experimentation::ExperimentRequest>,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<KnowledgeConflictResolutionPlan> {
        let conflict = self.knowledge_conflict(id)?;
        let mut plan = DefaultKnowledgeConflictPlanner {
            experiment_template: template,
        }
        .plan(&conflict.conflict, &conflict.context, budget)?;
        plan.conflict = id.clone();
        let exposure = self
            .runtime_decisions()?
            .iter()
            .filter(|r| {
                r.context
                    .operational_knowledge
                    .as_ref()
                    .is_some_and(|k| k.provenance.conflicts.contains(id))
            })
            .count();
        self.save_experience_opportunity(&conflict_opportunity(&conflict, exposure, budget)?)?;
        self.knowledge_event("knowledge_conflict_planned", &plan)?;
        Ok(plan)
    }
    /// Scope changes require controlled stored experiments and preserve historic revisions.
    pub fn narrow_knowledge_conflict(
        &self,
        id: &KnowledgeConflictId,
        hierarchy: &KnowledgeHierarchyId,
        node: &KnowledgeNodeId,
        scope: KnowledgeScope,
        experiments: &[ExperimentId],
    ) -> Result<KnowledgeHierarchy> {
        if experiments.is_empty() {
            return Err(Error::InvalidInput(
                "Controlled experiments required".into(),
            ));
        }
        for experiment in experiments {
            let e = crate::store::ExperimentStore::get(self, experiment)?
                .ok_or_else(|| Error::NotFound("Experiment not found".into()))?;
            if e.status != crate::experimentation::ExperimentStatus::Completed
                || !e.result.as_ref().is_some_and(|r| {
                    r.quality == crate::experimentation::ExperimentQuality::Controlled
                        && r.recommendation.is_some()
                })
            {
                return Err(Error::InvalidInput(
                    "Scope revision requires a completed controlled comparison".into(),
                ));
            }
        }
        let plans: Vec<KnowledgeConflictResolutionPlan> = self
            .connection
            .prepare("SELECT data FROM knowledge_events WHERE kind='knowledge_conflict_planned'")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        for experiment in experiments {
            let e = crate::store::ExperimentStore::get(self, experiment)?
                .ok_or_else(|| Error::NotFound("Experiment missing".into()))?;
            if !plans
                .iter()
                .any(|p| &p.conflict == id && p.experiments.iter().any(|r| r.id == e.request.id))
            {
                return Err(Error::InvalidInput(
                    "Experiment was not planned for this conflict".into(),
                ));
            }
        }
        let mut conflict = self.knowledge_conflict(id)?;
        if !conflict.conflict.nodes.contains(node) {
            return Err(Error::InvalidInput(
                "Node not a conflict participant".into(),
            ));
        }
        let mut h = self.knowledge_hierarchy(hierarchy)?;
        let n = h
            .nodes
            .get_mut(node)
            .ok_or_else(|| Error::NotFound("Node missing".into()))?;
        if DeterministicScopeRelationEvaluator.compare(&scope, &n.scope) != ScopeRelation::Narrower
        {
            return Err(Error::InvalidInput(
                "New scope must be proven narrower".into(),
            ));
        }
        n.scope = scope;
        n.artifact.revision += 1;
        n.provenance
            .evidence
            .extend(experiments.iter().map(|id| crate::epistemic::EvidenceRef {
                kind: "controlled_experiment".into(),
                id: id.to_string(),
            }));
        h.revision += 1;
        h.updated_at = Utc::now();
        let after = DeterministicKnowledgeResolver.resolve(
            &health_projection(&h).0,
            &conflict.context,
            &KnowledgeResolutionPolicy::default(),
        )?;
        if after.conflicts.iter().any(|c| {
            c.nodes
                .iter()
                .any(|id| conflict.conflict.nodes.contains(id))
        }) {
            return Err(Error::InvalidInput(
                "Controlled scope change has not resolved this context's conflict".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.save_knowledge_hierarchy(&h)?;
        conflict.resolved = true;
        self.connection.execute(
            "UPDATE operational_knowledge_conflicts SET data=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&conflict)?],
        )?;
        self.knowledge_event("knowledge_conflict_resolved",&serde_json::json!({"conflict":id,"hierarchy":h.id,"revision":h.revision,"experiments":experiments}))?;
        tx.commit()?;
        Ok(h)
    }
}
impl Store {
    pub fn knowledge_bundle_for_context(
        &self,
        context: &crate::experience::ExperienceContext,
        agent: &AgentIdentity,
    ) -> Result<Option<HierarchyContextBundle>> {
        if self.knowledge_hierarchies()?.is_empty() {
            return Ok(None);
        }
        let mut runtime =
            RuntimeContextSynthesizer { store: self }.synthesize(RuntimeContextRequest {
                external_session_id: "knowledge-context-bundle".into(),
                agent: agent.clone(),
                task: TaskDescriptor {
                    description: "Operational knowledge context".into(),
                    family: None,
                    tags: vec![],
                },
                query_context: crate::retrieval::QueryContext::new(context, "", vec![]),
                proposed_action: None,
                proposed_effect: None,
                risk: None,
                capability_context: CapabilityContext::default(),
                failure_signature: None,
                consecutive_failures: 0,
                no_state_change: false,
                config_changed: false,
                candidate_strategies: vec![],
                experiment_capability: Default::default(),
                known_unknowns: vec![],
                externally_supported: false,
                envelope_position: None,
            })?;
        self.attach_runtime_knowledge(&mut runtime)?;
        Ok(runtime.operational_knowledge.map(|r| r.bundle))
    }
}
impl Store {
    pub(crate) fn record_hierarchy_health(&self, h: &KnowledgeHierarchy) -> Result<()> {
        for change in health_projection(h).1 {
            self.connection.execute(
                "INSERT INTO knowledge_health_changes(hierarchy,data) VALUES(?1,?2)",
                params![h.id.to_string(), serde_json::to_string(&change)?],
            )?;
            self.knowledge_event("knowledge_health_changed", &change)?;
        }
        let candidates = self.guard_candidates()?;
        let mut seen = std::collections::BTreeSet::new();
        for previous in &candidates {
            if let Some(guard) = &previous.source_guard {
                for n in h.nodes.values().filter(|n| {
                    n.freshness == FreshnessStatus::Contradicted
                        || n.maturity == KnowledgeMaturity::Contradicted
                }) {
                    if previous
                        .source_knowledge
                        .iter()
                        .any(|r| r.artifact.id == n.artifact.id)
                        && seen.insert((guard.id.clone(), n.id.clone()))
                        && !candidates.iter().any(|c| {
                            c.reason == GuardRevisionReason::ContradictionDiscovered
                                && c.evidence.hierarchy.revision == h.revision
                                && c.source_guard.as_ref() == Some(guard)
                        })
                    {
                        self.save_guard_candidate(&guard_revision_candidate(
                            h,
                            &n.id,
                            Some(guard.clone()),
                        )?)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Store {
    /// Compile an explicit comparison into the existing curriculum executor.
    pub fn compile_knowledge_curriculum(
        &self,
        plan: &KnowledgeConflictResolutionPlan,
        skill: &str,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<crate::curriculum::Curriculum> {
        use crate::curriculum::*;
        let skill = self.skill(skill)?;
        if plan.experiments.is_empty() {
            return Err(Error::InvalidInput(
                "Supply a concrete paired experiment before compiling a curriculum".into(),
            ));
        }
        let trials = plan
            .experiments
            .iter()
            .map(|request| {
                Ok(CurriculumTrial {
                    id: CurriculumTrialId::new(),
                    goal_id: plan.goal.id.clone(),
                    skill_id: skill.id.clone(),
                    condition: format!("knowledge-conflict:{}", plan.conflict),
                    fingerprint: hash(request)?,
                    intent: TrialIntent::Revalidation,
                    execution: TrialExecution::Experiment {
                        request: Box::new(request.clone()),
                    },
                    result: None,
                    learning_outcome: None,
                    status: GoalStatus::Planned,
                    estimated_budget: crate::budget::ExperienceUsage {
                        realities: request.candidates.len(),
                        ..Default::default()
                    },
                    expected_value: plan.reason.clone(),
                    required_isolation: RealityCapabilities::default(),
                    round: 1,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let now = Utc::now();
        let curriculum = Curriculum {
            id: CurriculumId::new(),
            target: CurriculumTarget::Skill(skill.id),
            profile: "knowledge-conflict".into(),
            goals: vec![plan.goal.clone()],
            trials,
            budget: budget.clone(),
            usage: Default::default(),
            reserved: Default::default(),
            trials_executed: 0,
            status: CurriculumStatus::Planned,
            created_at: now,
            updated_at: now,
            rounds: 0,
            max_rounds: 1,
            revision: 1,
            before: vec![],
            after: vec![],
            stop_reason: None,
            session_id: None,
            quality: CurriculumQuality::Medium,
        };
        crate::store::CurriculumStore::insert(self, &curriculum)?;
        self.knowledge_event(
            "knowledge_conflict_curriculum_created",
            &serde_json::json!({"conflict":plan.conflict,"curriculum":curriculum.id}),
        )?;
        Ok(curriculum)
    }
}
