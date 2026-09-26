// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::Store;
use crate::{
    Error, Result,
    core::RuntimeDecisionId,
    lesson::{EvidenceRef as LessonEvidenceRef, EvidenceRelationship},
    resilience::{ReflexStatus, ResilienceTestStatus},
    runtime::*,
};

type RuntimeGapKey = (String, Option<String>, KnowledgeState, RuntimeDecisionKind);
type RuntimeGapAggregate = (u64, Vec<String>);

#[derive(Clone, Debug)]
pub(crate) struct PreparedRuntimeDecision {
    pub(crate) record: RuntimeDecisionRecord,
    config: RuntimePolicyConfig,
    role_violation: Option<PreparedRoleViolation>,
}

#[derive(Clone, Debug)]
struct PreparedRoleViolation {
    id: String,
    team: String,
    data: String,
}

pub trait RuntimeStore {
    fn record_runtime_decision(
        &self,
        context: &RuntimeDecisionContext,
        config: RuntimePolicyConfig,
    ) -> Result<RuntimeDecisionRecord>;
    fn persist_runtime_decision(
        &self,
        record: &RuntimeDecisionRecord,
        config: RuntimePolicyConfig,
    ) -> Result<()>;
    fn runtime_decision(&self, id: &RuntimeDecisionId) -> Result<RuntimeDecisionRecord>;
    fn runtime_decisions(&self) -> Result<Vec<RuntimeDecisionRecord>>;
    fn record_runtime_feedback(&self, feedback: &RuntimeDecisionFeedback) -> Result<()>;
    fn runtime_feedback(&self, id: &RuntimeDecisionId) -> Result<Vec<RuntimeDecisionFeedback>>;
    fn runtime_audit(&self, limit: usize) -> Result<RuntimeAudit>;
    fn runtime_gaps(&self) -> Result<Vec<RuntimeGap>>;
    fn runtime_curriculum_recommendations(
        &self,
    ) -> Result<Vec<crate::curriculum::CurriculumRecommendation>>;
    fn runtime_development_metrics(&self) -> Result<RuntimeDevelopmentMetrics>;
    fn replay_runtime_decision(
        &self,
        id: &RuntimeDecisionId,
        config: RuntimePolicyConfig,
    ) -> Result<RuntimeDecisionRecord>;
}

impl Store {
    pub(crate) fn prepare_runtime_decision_from_context(
        &self,
        context: &RuntimeDecisionContext,
        mut config: RuntimePolicyConfig,
    ) -> Result<PreparedRuntimeDecision> {
        let mut context = context.clone();
        context.active_forecasts = context
            .active_forecasts
            .iter()
            .filter_map(|item| self.forecast(&item.id).ok())
            .filter(|item| item.status.is_active())
            .filter(|item| {
                self.forecast_health(&item.signature)
                    .is_ok_and(|health| health.health == crate::predictive::ForecastHealth::Healthy)
            })
            .collect();
        context.preventive_interventions = context
            .preventive_interventions
            .iter()
            .filter_map(|item| self.preventive_intervention(&item.id).ok())
            .filter(|item| {
                item.status == crate::predictive::PreventiveInterventionStatus::Validated
                    && item.origin == crate::predictive::PredictiveOrigin::Local
            })
            .collect();
        context.causal = self.causal_runtime_guidance(
            &context.query_context,
            context
                .failure_signature
                .as_ref()
                .map(|s| s.signature.as_str()),
        )?;
        let mut blocked = std::collections::BTreeSet::new();
        for id in context
            .available_recovery
            .iter()
            .map(|r| r.id.to_string())
            .chain(context.matched_reflexes.iter().map(|r| r.id.to_string()))
            .chain(
                context
                    .relevant_experience
                    .lessons
                    .iter()
                    .map(|l| l.lesson.id.to_string()),
            )
        {
            if self.causal_artifact_quarantined(&id)? {
                blocked.insert(id);
            }
        }
        context
            .available_recovery
            .retain(|r| !blocked.contains(&r.id.to_string()));
        context
            .matched_reflexes
            .retain(|r| !blocked.contains(&r.id.to_string()));
        context
            .relevant_experience
            .lessons
            .retain(|l| !blocked.contains(&l.lesson.id.to_string()));
        if !blocked.is_empty() {
            context.knowledge_signals.local_supported = false;
            context.knowledge_signals.local_contradicted = true;
        }
        if context
            .assurance
            .summary
            .certification
            .as_ref()
            .is_some_and(|id| {
                self.causal_artifact_quarantined(&id.to_string())
                    .unwrap_or(true)
            })
        {
            context.assurance.summary.status =
                crate::runtime::AssuranceRuntimeStatus::ReviewRecommended;
            context
                .assurance
                .summary
                .reasons
                .push("A supporting causal mechanism requires revalidation".into());
        }
        self.attach_runtime_knowledge_read_only(&mut context)?;
        self.attach_plan_validity(&mut context, true)?;
        self.attach_team_authority(&mut context)?;
        config.refresh_version();
        config.validate()?;
        let evaluation =
            DeterministicRuntimeController::with_config(config.clone())?.evaluate(&context)?;
        let record = RuntimeDecisionRecord {
            id: RuntimeDecisionId::new(),
            session_id: context.session_id.clone(),
            context_hash: context.context_hash()?,
            context,
            decision: evaluation.decision.clone(),
            evaluation,
            created_at: Utc::now(),
        };
        self.prepare_runtime_decision(&record, config)
    }

    pub(crate) fn prepare_runtime_decision(
        &self,
        record: &RuntimeDecisionRecord,
        mut config: RuntimePolicyConfig,
    ) -> Result<PreparedRuntimeDecision> {
        config.refresh_version();
        config.validate()?;
        let expected = DeterministicRuntimeController::with_config(config.clone())?
            .evaluate(&record.context)?;
        if record.context_hash != record.context.context_hash()?
            || record.session_id != record.context.session_id
            || serde_json::to_value(&record.evaluation)? != serde_json::to_value(&expected)?
            || serde_json::to_value(&record.decision)?
                != serde_json::to_value(&record.evaluation.decision)?
        {
            return Err(Error::InvalidInput(
                "Runtime decision record is inconsistent with its context or policy".into(),
            ));
        }
        let mut checked = record.context.clone();
        self.attach_composition_knowledge(&mut checked)?;
        self.attach_plan_identity(&mut checked)?;
        self.attach_plan_validity(&mut checked, false)?;
        self.attach_team_authority(&mut checked)?;
        if serde_json::to_value(&checked.team)? != serde_json::to_value(&record.context.team)? {
            return Err(Error::Intervention(
                "Team authority changed before publication; resolve again".into(),
            ));
        }
        if let Some(plan) = &mut checked.plan
            && let (Some(current), Some(original)) = (
                &mut plan.validity,
                record
                    .context
                    .plan
                    .as_ref()
                    .and_then(|p| p.validity.as_ref()),
            )
        {
            current.id = original.id.clone();
            current.assessed_at = original.assessed_at;
        }
        if serde_json::to_value(&checked.plan)? != serde_json::to_value(&record.context.plan)? {
            return Err(Error::Intervention(
                "Plan changed before decision publication; resolve again".into(),
            ));
        }
        if serde_json::to_value(&checked.composition_assessment)?
            != serde_json::to_value(&record.context.composition_assessment)?
            || checked.context_observations != record.context.context_observations
        {
            return Err(Error::Intervention(
                "Composition state changed before decision publication; resolve again".into(),
            ));
        }
        if record.context.operational_knowledge.is_none()
            && !self.knowledge_hierarchies()?.is_empty()
        {
            return Err(Error::Intervention(
                "Runtime decision requires current hierarchy resolution".into(),
            ));
        }
        if let Some(k) = &record.context.operational_knowledge {
            use crate::knowledge_runtime::RuntimeKnowledgeResolver;
            let policy = self
                .knowledge_snapshot(&k.snapshot.id)
                .map(|snapshot| snapshot.policy)
                .unwrap_or_default();
            let derived = crate::knowledge_runtime::DefaultRuntimeKnowledgeResolver {
                store: self,
                policy,
                budget: Default::default(),
                persist: false,
            }
            .resolve_for_runtime(&record.context)?;
            if serde_json::to_value((
                &derived.skills,
                &derived.lessons,
                &derived.constraints,
                &derived.antipatterns,
                &derived.recoveries,
                &derived.context,
                &derived.unresolved_conflicts,
            ))? != serde_json::to_value((
                &k.skills,
                &k.lessons,
                &k.constraints,
                &k.antipatterns,
                &k.recoveries,
                &k.context,
                &k.unresolved_conflicts,
            ))? {
                return Err(Error::InvalidInput(
                    "Operational knowledge projection differs from immutable revisions".into(),
                ));
            }
            let saved = self
                .connection
                .query_row(
                    "SELECT data FROM knowledge_resolution_records WHERE id=?1",
                    [k.provenance.resolution_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|data| {
                    serde_json::from_str::<crate::knowledge_runtime::KnowledgeResolutionRecord>(
                        &data,
                    )
                })
                .transpose()?;
            if let Some(saved) = saved
                && (saved.snapshot != k.snapshot.id
                    || serde_json::to_value(&saved.effective)?
                        != serde_json::to_value(&k.effective)?
                    || saved.context_hash != k.validity.context_hash)
            {
                return Err(Error::InvalidInput(
                    "Runtime knowledge differs from its recorded resolution".into(),
                ));
            }
            if !self.guidance_is_current(&k.validity, &k.context)? {
                return Err(Error::Intervention(
                    "Hierarchy changed before decision publication; re-resolution required".into(),
                ));
            }
        }
        let role_violation = if let Some(binding) = &checked.team
            && let Some(assessment) = &binding.assessment
            && !assessment.allowed
        {
            let violation = crate::team::RoleViolation {
                team: binding.team.clone(),
                member: binding.member.clone(),
                role: binding.assignment.clone(),
                attempted_action: assessment.action,
                reasons: assessment.reasons.clone(),
                created_at: Utc::now(),
            };
            Some(PreparedRoleViolation {
                id: uuid::Uuid::new_v4().to_string(),
                team: binding.team.to_string(),
                data: serde_json::to_string(&violation)?,
            })
        } else {
            None
        };
        Ok(PreparedRuntimeDecision {
            record: record.clone(),
            config,
            role_violation,
        })
    }

    pub(crate) fn persist_prepared_runtime_decision(
        &self,
        transaction: &Transaction<'_>,
        prepared: &PreparedRuntimeDecision,
    ) -> Result<()> {
        let record = &prepared.record;
        let config = &prepared.config;
        self.prepare_runtime_decision(record, config.clone())?;
        self.persist_runtime_knowledge(transaction, record)?;
        if let Some(violation) = &prepared.role_violation {
            transaction.execute(
                "INSERT INTO team_records(kind,id,team,data) VALUES('role_violation_attempted',?1,?2,?3)",
                params![violation.id, violation.team, violation.data],
            )?;
            transaction.execute(
                "INSERT INTO team_events(team,kind,data) VALUES(?1,'role_violation_attempted',?2)",
                params![violation.team, violation.data],
            )?;
        }
        transaction.execute(
            "INSERT INTO runtime_policy_versions(version,created_at,data) VALUES(?1,?2,?3) ON CONFLICT(version) DO NOTHING",
            params![config.version, record.created_at.to_rfc3339(), serde_json::to_string(config)?],
        )?;
        let stored_config: String = transaction.query_row(
            "SELECT data FROM runtime_policy_versions WHERE version=?1",
            [record.evaluation.policy_version.clone()],
            |row| row.get(0),
        )?;
        if serde_json::from_str::<RuntimePolicyConfig>(&stored_config)? != *config {
            return Err(Error::Intervention(
                "Runtime policy version already names different policy contents".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(NULL,?1,'runtime_decision_requested',?2,?3)",
            params![record.session_id.to_string(), record.created_at.to_rfc3339(), serde_json::to_string(&serde_json::json!({"context_hash":record.context_hash}))?],
        )?;
        transaction.execute(
            "INSERT INTO runtime_decisions(id,session_id,context_hash,decision_kind,knowledge_state,policy_version,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                record.id.to_string(),
                record.session_id.to_string(),
                record.context_hash,
                enum_name(&record.decision.kind())?,
                enum_name(&record.evaluation.knowledge)?,
                record.evaluation.policy_version,
                record.created_at.to_rfc3339(),
                serde_json::to_string(record)?
            ],
        )?;
        self.persist_knowledge_applications_in_transaction(transaction, record)?;
        for selected in record
            .context
            .knowledge_resolution
            .selected
            .iter()
            .filter(|item| item.level == crate::abstraction::ResolutionLevel::Abstract)
        {
            transaction.execute(
                "INSERT INTO abstraction_events(subject,kind,created_at,data) VALUES(?1,'abstract_knowledge_applied',?2,?3)",
                params![selected.reference, record.created_at.to_rfc3339(), serde_json::to_string(&serde_json::json!({"decision_id":record.id,"session_id":record.session_id,"reason":selected.reason}))?],
            )?;
            transaction.execute(
                "INSERT INTO bridge_events(session_id,kind,data) VALUES(?1,'abstract_knowledge_applied',?2)",
                params![record.session_id.to_string(), serde_json::to_string(&serde_json::json!({"abstract_id":selected.reference,"decision_id":record.id}))?],
            )?;
        }
        for (position, reason) in record.evaluation.reasons.iter().enumerate() {
            transaction.execute(
                "INSERT INTO runtime_decision_reasons(decision_id,position,reason_kind,data) VALUES(?1,?2,?3,?4)",
                params![record.id.to_string(), sql_position(position)?, tagged_kind(reason)?, serde_json::to_string(reason)?],
            )?;
        }
        for (position, evidence) in record.evaluation.evidence.iter().enumerate() {
            let (kind, id) = evidence_parts(evidence);
            transaction.execute(
                "INSERT INTO runtime_decision_evidence(decision_id,position,evidence_kind,evidence_id,data) VALUES(?1,?2,?3,?4,?5)",
                params![record.id.to_string(), sql_position(position)?, kind, id, serde_json::to_string(evidence)?],
            )?;
        }
        if let RuntimeDecision::Abstain(abstention) = &record.decision {
            transaction.execute(
                "INSERT INTO runtime_abstentions(decision_id,reason,data) VALUES(?1,?2,?3)",
                params![
                    record.id.to_string(),
                    enum_name(&abstention.reason)?,
                    serde_json::to_string(abstention)?
                ],
            )?;
        }
        let control_event = decision_event(&record.decision);
        transaction.execute(
            "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(?1,?2,'runtime_decision_made',?3,?4)",
            params![record.id.to_string(), record.session_id.to_string(), record.created_at.to_rfc3339(), serde_json::to_string(&serde_json::json!({"decision":record.decision.kind(),"policy_version":record.evaluation.policy_version}))?],
        )?;
        if let Some(control_event) = control_event {
            transaction.execute(
                "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(?1,?2,?3,?4,?5)",
                params![record.id.to_string(), record.session_id.to_string(), enum_name(&control_event)?, record.created_at.to_rfc3339(), serde_json::to_string(&record.decision)?],
            )?;
        }
        for reason in &record.evaluation.reasons {
            if let crate::runtime::DecisionReason::CausalMechanismSupported {
                hypothesis,
                intervention,
            } = reason
            {
                let dependency = crate::causal::CausalArtifactDependency {
                    hypothesis: hypothesis.clone(),
                    artifact: crate::causal::CausalArtifact::RuntimeDecision(record.id.clone()),
                    intervention: Some(intervention.clone()),
                    severity: record.context.risk.severity,
                };
                transaction.execute("INSERT OR IGNORE INTO causal_artifact_dependencies(hypothesis_id,artifact_id,data) VALUES(?1,?2,?3)",params![hypothesis.to_string(),record.id.to_string(),serde_json::to_string(&dependency)?])?;
            }
        }
        if matches!(
            config.forecast.mode,
            ForecastRuntimeMode::Advise | ForecastRuntimeMode::Prevent
        ) && matches!(
            record.decision.kind(),
            RuntimeDecisionKind::Act
                | RuntimeDecisionKind::Replan
                | RuntimeDecisionKind::RequireApproval
        ) {
            for intervention in record.evaluation.reasons.iter().filter_map(|reason| {
                if let DecisionReason::ValidatedPreventiveIntervention(id) = reason {
                    Some(id)
                } else {
                    None
                }
            }) {
                let compact = serde_json::to_string(&serde_json::json!({
                    "subject": intervention,
                    "runtime_decision": record.id,
                }))?;
                transaction.execute(
                    "INSERT INTO predictive_events(subject,kind,data) VALUES(?1,'preventive_intervention_suggested',?2)",
                    params![intervention.to_string(), compact],
                )?;
                transaction.execute(
                    "INSERT INTO bridge_events(session_id,kind,data) VALUES(?1,'preventive_intervention_suggested',?2)",
                    params![record.session_id.to_string(), compact],
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn attach_runtime_knowledge_read_only(
        &self,
        context: &mut RuntimeDecisionContext,
    ) -> Result<()> {
        self.attach_composition_knowledge(context)?;
        self.attach_plan_identity(context)?;
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
        use crate::knowledge_runtime::RuntimeKnowledgeResolver;
        let mut resolution = crate::knowledge_runtime::DefaultRuntimeKnowledgeResolver {
            store: self,
            policy: Default::default(),
            budget: Default::default(),
            persist: false,
        }
        .resolve_for_runtime(context)?;
        let existing = self
            .connection
            .query_row(
                "SELECT data FROM knowledge_snapshots WHERE fingerprint=?1",
                [&resolution.snapshot.fingerprint],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|data| serde_json::from_str::<crate::knowledge_runtime::KnowledgeSnapshot>(&data))
            .transpose()?;
        if let Some(snapshot) = existing {
            resolution.snapshot.id = snapshot.id.clone();
            resolution.validity.snapshot = snapshot.id.clone();
            resolution.provenance.snapshot_id = snapshot.id;
        }
        context.operational_knowledge = Some(resolution);
        Ok(())
    }

    fn persist_runtime_knowledge(
        &self,
        transaction: &Transaction<'_>,
        record: &RuntimeDecisionRecord,
    ) -> Result<()> {
        let Some(knowledge) = &record.context.operational_knowledge else {
            return Ok(());
        };
        let stored_snapshot = transaction
            .query_row(
                "SELECT data FROM knowledge_snapshots WHERE id=?1",
                [knowledge.snapshot.id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|data| serde_json::from_str::<crate::knowledge_runtime::KnowledgeSnapshot>(&data))
            .transpose()?;
        if let Some(snapshot) = &stored_snapshot {
            if snapshot.id != knowledge.snapshot.id
                || snapshot.fingerprint != knowledge.snapshot.fingerprint
                || snapshot.hierarchies != knowledge.validity.hierarchy_revisions
            {
                return Err(Error::InvalidInput(
                    "Runtime knowledge differs from its immutable snapshot".into(),
                ));
            }
        } else {
            let hierarchies = self.knowledge_hierarchies()?;
            let mut artifact_revisions = std::collections::BTreeSet::new();
            for reference in &knowledge.validity.hierarchy_revisions {
                let hierarchy = hierarchies
                    .iter()
                    .find(|hierarchy| {
                        hierarchy.id == reference.id && hierarchy.revision == reference.revision
                    })
                    .ok_or_else(|| {
                        Error::Intervention(
                            "Hierarchy changed before runtime knowledge persistence".into(),
                        )
                    })?;
                let content_hash = crate::store::knowledge_runtime::hash(hierarchy)?;
                if content_hash != reference.content_hash {
                    return Err(Error::Intervention(
                        "Hierarchy changed before runtime knowledge persistence".into(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO knowledge_hierarchy_revisions(id,revision,hash,data) VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
                    params![hierarchy.id.to_string(), i64::try_from(hierarchy.revision).map_err(|_| Error::InvalidInput("Revision overflow".into()))?, content_hash, serde_json::to_string(hierarchy)?],
                )?;
                let stored_hash: String = transaction.query_row(
                    "SELECT hash FROM knowledge_hierarchy_revisions
                     WHERE id=?1 AND revision=?2",
                    params![
                        hierarchy.id.to_string(),
                        i64::try_from(hierarchy.revision)
                            .map_err(|_| Error::InvalidInput("Revision overflow".into()))?
                    ],
                    |row| row.get(0),
                )?;
                if stored_hash != content_hash {
                    return Err(Error::Intervention(
                        "Hierarchy revision names different immutable contents".into(),
                    ));
                }
                artifact_revisions.extend(hierarchy.nodes.values().map(|node| {
                    crate::knowledge_runtime::KnowledgeRevisionRef::from(&node.artifact)
                }));
            }
            let policy = crate::hierarchy::KnowledgeResolutionPolicy::default();
            let artifact_revisions = artifact_revisions.into_iter().collect::<Vec<_>>();
            let fingerprint = crate::store::knowledge_runtime::hash(&(
                crate::knowledge_runtime::RESOLUTION_POLICY_VERSION,
                &knowledge.validity.hierarchy_revisions,
                &artifact_revisions,
                &policy,
            ))?;
            if fingerprint != knowledge.snapshot.fingerprint {
                return Err(Error::InvalidInput(
                    "Runtime knowledge snapshot fingerprint changed before persistence".into(),
                ));
            }
            let snapshot = crate::knowledge_runtime::KnowledgeSnapshot {
                id: knowledge.snapshot.id.clone(),
                created_at: record.created_at,
                hierarchies: knowledge.validity.hierarchy_revisions.clone(),
                artifact_revisions,
                resolution_policy_version: crate::knowledge_runtime::RESOLUTION_POLICY_VERSION
                    .into(),
                policy,
                fingerprint,
            };
            transaction.execute(
                "INSERT INTO knowledge_snapshots(id,fingerprint,data) VALUES(?1,?2,?3)",
                params![
                    snapshot.id.to_string(),
                    snapshot.fingerprint,
                    serde_json::to_string(&snapshot)?
                ],
            )?;
            transaction.execute(
                "INSERT INTO knowledge_events(kind,data) VALUES('knowledge_snapshot_created',?1)",
                [serde_json::to_string(&snapshot.id)?],
            )?;
        }
        for (id, conflict) in knowledge
            .provenance
            .conflicts
            .iter()
            .zip(&knowledge.effective.conflicts)
        {
            let stored = crate::knowledge_runtime::StoredKnowledgeConflict {
                id: id.clone(),
                conflict: conflict.clone(),
                snapshot: knowledge.snapshot.id.clone(),
                context: knowledge.context.clone(),
                resolved: false,
            };
            transaction.execute(
                "INSERT INTO operational_knowledge_conflicts(id,data) VALUES(?1,?2) ON CONFLICT DO NOTHING",
                params![id.to_string(), serde_json::to_string(&stored)?],
            )?;
        }
        let resolution = crate::knowledge_runtime::KnowledgeResolutionRecord {
            id: knowledge.provenance.resolution_id.clone(),
            snapshot: knowledge.snapshot.id.clone(),
            context_hash: knowledge.validity.context_hash.clone(),
            context: knowledge.context.clone(),
            context_conflicts: knowledge.context_conflicts.clone(),
            effective: knowledge.effective.clone(),
            created_at: record.created_at,
        };
        let inserted = transaction.execute(
            "INSERT INTO knowledge_resolution_records(id,snapshot,data) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",
            params![
                resolution.id.to_string(),
                resolution.snapshot.to_string(),
                serde_json::to_string(&resolution)?
            ],
        )?;
        if inserted == 0 {
            let saved: String = transaction.query_row(
                "SELECT data FROM knowledge_resolution_records WHERE id=?1",
                [resolution.id.to_string()],
                |row| row.get(0),
            )?;
            if serde_json::to_value(serde_json::from_str::<
                crate::knowledge_runtime::KnowledgeResolutionRecord,
            >(&saved)?)?
                != serde_json::to_value(&resolution)?
            {
                return Err(Error::InvalidInput(
                    "Knowledge resolution id names different immutable contents".into(),
                ));
            }
        } else {
            transaction.execute(
                "INSERT INTO knowledge_events(kind,data)
                 VALUES('knowledge_resolution_recorded',?1)",
                [serde_json::to_string(&resolution.id)?],
            )?;
        }
        Ok(())
    }

    fn persist_knowledge_applications_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        record: &RuntimeDecisionRecord,
    ) -> Result<()> {
        let Some(knowledge) = &record.context.operational_knowledge else {
            return Ok(());
        };
        for applied in &knowledge.effective.applied {
            let application = crate::knowledge_runtime::KnowledgeApplication {
                id: crate::core::KnowledgeApplicationId::new(),
                knowledge: crate::knowledge_runtime::KnowledgeRevisionRef::from(&applied.artifact),
                resolution: knowledge.provenance.resolution_id.clone(),
                role: applied.role,
                runtime_decision: record.id.clone(),
                outcome: None,
            };
            transaction.execute(
                "INSERT INTO knowledge_applications(id,resolution,decision,data)
                 VALUES(?1,?2,?3,?4)",
                params![
                    application.id.to_string(),
                    application.resolution.to_string(),
                    application.runtime_decision.to_string(),
                    serde_json::to_string(&application)?
                ],
            )?;
            transaction.execute(
                "INSERT INTO knowledge_events(kind,data) VALUES('knowledge_applied',?1)",
                [serde_json::to_string(&application)?],
            )?;
        }
        Ok(())
    }
}

impl RuntimeStore for Store {
    fn record_runtime_decision(
        &self,
        context: &RuntimeDecisionContext,
        config: RuntimePolicyConfig,
    ) -> Result<RuntimeDecisionRecord> {
        let prepared = self.prepare_runtime_decision_from_context(context, config)?;
        let record = prepared.record.clone();
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.persist_prepared_runtime_decision(&transaction, &prepared)?;
        transaction.commit()?;
        Ok(record)
    }

    fn persist_runtime_decision(
        &self,
        record: &RuntimeDecisionRecord,
        config: RuntimePolicyConfig,
    ) -> Result<()> {
        let prepared = self.prepare_runtime_decision(record, config)?;
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.persist_prepared_runtime_decision(&transaction, &prepared)?;
        transaction.commit()?;
        Ok(())
    }

    fn runtime_decision(&self, id: &RuntimeDecisionId) -> Result<RuntimeDecisionRecord> {
        let record: RuntimeDecisionRecord = self.get(
            "SELECT data FROM runtime_decisions WHERE id=?1",
            &id.to_string(),
        )?;
        verify_record(self, &record)?;
        Ok(record)
    }

    fn runtime_decisions(&self) -> Result<Vec<RuntimeDecisionRecord>> {
        let records: Vec<RuntimeDecisionRecord> =
            self.list("SELECT data FROM runtime_decisions ORDER BY created_at,id")?;
        for record in &records {
            verify_record(self, record)?;
        }
        Ok(records)
    }

    fn record_runtime_feedback(&self, feedback: &RuntimeDecisionFeedback) -> Result<()> {
        let record = self.runtime_decision(&feedback.decision_id)?;
        if feedback.observed_at < record.created_at {
            return Err(Error::InvalidInput(
                "Runtime feedback cannot predate its decision".into(),
            ));
        }
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO runtime_decision_feedback(decision_id,observed_at,outcome,data) VALUES(?1,?2,?3,?4)",
            params![feedback.decision_id.to_string(), feedback.observed_at.to_rfc3339(), enum_name(&feedback.outcome)?, serde_json::to_string(feedback)?],
        )?;
        if feedback.outcome == DecisionOutcome::UnnecessaryIntervention
            && matches!(record.decision, RuntimeDecision::Replan(_))
        {
            lower_false_positive_reflexes(self, &transaction, &record, feedback)?;
        }
        if feedback.agent_disagreed {
            transaction.execute(
                "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(?1,?2,'agent_disagreed',?3,?4)",
                params![record.id.to_string(), record.session_id.to_string(), feedback.observed_at.to_rfc3339(), serde_json::to_string(feedback)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn runtime_feedback(&self, id: &RuntimeDecisionId) -> Result<Vec<RuntimeDecisionFeedback>> {
        self.runtime_decision(id)?;
        let mut statement = self.connection.prepare(
            "SELECT data FROM runtime_decision_feedback WHERE decision_id=?1 ORDER BY observed_at",
        )?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn runtime_audit(&self, limit: usize) -> Result<RuntimeAudit> {
        if limit == 0 || limit > 10_000 {
            return Err(Error::InvalidInput(
                "Runtime audit limit must be between 1 and 10000".into(),
            ));
        }
        let records = self.runtime_decisions()?;
        let mut audit = RuntimeAudit::default();
        for record in records.iter().rev().take(limit) {
            *audit.decisions.entry(record.decision.kind()).or_default() += 1;
            audit.total += 1;
            for feedback in self.runtime_feedback(&record.id)? {
                *audit.outcomes.entry(feedback.outcome).or_default() += 1;
            }
        }
        Ok(audit)
    }

    fn runtime_gaps(&self) -> Result<Vec<RuntimeGap>> {
        let mut grouped: BTreeMap<RuntimeGapKey, RuntimeGapAggregate> = BTreeMap::new();
        for record in self.runtime_decisions()? {
            if !matches!(
                record.evaluation.knowledge,
                KnowledgeState::Unknown
                    | KnowledgeState::KnownContradicted
                    | KnowledgeState::KnownStale
                    | KnowledgeState::OutOfScope
            ) && !matches!(
                record.decision,
                RuntimeDecision::Experiment(_) | RuntimeDecision::Abstain(_)
            ) {
                continue;
            }
            let key = (
                record.context_hash.clone(),
                record.context.task.family.clone(),
                record.evaluation.knowledge,
                record.decision.kind(),
            );
            let entry = grouped.entry(key).or_default();
            entry.0 += 1;
            entry.1.extend(
                record
                    .evaluation
                    .reasons
                    .iter()
                    .map(|reason| format!("{reason:?}")),
            );
            entry.1.sort();
            entry.1.dedup();
        }
        let mut gaps = grouped
            .into_iter()
            .map(
                |((context_hash, family, knowledge, decision), (occurrences, reasons))| {
                    RuntimeGap {
                        context_hash,
                        task_family: family.clone(),
                        knowledge,
                        decision,
                        occurrences,
                        reasons,
                        curriculum_recommendation: format!(
                            "Plan bounded curriculum for {} to resolve {:?} evidence",
                            family.as_deref().unwrap_or("this task family"),
                            knowledge
                        ),
                    }
                },
            )
            .collect::<Vec<_>>();
        gaps.sort_by(|left, right| {
            right
                .occurrences
                .cmp(&left.occurrences)
                .then_with(|| left.context_hash.cmp(&right.context_hash))
        });
        Ok(gaps)
    }

    fn runtime_curriculum_recommendations(
        &self,
    ) -> Result<Vec<crate::curriculum::CurriculumRecommendation>> {
        let records = self.runtime_decisions()?;
        self.runtime_gaps()?
            .into_iter()
            .map(|gap| {
                let record = records
                    .iter()
                    .find(|record| record.context_hash == gap.context_hash)
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Runtime gap no longer has a decision provenance record".into(),
                        )
                    })?;
                Ok(crate::curriculum::CurriculumRecommendation {
                    target: crate::curriculum::CurriculumTarget::Repository(
                        record.context.query_context.repository.clone(),
                    ),
                    gaps: vec![crate::curriculum::EvidenceGap {
                        dimension: "runtime_control".into(),
                        known_values: gap.reasons.clone(),
                        unknown_values: record.context.known_unknowns.clone(),
                        rationale: gap.curriculum_recommendation.clone(),
                    }],
                    rationale: format!(
                        "{} recurring {:?} decision(s) for knowledge state {:?}",
                        gap.occurrences, gap.decision, gap.knowledge
                    ),
                    auto_run: false,
                })
            })
            .collect()
    }

    fn runtime_development_metrics(&self) -> Result<RuntimeDevelopmentMetrics> {
        let records = self.runtime_decisions()?;
        let mut metrics = RuntimeDevelopmentMetrics::default();
        for record in &records {
            *metrics.decisions.entry(record.decision.kind()).or_default() += 1;
            for feedback in self.runtime_feedback(&record.id)? {
                match feedback.outcome {
                    DecisionOutcome::AvoidedFailure => metrics.avoided_failures += 1,
                    DecisionOutcome::UnnecessaryIntervention => {
                        metrics.unnecessary_interventions += 1
                    }
                    _ => {}
                }
            }
        }
        let total = records.len() as f64;
        let experiments = metrics
            .decisions
            .get(&RuntimeDecisionKind::Experiment)
            .copied()
            .unwrap_or(0);
        metrics.experiments_per_task = (total > 0.0).then_some(experiments as f64 / total);
        let evaluated_interventions = metrics.avoided_failures + metrics.unnecessary_interventions;
        metrics.unnecessary_intervention_rate = (evaluated_interventions > 0)
            .then_some(metrics.unnecessary_interventions as f64 / evaluated_interventions as f64);
        let recovery_records = records
            .iter()
            .filter(|record| record.decision.kind() == RuntimeDecisionKind::Recover)
            .collect::<Vec<_>>();
        let mut recovery_feedback = 0_u64;
        let mut recovery_success = 0_u64;
        for record in recovery_records {
            for feedback in self.runtime_feedback(&record.id)? {
                recovery_feedback += 1;
                recovery_success += u64::from(matches!(
                    feedback.outcome,
                    DecisionOutcome::Successful | DecisionOutcome::AvoidedFailure
                ));
            }
        }
        metrics.recovery_success_rate =
            (recovery_feedback > 0).then_some(recovery_success as f64 / recovery_feedback as f64);
        Ok(metrics)
    }

    fn replay_runtime_decision(
        &self,
        id: &RuntimeDecisionId,
        config: RuntimePolicyConfig,
    ) -> Result<RuntimeDecisionRecord> {
        let previous = self.runtime_decision(id)?;
        let mut current = previous.context.clone();
        self.attach_plan_identity(&mut current)?;
        self.attach_composition_knowledge(&mut current)?;
        current.operational_knowledge = if self.knowledge_hierarchies()?.is_empty() {
            None
        } else {
            use crate::knowledge_runtime::RuntimeKnowledgeResolver;
            Some(
                crate::knowledge_runtime::DefaultRuntimeKnowledgeResolver {
                    store: self,
                    policy: Default::default(),
                    budget: Default::default(),
                    persist: false,
                }
                .resolve_for_runtime(&current)?,
            )
        };
        self.attach_plan_validity(&mut current, false)?;
        self.attach_team_authority(&mut current)?;
        let evaluation = DeterministicRuntimeController::with_config(config)?.evaluate(&current)?;
        Ok(RuntimeDecisionRecord {
            id: RuntimeDecisionId::new(),
            session_id: current.session_id.clone(),
            context_hash: current.context_hash()?,
            context: current,
            decision: evaluation.decision.clone(),
            evaluation,
            created_at: Utc::now(),
        })
    }
}

fn verify_record(store: &Store, record: &RuntimeDecisionRecord) -> Result<()> {
    if record.context_hash != record.context.context_hash()?
        || record.session_id != record.context.session_id
        || serde_json::to_value(&record.decision)?
            != serde_json::to_value(&record.evaluation.decision)?
    {
        return Err(Error::InvalidInput(
            "Runtime decision record context or duplicated decision is inconsistent".into(),
        ));
    }
    let data: Option<String> = store
        .connection
        .query_row(
            "SELECT data FROM runtime_policy_versions WHERE version=?1",
            [record.evaluation.policy_version.clone()],
            |row| row.get(0),
        )
        .optional()?;
    let config: RuntimePolicyConfig = serde_json::from_str(&data.ok_or_else(|| {
        Error::InvalidInput("Runtime decision references a missing policy version".into())
    })?)?;
    let evaluation =
        DeterministicRuntimeController::with_config(config)?.evaluate(&record.context)?;
    if serde_json::to_value(evaluation)? != serde_json::to_value(&record.evaluation)? {
        return Err(Error::InvalidInput(
            "Stored runtime decision does not match its deterministic policy evaluation".into(),
        ));
    }
    Ok(())
}

fn lower_false_positive_reflexes(
    store: &Store,
    transaction: &Transaction<'_>,
    record: &RuntimeDecisionRecord,
    feedback: &RuntimeDecisionFeedback,
) -> Result<()> {
    let mut contradictory_experiences = feedback
        .evidence
        .iter()
        .filter_map(|evidence| match evidence {
            crate::runtime::EvidenceRef::Experience(id) => id.parse().ok(),
            _ => None,
        })
        .collect::<Vec<_>>();
    contradictory_experiences.sort();
    contradictory_experiences.dedup();
    for id in &contradictory_experiences {
        store.experience(id)?;
    }
    for matched in &record.context.matched_reflexes {
        let mut reflex = store.reflex(&matched.id)?;
        if !matches!(
            reflex.status,
            ReflexStatus::Active | ReflexStatus::Supported
        ) {
            continue;
        }
        reflex.status = ReflexStatus::Disabled;
        reflex.confidence = 0.30.try_into()?;
        reflex.version += 1;
        reflex.updated_at = feedback.observed_at;
        reflex.evidence.extend(
            contradictory_experiences
                .iter()
                .cloned()
                .map(|experience_id| LessonEvidenceRef::Experience {
                    experience_id,
                    relationship: EvidenceRelationship::Contradicts,
                }),
        );
        let changed = transaction.execute(
            "UPDATE reflexes SET version=?2,data=?3 WHERE id=?1 AND version=?4",
            params![
                matched.id.to_string(),
                reflex.version,
                serde_json::to_string(&reflex)?,
                reflex.version - 1
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention(
                "Concurrent Reflex revision while applying runtime feedback".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO reflex_versions(reflex_id,version,data) VALUES(?1,?2,?3)",
            params![
                matched.id.to_string(),
                reflex.version,
                serde_json::to_string(&reflex)?
            ],
        )?;
        for evidence in contradictory_experiences.iter() {
            transaction.execute(
                "INSERT INTO reflex_evidence(reflex_id,experience_id,relationship) VALUES(?1,?2,?3) ON CONFLICT(reflex_id,experience_id) DO NOTHING",
                params![matched.id.to_string(), evidence.to_string(), serde_json::to_string(&EvidenceRelationship::Contradicts)?],
            )?;
        }
        let test = crate::resilience::ResilienceTest {
            id: crate::core::ResilienceTestId::new(),
            reflex_id: Some(matched.id.clone()),
            recovery_id: None,
            source_trial: reflex.source_trial.clone(),
            perturbations: Vec::new(),
            without: contradictory_experiences.first().cloned(),
            with: contradictory_experiences.get(1).cloned(),
            status: ResilienceTestStatus::FalsePositive,
            false_positive: Some(true),
            created_at: feedback.observed_at,
            reason: "Controlled runtime counterfactual marked this intervention unnecessary".into(),
        };
        transaction.execute(
            "INSERT INTO resilience_tests(id,created_at,reflex_id,recovery_id,source_trial,status,data) VALUES(?1,?2,?3,NULL,?4,'false_positive',?5)",
            params![
                test.id.to_string(),
                feedback.observed_at.to_rfc3339(),
                matched.id.to_string(),
                reflex.source_trial.to_string(),
                serde_json::to_string(&test)?
            ],
        )?;
    }
    Ok(())
}

fn decision_event(decision: &RuntimeDecision) -> Option<RuntimeControlEventKind> {
    match decision {
        RuntimeDecision::Act(_) => None,
        RuntimeDecision::Experiment(_) => Some(RuntimeControlEventKind::ExperimentSuggested),
        RuntimeDecision::Replan(_) => Some(RuntimeControlEventKind::ReplanRequested),
        RuntimeDecision::Recover(_) => Some(RuntimeControlEventKind::RecoverySelected),
        RuntimeDecision::RequireApproval(_) => Some(RuntimeControlEventKind::ApprovalRequired),
        RuntimeDecision::Abstain(_) => Some(RuntimeControlEventKind::Abstained),
    }
}

fn sql_position(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidInput("Position exceeds SQLite range".into()))
}

fn enum_name(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::InvalidInput("Expected serialized enum name".into()))
}

fn tagged_kind(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::InvalidInput("Expected tagged runtime value".into()))
}

fn evidence_parts(evidence: &crate::runtime::EvidenceRef) -> (&'static str, Option<String>) {
    match evidence {
        crate::runtime::EvidenceRef::Skill(reference) => ("skill", Some(reference.id.to_string())),
        crate::runtime::EvidenceRef::Lesson(reference) => {
            ("lesson", Some(reference.id.to_string()))
        }
        crate::runtime::EvidenceRef::Reflex(reference) => {
            ("reflex", Some(reference.id.to_string()))
        }
        crate::runtime::EvidenceRef::Recovery { id, .. } => ("recovery", Some(id.to_string())),
        crate::runtime::EvidenceRef::Certification(id) => ("certification", Some(id.to_string())),
        crate::runtime::EvidenceRef::OperatingEnvelope { id, .. } => {
            ("operating_envelope", Some(id.to_string()))
        }
        crate::runtime::EvidenceRef::ExternalAdvisory(id) => {
            ("external_advisory", Some(id.clone()))
        }
        crate::runtime::EvidenceRef::Experience(id) => ("experience", Some(id.clone())),
        crate::runtime::EvidenceRef::Custom(id) => ("custom", Some(id.clone())),
    }
}
