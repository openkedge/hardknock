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

impl RuntimeStore for Store {
    fn record_runtime_decision(
        &self,
        context: &RuntimeDecisionContext,
        mut config: RuntimePolicyConfig,
    ) -> Result<RuntimeDecisionRecord> {
        config.refresh_version();
        config.validate()?;
        let evaluation =
            DeterministicRuntimeController::with_config(config.clone())?.evaluate(context)?;
        let record = RuntimeDecisionRecord {
            id: RuntimeDecisionId::new(),
            session_id: context.session_id.clone(),
            context_hash: context.context_hash()?,
            context: context.clone(),
            decision: evaluation.decision.clone(),
            evaluation,
            created_at: Utc::now(),
        };
        self.persist_runtime_decision(&record, config)?;
        Ok(record)
    }

    fn persist_runtime_decision(
        &self,
        record: &RuntimeDecisionRecord,
        mut config: RuntimePolicyConfig,
    ) -> Result<()> {
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
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO runtime_policy_versions(version,created_at,data) VALUES(?1,?2,?3) ON CONFLICT(version) DO NOTHING",
            params![config.version, record.created_at.to_rfc3339(), serde_json::to_string(&config)?],
        )?;
        let stored_config: String = transaction.query_row(
            "SELECT data FROM runtime_policy_versions WHERE version=?1",
            [record.evaluation.policy_version.clone()],
            |row| row.get(0),
        )?;
        if serde_json::from_str::<RuntimePolicyConfig>(&stored_config)? != config {
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
                serde_json::to_string(&record)?
            ],
        )?;
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
        let event = decision_event(&record.decision);
        transaction.execute(
            "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(?1,?2,'runtime_decision_made',?3,?4)",
            params![record.id.to_string(), record.session_id.to_string(), record.created_at.to_rfc3339(), serde_json::to_string(&serde_json::json!({"decision":record.decision.kind(),"policy_version":record.evaluation.policy_version}))?],
        )?;
        if let Some(event) = event {
            transaction.execute(
                "INSERT INTO runtime_control_events(decision_id,session_id,kind,created_at,data) VALUES(?1,?2,?3,?4,?5)",
                params![record.id.to_string(), record.session_id.to_string(), enum_name(&event)?, record.created_at.to_rfc3339(), serde_json::to_string(&record.decision)?],
            )?;
        }
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
        let old = &previous.context;
        let mut current =
            RuntimeContextSynthesizer { store: self }.synthesize(RuntimeContextRequest {
                external_session_id: old.session_id.to_string(),
                agent: old.agent.clone(),
                task: old.task.clone(),
                query_context: old.query_context.clone(),
                proposed_action: old.proposed_action.clone(),
                proposed_effect: old.proposed_effect.clone(),
                risk: Some(old.risk.clone()),
                capability_context: old.capability_context.clone(),
                failure_signature: old.failure_signature.clone(),
                consecutive_failures: 0,
                no_state_change: false,
                config_changed: false,
                candidate_strategies: old.uncertainty.candidate_strategies.clone(),
                experiment_capability: old.available_experiments.clone(),
                known_unknowns: old.known_unknowns.clone(),
                externally_supported: old.externally_supported,
                envelope_position: None,
            })?;
        current.session_id = old.session_id.clone();
        self.record_runtime_decision(&current, config)
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
