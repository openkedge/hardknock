// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::Store;
use crate::{
    Error, Result,
    abstraction::*,
    core::{AbstractKnowledgeId, ExperiencePatternId, TransferHypothesisId},
    epistemic::EvidenceRef,
    lesson::EvidenceRef as LessonEvidenceRef,
    resilience::RecoveryStep,
};

fn json(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn name(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or("custom")
        .to_owned())
}

fn selector_variables(selector: &crate::lesson::ContextSelector) -> Vec<ContextVariable> {
    let mut variables = Vec::new();
    if let Some(repository) = &selector.repository {
        variables.push(ContextVariable {
            name: "repository".into(),
            kind: ContextVariableKind::Resource,
            value: VariableValue::Text(repository.display().to_string()),
            relevance: ContextRelevance::Varies,
        });
    }
    for (name, values) in [
        ("markers", &selector.required_markers),
        ("tags", &selector.tags),
    ] {
        if !values.is_empty() {
            let mut values = values.clone();
            values.sort();
            variables.push(ContextVariable {
                name: name.into(),
                kind: ContextVariableKind::Environment,
                value: VariableValue::Set(values),
                relevance: ContextRelevance::Varies,
            });
        }
    }
    for (name, value) in [("os", &selector.os), ("arch", &selector.arch)] {
        if let Some(value) = value {
            variables.push(ContextVariable {
                name: name.into(),
                kind: ContextVariableKind::Environment,
                value: VariableValue::Text(value.clone()),
                relevance: ContextRelevance::Varies,
            });
        }
    }
    variables
}

fn event(tx: &Transaction<'_>, subject: &str, kind: &str, data: serde_json::Value) -> Result<()> {
    tx.execute(
        "INSERT INTO abstraction_events(subject,kind,data) VALUES(?1,?2,?3)",
        params![subject, kind, data.to_string()],
    )?;
    tx.execute(
        "INSERT INTO bridge_events(session_id,kind,data) VALUES('experience-abstraction',?1,?2)",
        params![kind, serde_json::json!({"subject":subject}).to_string()],
    )?;
    Ok(())
}

fn validate_pattern(pattern: &ExperiencePattern) -> Result<()> {
    if pattern.name.trim().is_empty()
        || pattern.name.len() > 200
        || pattern.members.len() < 2
        || pattern.members.len() > 10_000
    {
        return Err(Error::InvalidInput(
            "An ExperiencePattern needs a name of 1..200 bytes and 2..10000 members".into(),
        ));
    }
    let distinct = pattern.members.iter().collect::<BTreeSet<_>>();
    if distinct.len() != pattern.members.len() {
        return Err(Error::InvalidInput(
            "ExperiencePattern members must be distinct".into(),
        ));
    }
    Ok(())
}

fn validate_abstract(knowledge: &AbstractKnowledge) -> Result<()> {
    if knowledge.statement.trim().is_empty()
        || knowledge.statement.len() > 8192
        || knowledge.revision == 0
        || knowledge.provenance.source_artifacts.len() < 2
        || knowledge.supporting_patterns.is_empty()
    {
        return Err(Error::InvalidInput(
            "Abstract knowledge requires a statement, positive revision, pattern, and at least two source artifacts"
                .into(),
        ));
    }
    Ok(())
}

impl Store {
    pub fn save_experience_pattern(&self, pattern: &ExperiencePattern) -> Result<()> {
        validate_pattern(pattern)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let existed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM experience_patterns WHERE id=?1)",
            [pattern.id.to_string()],
            |row| row.get(0),
        )?;
        if existed {
            tx.execute(
                "UPDATE experience_patterns SET kind=?2,status=?3,updated_at=?4,data=?5 WHERE id=?1",
                params![
                    pattern.id.to_string(),
                    name(&pattern.kind)?,
                    name(&pattern.status)?,
                    pattern.updated_at.to_rfc3339(),
                    json(pattern)?
                ],
            )?;
            tx.execute(
                "DELETE FROM experience_pattern_members WHERE pattern_id=?1",
                [pattern.id.to_string()],
            )?;
        } else {
            tx.execute(
                "INSERT INTO experience_patterns(id,kind,status,created_at,updated_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
                params![pattern.id.to_string(),name(&pattern.kind)?,name(&pattern.status)?,pattern.created_at.to_rfc3339(),pattern.updated_at.to_rfc3339(),json(pattern)?],
            )?;
            event(
                &tx,
                &pattern.id.to_string(),
                "abstraction_candidate_created",
                serde_json::json!({"members":pattern.members.len(),"kind":pattern.kind}),
            )?;
        }
        for (position, member) in pattern.members.iter().enumerate() {
            tx.execute(
                "INSERT INTO experience_pattern_members(pattern_id,position,artifact_kind,artifact_id,artifact_revision) VALUES(?1,?2,?3,?4,?5)",
                params![pattern.id.to_string(),i64::try_from(position).map_err(|_|Error::InvalidInput("Pattern position overflow".into()))?,name(&member.kind)?,member.id,i64::try_from(member.revision).map_err(|_|Error::InvalidInput("Artifact revision overflow".into()))?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn experience_pattern(&self, id: &ExperiencePatternId) -> Result<ExperiencePattern> {
        self.get(
            "SELECT data FROM experience_patterns WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn experience_patterns(&self) -> Result<Vec<ExperiencePattern>> {
        self.list("SELECT data FROM experience_patterns ORDER BY created_at,id")
    }

    pub fn create_abstract_knowledge(
        &self,
        knowledge: &AbstractKnowledge,
        reason: &str,
    ) -> Result<()> {
        validate_abstract(knowledge)?;
        if reason.trim().is_empty() || reason.len() > 2048 {
            return Err(Error::InvalidInput(
                "Abstract knowledge revision reason must be 1..2048 bytes".into(),
            ));
        }
        for pattern in &knowledge.supporting_patterns {
            self.experience_pattern(pattern)?;
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO abstract_knowledge(id,revision,kind,maturity,origin,updated_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![knowledge.id.to_string(),i64::try_from(knowledge.revision).map_err(|_|Error::InvalidInput("Abstract revision overflow".into()))?,name(&knowledge.kind)?,name(&knowledge.maturity)?,name(&knowledge.provenance.origin)?,knowledge.updated_at.to_rfc3339(),json(knowledge)?],
        )?;
        tx.execute(
            "INSERT INTO abstract_knowledge_revisions(abstract_id,revision,reason,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![knowledge.id.to_string(),i64::try_from(knowledge.revision).map_err(|_|Error::InvalidInput("Abstract revision overflow".into()))?,reason,knowledge.updated_at.to_rfc3339(),json(knowledge)?],
        )?;
        tx.execute(
            "INSERT INTO generalization_boundaries(abstract_id,revision,data) VALUES(?1,?2,?3)",
            params![
                knowledge.id.to_string(),
                i64::try_from(knowledge.revision)
                    .map_err(|_| Error::InvalidInput("Abstract revision overflow".into()))?,
                json(&knowledge.generalization_boundary)?
            ],
        )?;
        event(
            &tx,
            &knowledge.id.to_string(),
            "abstract_knowledge_proposed",
            serde_json::json!({"kind":knowledge.kind,"revision":knowledge.revision}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn revise_abstract_knowledge(
        &self,
        knowledge: &AbstractKnowledge,
        reason: &str,
        event_kind: &str,
    ) -> Result<()> {
        validate_abstract(knowledge)?;
        if reason.trim().is_empty() || reason.len() > 2048 {
            return Err(Error::InvalidInput(
                "Abstract knowledge revision reason must be 1..2048 bytes".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let current: i64 = tx
            .query_row(
                "SELECT revision FROM abstract_knowledge WHERE id=?1",
                [knowledge.id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                Error::NotFound(format!("Abstract knowledge {} not found", knowledge.id))
            })?;
        let expected = i64::try_from(knowledge.revision.saturating_sub(1))
            .map_err(|_| Error::InvalidInput("Abstract revision overflow".into()))?;
        if current != expected {
            return Err(Error::Intervention(
                "Abstract knowledge changed concurrently; reload before revising".into(),
            ));
        }
        tx.execute(
            "INSERT INTO abstract_knowledge_revisions(abstract_id,revision,reason,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![knowledge.id.to_string(),i64::try_from(knowledge.revision).map_err(|_|Error::InvalidInput("Abstract revision overflow".into()))?,reason,knowledge.updated_at.to_rfc3339(),json(knowledge)?],
        )?;
        tx.execute(
            "INSERT INTO generalization_boundaries(abstract_id,revision,data) VALUES(?1,?2,?3)",
            params![
                knowledge.id.to_string(),
                i64::try_from(knowledge.revision)
                    .map_err(|_| Error::InvalidInput("Abstract revision overflow".into()))?,
                json(&knowledge.generalization_boundary)?
            ],
        )?;
        tx.execute(
            "UPDATE abstract_knowledge SET revision=?2,kind=?3,maturity=?4,origin=?5,updated_at=?6,data=?7 WHERE id=?1 AND revision=?8",
            params![knowledge.id.to_string(),i64::try_from(knowledge.revision).map_err(|_|Error::InvalidInput("Abstract revision overflow".into()))?,name(&knowledge.kind)?,name(&knowledge.maturity)?,name(&knowledge.provenance.origin)?,knowledge.updated_at.to_rfc3339(),json(knowledge)?,expected],
        )?;
        event(
            &tx,
            &knowledge.id.to_string(),
            event_kind,
            serde_json::json!({"reason":reason,"revision":knowledge.revision}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn abstract_knowledge(&self, id: &AbstractKnowledgeId) -> Result<AbstractKnowledge> {
        self.get(
            "SELECT data FROM abstract_knowledge WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn abstract_knowledge_items(&self) -> Result<Vec<AbstractKnowledge>> {
        self.list("SELECT data FROM abstract_knowledge ORDER BY updated_at,id")
    }

    pub fn abstract_knowledge_history(
        &self,
        id: &AbstractKnowledgeId,
    ) -> Result<Vec<AbstractKnowledge>> {
        let mut query = self.connection.prepare(
            "SELECT data FROM abstract_knowledge_revisions WHERE abstract_id=?1 ORDER BY revision",
        )?;
        query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn save_transfer_hypothesis(&self, hypothesis: &TransferHypothesis) -> Result<()> {
        self.abstract_knowledge(&hypothesis.abstract_knowledge.id)?;
        if hypothesis.source_contexts.is_empty()
            || hypothesis
                .source_contexts
                .iter()
                .any(|source| source == &hypothesis.target_context)
        {
            return Err(Error::InvalidInput(
                "Transfer hypothesis requires a target held out from nonempty source contexts"
                    .into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let existed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM transfer_hypotheses WHERE id=?1)",
            [hypothesis.id.to_string()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO transfer_hypotheses(id,abstract_id,status,created_at,data) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET status=excluded.status,data=excluded.data",
            params![hypothesis.id.to_string(),hypothesis.abstract_knowledge.id.to_string(),name(&hypothesis.status)?,hypothesis.created_at.to_rfc3339(),json(hypothesis)?],
        )?;
        if !existed {
            event(
                &tx,
                &hypothesis.id.to_string(),
                "abstraction_transfer_planned",
                serde_json::json!({"abstract":hypothesis.abstract_knowledge.id}),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn transfer_hypothesis(&self, id: &TransferHypothesisId) -> Result<TransferHypothesis> {
        self.get(
            "SELECT data FROM transfer_hypotheses WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn transfer_hypotheses_for(
        &self,
        id: &AbstractKnowledgeId,
    ) -> Result<Vec<TransferHypothesis>> {
        let mut query = self.connection.prepare(
            "SELECT data FROM transfer_hypotheses WHERE abstract_id=?1 ORDER BY created_at,id",
        )?;
        query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn save_transfer_evaluation_set(&self, set: &TransferEvaluationSet) -> Result<()> {
        self.transfer_hypothesis(&set.hypothesis)?;
        if set.held_out_contexts.is_empty()
            || set
                .held_out_contexts
                .iter()
                .any(|target| set.source_contexts.contains(target))
        {
            return Err(Error::InvalidInput(
                "Transfer evaluation set requires a genuinely held-out context".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO transfer_evaluation_sets(hypothesis_id,data) VALUES(?1,?2) ON CONFLICT(hypothesis_id) DO UPDATE SET data=excluded.data",
            params![set.hypothesis.to_string(),json(set)?],
        )?;
        Ok(())
    }

    pub fn transfer_evaluation_set(
        &self,
        id: &TransferHypothesisId,
    ) -> Result<TransferEvaluationSet> {
        self.get(
            "SELECT data FROM transfer_evaluation_sets WHERE hypothesis_id=?1",
            &id.to_string(),
        )
    }

    pub fn record_transfer_evidence(&self, evidence: &TransferEvidence) -> Result<()> {
        let mut hypothesis = self.transfer_hypothesis(&evidence.hypothesis)?;
        if hypothesis.abstract_knowledge != evidence.source_artifact {
            return Err(Error::InvalidInput(
                "Transfer evidence abstraction revision does not match its hypothesis".into(),
            ));
        }
        if evidence.context_role == TransferContextRole::HeldOut
            && hypothesis
                .source_contexts
                .contains(&evidence.target_context)
        {
            return Err(Error::InvalidInput(
                "A source context cannot be recorded as held-out transfer evidence".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO transfer_evidence(id,hypothesis_id,abstract_id,context_role,outcome,local,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![evidence.id.to_string(),evidence.hypothesis.to_string(),evidence.source_artifact.id.to_string(),name(&evidence.context_role)?,name(&evidence.outcome)?,evidence.local,evidence.created_at.to_rfc3339(),json(evidence)?],
        )?;
        let reference = TransferEvidenceRef {
            id: evidence.id.clone(),
        };
        hypothesis.evidence.push(reference);
        hypothesis.status = match evidence.outcome {
            TransferEvidenceOutcome::Supports | TransferEvidenceOutcome::NarrowsScope => {
                TransferHypothesisStatus::Supported
            }
            TransferEvidenceOutcome::Contradicts => TransferHypothesisStatus::Contradicted,
            TransferEvidenceOutcome::Inconclusive => TransferHypothesisStatus::Inconclusive,
            TransferEvidenceOutcome::Invalid => TransferHypothesisStatus::Untestable,
        };
        tx.execute(
            "UPDATE transfer_hypotheses SET status=?2,data=?3 WHERE id=?1",
            params![
                hypothesis.id.to_string(),
                name(&hypothesis.status)?,
                json(&hypothesis)?
            ],
        )?;
        let kind = match evidence.outcome {
            TransferEvidenceOutcome::Supports => "transfer_supported",
            TransferEvidenceOutcome::Contradicts => "transfer_contradicted",
            TransferEvidenceOutcome::NarrowsScope => "abstraction_scope_narrowing_observed",
            TransferEvidenceOutcome::Inconclusive => "transfer_inconclusive",
            TransferEvidenceOutcome::Invalid => "transfer_invalid",
        };
        event(
            &tx,
            &evidence.source_artifact.id.to_string(),
            kind,
            serde_json::json!({"evidence":evidence.id,"hypothesis":evidence.hypothesis}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn transfer_evidence_for(&self, id: &AbstractKnowledgeId) -> Result<Vec<TransferEvidence>> {
        let mut query = self.connection.prepare(
            "SELECT data FROM transfer_evidence WHERE abstract_id=?1 ORDER BY created_at,id",
        )?;
        query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn save_knowledge_exception(&self, exception: &KnowledgeException) -> Result<()> {
        self.abstract_knowledge(&exception.parent.id)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO knowledge_exceptions(id,parent_id,parent_revision,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![exception.id.to_string(),exception.parent.id.to_string(),i64::try_from(exception.parent.revision).map_err(|_|Error::InvalidInput("Parent revision overflow".into()))?,exception.created_at.to_rfc3339(),json(exception)?],
        )?;
        event(
            &tx,
            &exception.parent.id.to_string(),
            "knowledge_exception_created",
            serde_json::json!({"exception":exception.id}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn knowledge_exceptions_for(
        &self,
        id: &AbstractKnowledgeId,
    ) -> Result<Vec<KnowledgeException>> {
        let mut query = self.connection.prepare(
            "SELECT data FROM knowledge_exceptions WHERE parent_id=?1 ORDER BY created_at,id",
        )?;
        query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn save_knowledge_specialization(
        &self,
        specialization: &KnowledgeSpecialization,
    ) -> Result<()> {
        self.abstract_knowledge(&specialization.parent.id)?;
        self.abstract_knowledge(&specialization.child.id)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO knowledge_specializations(parent_id,parent_revision,child_id,child_revision,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![specialization.parent.id.to_string(),i64::try_from(specialization.parent.revision).map_err(|_|Error::InvalidInput("Parent revision overflow".into()))?,specialization.child.id.to_string(),i64::try_from(specialization.child.revision).map_err(|_|Error::InvalidInput("Child revision overflow".into()))?,specialization.created_at.to_rfc3339(),json(specialization)?],
        )?;
        event(
            &tx,
            &specialization.parent.id.to_string(),
            "knowledge_specialization_created",
            serde_json::json!({"child":specialization.child.id}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn knowledge_specializations_for(
        &self,
        id: &AbstractKnowledgeId,
    ) -> Result<Vec<KnowledgeSpecialization>> {
        let mut query = self.connection.prepare(
            "SELECT data FROM knowledge_specializations WHERE parent_id=?1 ORDER BY created_at,child_id",
        )?;
        query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn save_distillation(
        &self,
        distillation: &KnowledgeDistillation,
        representations: &[KnowledgeRepresentation],
    ) -> Result<()> {
        if distillation.inputs.is_empty() || distillation.outputs.is_empty() {
            return Err(Error::InvalidInput(
                "Distillation requires retained inputs and abstract outputs".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO knowledge_distillations(id,created_at,data) VALUES(?1,?2,?3)",
            params![
                distillation.id.to_string(),
                distillation.created_at.to_rfc3339(),
                json(distillation)?
            ],
        )?;
        for representation in representations {
            tx.execute(
                "INSERT INTO knowledge_representation_state(artifact_kind,artifact_id,artifact_revision,changed_at,data) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(artifact_kind,artifact_id,artifact_revision) DO UPDATE SET changed_at=excluded.changed_at,data=excluded.data",
                params![name(&representation.artifact.kind)?,representation.artifact.id,i64::try_from(representation.artifact.revision).map_err(|_|Error::InvalidInput("Artifact revision overflow".into()))?,representation.changed_at.to_rfc3339(),json(representation)?],
            )?;
            event(
                &tx,
                &representation.artifact.id,
                "specific_knowledge_represented",
                serde_json::json!({"distillation":distillation.id,"state":representation.state}),
            )?;
        }
        event(
            &tx,
            &distillation.id.to_string(),
            "knowledge_distillation_completed",
            serde_json::json!({"inputs":distillation.inputs.len(),"outputs":distillation.outputs.len()}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn knowledge_representations(&self) -> Result<Vec<KnowledgeRepresentation>> {
        self.list("SELECT data FROM knowledge_representation_state ORDER BY artifact_kind,artifact_id,artifact_revision")
    }

    pub fn reactivate_abstract_members(&self, id: &AbstractKnowledgeId) -> Result<usize> {
        let knowledge = self.abstract_knowledge(id)?;
        let current = self.knowledge_representations()?;
        let reactivated = reactivate_represented_members(&knowledge, &current);
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for record in &reactivated {
            tx.execute(
                "UPDATE knowledge_representation_state SET changed_at=?4,data=?5 WHERE artifact_kind=?1 AND artifact_id=?2 AND artifact_revision=?3",
                params![name(&record.artifact.kind)?,record.artifact.id,i64::try_from(record.artifact.revision).map_err(|_|Error::InvalidInput("Artifact revision overflow".into()))?,record.changed_at.to_rfc3339(),json(record)?],
            )?;
            event(
                &tx,
                &record.artifact.id,
                "specific_knowledge_reactivated",
                serde_json::json!({"abstract":id}),
            )?;
        }
        tx.commit()?;
        Ok(reactivated.len())
    }

    pub fn record_negative_transfer(&self, item: &NegativeTransferEvent) -> Result<()> {
        self.abstract_knowledge(&item.knowledge)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO negative_transfer_events(abstract_id,outcome,observed_at,data) VALUES(?1,?2,?3,?4)",
            params![item.knowledge.to_string(),name(&item.outcome)?,item.observed_at.to_rfc3339(),json(item)?],
        )?;
        event(
            &tx,
            &item.knowledge.to_string(),
            "negative_transfer_detected",
            serde_json::json!({"outcome":item.outcome}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn negative_transfer_events(&self) -> Result<Vec<NegativeTransferEvent>> {
        self.list("SELECT data FROM negative_transfer_events ORDER BY sequence")
    }

    pub fn save_analogy_mapping(&self, mapping: &AnalogyMapping) -> Result<()> {
        self.connection.execute(
            "INSERT INTO analogy_mappings(id,created_at,data) VALUES(?1,?2,?3)",
            params![
                mapping.id.to_string(),
                mapping.created_at.to_rfc3339(),
                json(mapping)?
            ],
        )?;
        Ok(())
    }

    pub fn abstraction_events(&self, subject: Option<&str>) -> Result<Vec<AbstractionEvent>> {
        let sql = if subject.is_some() {
            "SELECT sequence,subject,kind,created_at,data FROM abstraction_events WHERE subject=?1 ORDER BY sequence"
        } else {
            "SELECT sequence,subject,kind,created_at,data FROM abstraction_events ORDER BY sequence"
        };
        let mut query = self.connection.prepare(sql)?;
        let rows = if let Some(subject) = subject {
            query
                .query_map([subject], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(|(sequence, subject, kind, created_at, data)| {
                Ok(AbstractionEvent {
                    sequence: u64::try_from(sequence).map_err(|_| {
                        Error::InvalidInput("Negative abstraction event sequence".into())
                    })?,
                    subject,
                    kind,
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|error| {
                            Error::InvalidInput(format!(
                                "Invalid abstraction event timestamp: {error}"
                            ))
                        })?
                        .with_timezone(&Utc),
                    data: serde_json::from_str(&data)?,
                })
            })
            .collect()
    }

    pub fn knowledge_artifacts_for_abstraction(&self) -> Result<Vec<KnowledgeArtifact>> {
        let mut artifacts = Vec::new();
        let mut causal_by_artifact: BTreeMap<String, Vec<CausalHypothesisRef>> = BTreeMap::new();
        for hypothesis in self
            .causal_hypotheses()?
            .into_iter()
            .filter(|hypothesis| hypothesis.status.supported())
        {
            for dependency in self.causal_dependencies(&hypothesis.id)? {
                causal_by_artifact
                    .entry(dependency.artifact.key())
                    .or_default()
                    .push(CausalHypothesisRef {
                        id: hypothesis.id.clone(),
                    });
            }
        }
        for lesson in self.all_lessons()? {
            let context_variables = selector_variables(&lesson.context_match);
            let evidence = lesson
                .evidence
                .iter()
                .map(|item| match item {
                    LessonEvidenceRef::Experience { experience_id, .. } => EvidenceRef {
                        kind: "experience".into(),
                        id: experience_id.to_string(),
                    },
                    LessonEvidenceRef::Trial {
                        experiment_id,
                        trial_id,
                        ..
                    } => EvidenceRef {
                        kind: "trial".into(),
                        id: format!("{experiment_id}/{trial_id}"),
                    },
                })
                .collect();
            artifacts.push(KnowledgeArtifact {
                artifact: KnowledgeArtifactRef {
                    kind: KnowledgeArtifactKind::Lesson,
                    id: lesson.id.to_string(),
                    revision: u64::from(lesson.version),
                },
                statement: lesson.claim,
                scope: lesson.context_match,
                structure: PatternStructure {
                    trigger: lesson.avoid.as_ref().map(|_| PatternPredicate {
                        variable: "proposed_action".into(),
                        operator: PredicateOperator::Present,
                        value: None,
                    }),
                    context_variables,
                    action_pattern: lesson.avoid,
                    outcome_pattern: Some(OutcomePattern {
                        classification: "failure".into(),
                        observable: "Recorded Lesson source outcome".into(),
                    }),
                    causal_mechanisms: causal_by_artifact
                        .get(&lesson.id.to_string())
                        .cloned()
                        .unwrap_or_default(),
                    required_conditions: Vec::new(),
                },
                evidence,
                root_origins: vec![lesson.source_experience.to_string()],
                updated_at: lesson.updated_at,
            });
        }
        for skill in self.skills()? {
            let context_variables = selector_variables(&skill.context);
            artifacts.push(KnowledgeArtifact {
                artifact: KnowledgeArtifactRef {
                    kind: KnowledgeArtifactKind::Skill,
                    id: skill.id.to_string(),
                    revision: self
                        .skill_revisions(&skill.id)?
                        .last()
                        .map(|item| item.revision)
                        .unwrap_or(1),
                },
                statement: skill.description,
                scope: skill.context,
                structure: PatternStructure {
                    trigger: None,
                    context_variables,
                    action_pattern: skill.procedure.first().cloned(),
                    outcome_pattern: Some(OutcomePattern {
                        classification: "success".into(),
                        observable: "Skill evidence passed".into(),
                    }),
                    causal_mechanisms: causal_by_artifact
                        .get(&skill.id.to_string())
                        .cloned()
                        .unwrap_or_default(),
                    required_conditions: Vec::new(),
                },
                evidence: skill
                    .evidence
                    .iter()
                    .map(|item| match item {
                        LessonEvidenceRef::Experience { experience_id, .. } => EvidenceRef {
                            kind: "experience".into(),
                            id: experience_id.to_string(),
                        },
                        LessonEvidenceRef::Trial {
                            experiment_id,
                            trial_id,
                            ..
                        } => EvidenceRef {
                            kind: "trial".into(),
                            id: format!("{experiment_id}/{trial_id}"),
                        },
                    })
                    .collect(),
                root_origins: vec![skill.source_experience.to_string()],
                updated_at: Utc::now(),
            });
        }
        for recovery in self.recoveries()? {
            let context_variables = selector_variables(&recovery.context);
            let action_pattern = recovery.steps.first().map(|step| match step {
                RecoveryStep::ShellCommand { command } => crate::lesson::ActionPattern::Custom {
                    kind: "recovery_shell".into(),
                    value: format!("{:?}", command),
                },
                RecoveryStep::SetEnvironmentVariable { key, .. } => {
                    crate::lesson::ActionPattern::Custom {
                        kind: "recovery_environment".into(),
                        value: key.clone(),
                    }
                }
                RecoveryStep::Replan => crate::lesson::ActionPattern::Custom {
                    kind: "recovery".into(),
                    value: "replan".into(),
                },
            });
            artifacts.push(KnowledgeArtifact {
                artifact: KnowledgeArtifactRef {
                    kind: KnowledgeArtifactKind::Recovery,
                    id: recovery.id.to_string(),
                    revision: u64::from(recovery.version),
                },
                statement: format!("Recover from {}", recovery.failure_signature.signature),
                scope: recovery.context,
                structure: PatternStructure {
                    trigger: Some(PatternPredicate {
                        variable: "failure_signature".into(),
                        operator: PredicateOperator::Equals,
                        value: Some(VariableValue::Text(recovery.failure_signature.signature)),
                    }),
                    context_variables,
                    action_pattern,
                    outcome_pattern: Some(OutcomePattern {
                        classification: "recovery".into(),
                        observable: "Recovery trial result".into(),
                    }),
                    causal_mechanisms: causal_by_artifact
                        .get(&recovery.id.to_string())
                        .cloned()
                        .unwrap_or_default(),
                    required_conditions: Vec::new(),
                },
                evidence: recovery
                    .evidence
                    .iter()
                    .map(|item| match item {
                        LessonEvidenceRef::Experience { experience_id, .. } => EvidenceRef {
                            kind: "experience".into(),
                            id: experience_id.to_string(),
                        },
                        LessonEvidenceRef::Trial {
                            experiment_id,
                            trial_id,
                            ..
                        } => EvidenceRef {
                            kind: "trial".into(),
                            id: format!("{experiment_id}/{trial_id}"),
                        },
                    })
                    .collect(),
                root_origins: vec![recovery.source_trial.to_string()],
                updated_at: recovery.updated_at,
            });
        }
        Ok(artifacts)
    }

    pub fn discover_abstraction_candidates(&self) -> Result<Vec<CandidateAbstraction>> {
        let candidates = DeterministicAbstractionCandidateProvider.propose(
            &self.knowledge_artifacts_for_abstraction()?,
            &AbstractionContext::default(),
        )?;
        for candidate in &candidates {
            self.save_experience_pattern(&candidate.pattern)?;
            if self.abstract_knowledge(&candidate.knowledge.id).is_err() {
                self.create_abstract_knowledge(
                    &candidate.knowledge,
                    "Deterministic structural candidate discovery",
                )?;
            }
        }
        Ok(candidates)
    }

    pub fn abstraction_impact(&self, id: &AbstractKnowledgeId) -> Result<AbstractionImpact> {
        let knowledge = self.abstract_knowledge(id)?;
        let events = self.abstraction_events(Some(&id.to_string()))?;
        let transfer = self.transfer_evidence_for(id)?;
        let guard = assess_guard_candidate(&knowledge, &transfer);
        let mut query = self.connection.prepare(
            "SELECT c.data,m.data FROM skill_certifications c JOIN evidence_manifests m ON m.id=c.evidence_manifest_id ORDER BY c.issued_at,c.id",
        )?;
        let certification_rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut certifications = Vec::new();
        for (certification, manifest) in certification_rows {
            let certification: crate::assurance::SkillCertification =
                serde_json::from_str(&certification)?;
            let manifest: crate::assurance::EvidenceManifest = serde_json::from_str(&manifest)?;
            if manifest
                .abstract_knowledge
                .iter()
                .any(|reference| reference.id == *id)
            {
                certifications.push(certification.id.to_string());
            }
        }
        Ok(AbstractionImpact {
            runtime_uses: events
                .iter()
                .filter(|item| item.kind == "abstract_knowledge_applied")
                .count(),
            skills_influenced: knowledge
                .provenance
                .source_artifacts
                .iter()
                .filter(|item| item.kind == KnowledgeArtifactKind::Skill)
                .map(|item| item.id.clone())
                .collect(),
            constraints_influenced: knowledge
                .provenance
                .source_artifacts
                .iter()
                .filter(|item| item.kind == KnowledgeArtifactKind::Constraint)
                .map(|item| item.id.clone())
                .collect(),
            recoveries_influenced: knowledge
                .provenance
                .source_artifacts
                .iter()
                .filter(|item| item.kind == KnowledgeArtifactKind::Recovery)
                .map(|item| item.id.clone())
                .collect(),
            guard_candidates: guard
                .eligible_for_governance_review
                .then(|| format!("external-governance-review:{}@{}", id, knowledge.revision))
                .into_iter()
                .collect(),
            certifications,
        })
    }

    pub fn runtime_abstraction_candidates(&self) -> Result<Vec<KnowledgeCandidateRef>> {
        let representations = self.knowledge_representations()?;
        let mut result = Vec::new();
        for knowledge in self.abstract_knowledge_items()? {
            if !matches!(
                knowledge.maturity,
                KnowledgeMaturity::Supported | KnowledgeMaturity::Validated
            ) || knowledge.provenance.origin == KnowledgeOrigin::FederatedAdvisory
            {
                continue;
            }
            let scope = knowledge
                .supporting_patterns
                .first()
                .and_then(|id| self.experience_pattern(id).ok())
                .map(|pattern| pattern.scope)
                .unwrap_or(crate::lesson::ContextSelector {
                    repository: None,
                    required_markers: Vec::new(),
                    tags: Vec::new(),
                    os: None,
                    arch: None,
                });
            result.push(KnowledgeCandidateRef {
                reference: knowledge.id.to_string(),
                abstract_parent: Some(knowledge.id.clone()),
                level: ResolutionLevel::Abstract,
                statement: knowledge.statement.clone(),
                scope: scope.clone(),
                applicability: knowledge.applicability.clone(),
                boundary: knowledge.generalization_boundary.clone(),
                maturity: knowledge.maturity,
                quality: crate::experimentation::ExperimentQuality::Controlled,
                updated_at: knowledge.updated_at,
            });
            for exception in self.knowledge_exceptions_for(&knowledge.id)? {
                result.push(KnowledgeCandidateRef {
                    reference: exception.id.to_string(),
                    abstract_parent: Some(knowledge.id.clone()),
                    level: ResolutionLevel::Exception,
                    statement: format!(
                        "Exception to {}: {:?}",
                        knowledge.statement, exception.reason
                    ),
                    scope: exception.context,
                    applicability: exception.applicability,
                    boundary: GeneralizationBoundary::default(),
                    maturity: KnowledgeMaturity::Validated,
                    quality: crate::experimentation::ExperimentQuality::Controlled,
                    updated_at: exception.created_at,
                });
            }
            for specialization in self.knowledge_specializations_for(&knowledge.id)? {
                let child = self.abstract_knowledge(&specialization.child.id)?;
                result.push(KnowledgeCandidateRef {
                    reference: child.id.to_string(),
                    abstract_parent: Some(knowledge.id.clone()),
                    level: ResolutionLevel::Specialization,
                    statement: child.statement,
                    scope: scope.clone(),
                    applicability: specialization.additional_scope,
                    boundary: child.generalization_boundary,
                    maturity: child.maturity,
                    quality: crate::experimentation::ExperimentQuality::Controlled,
                    updated_at: child.updated_at,
                });
            }
            for artifact in &knowledge.provenance.source_artifacts {
                let represented = representations.iter().any(|record| {
                    record.artifact == *artifact
                        && matches!(
                            &record.state,
                            KnowledgeRepresentationState::RepresentedByAbstract(id)
                                if id == &knowledge.id
                        )
                });
                if !represented {
                    result.push(KnowledgeCandidateRef {
                        reference: artifact.id.clone(),
                        abstract_parent: Some(knowledge.id.clone()),
                        level: ResolutionLevel::Specific,
                        statement: format!(
                            "Specific supporting {:?} {}",
                            artifact.kind, artifact.id
                        ),
                        scope: scope.clone(),
                        applicability: knowledge.applicability.clone(),
                        boundary: knowledge.generalization_boundary.clone(),
                        maturity: KnowledgeMaturity::Supported,
                        quality: crate::experimentation::ExperimentQuality::Controlled,
                        updated_at: knowledge.updated_at,
                    });
                }
            }
        }
        result.sort_by_key(|item| item.reference.clone());
        Ok(result)
    }

    pub fn abstraction_development_summary(&self) -> Result<AbstractionDevelopmentSummary> {
        let patterns = self.experience_patterns()?;
        let knowledge = self.abstract_knowledge_items()?;
        let representations = self.knowledge_representations()?;
        Ok(AbstractionDevelopmentSummary {
            candidate_patterns: patterns
                .iter()
                .filter(|item| {
                    matches!(
                        item.status,
                        ExperiencePatternStatus::Candidate
                            | ExperiencePatternStatus::TransferTestable
                    )
                })
                .count(),
            validated_abstractions: knowledge
                .iter()
                .filter(|item| item.maturity == KnowledgeMaturity::Validated)
                .count(),
            active_specializations: knowledge
                .iter()
                .map(|item| item.specializations.len())
                .sum(),
            exceptions: knowledge.iter().map(|item| item.exceptions.len()).sum(),
            negative_transfer_events: self.negative_transfer_events()?.len(),
            unknown_boundaries: knowledge
                .iter()
                .map(|item| item.generalization_boundary.unknown.len())
                .sum(),
            represented_specific_artifacts: representations
                .iter()
                .filter(|item| {
                    matches!(
                        item.state,
                        KnowledgeRepresentationState::RepresentedByAbstract(_)
                    )
                })
                .count(),
        })
    }
}
