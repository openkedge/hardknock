// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Error, Result,
    core::{CommitReceiptId, EffectGroupId, EffectId, EffectLedgerId, EffectPlanId, RealityId},
    effects::*,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn label<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidInput("Expected string enum representation".into()))
}

pub trait EffectStore {
    fn effect(&self, id: &EffectId) -> Result<Effect>;
    fn effects(&self, reality: Option<&RealityId>) -> Result<Vec<Effect>>;
    fn effect_events(&self, id: &EffectId) -> Result<Vec<EffectEvent>>;
    fn prepared_effect(&self, id: &EffectId) -> Result<PreparedEffect>;
    fn commit_receipt_for_effect(&self, id: &EffectId) -> Result<Option<CommitReceipt>>;
}

impl EffectStore for Store {
    fn effect(&self, id: &EffectId) -> Result<Effect> {
        self.effect_json(
            "SELECT data FROM effects WHERE id=?1",
            &id.to_string(),
            "Effect",
        )
    }
    fn effects(&self, reality: Option<&RealityId>) -> Result<Vec<Effect>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM effects WHERE (?1 IS NULL OR reality_id=?1) ORDER BY created_at,id",
        )?;
        let reality = reality.map(ToString::to_string);
        statement
            .query_map([reality], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    fn effect_events(&self, id: &EffectId) -> Result<Vec<EffectEvent>> {
        let mut statement = self
            .connection
            .prepare("SELECT data FROM effect_events WHERE effect_id=?1 ORDER BY sequence")?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    fn prepared_effect(&self, id: &EffectId) -> Result<PreparedEffect> {
        self.effect_json(
            "SELECT data FROM prepared_effects WHERE effect_id=?1",
            &id.to_string(),
            "Prepared effect",
        )
    }
    fn commit_receipt_for_effect(&self, id: &EffectId) -> Result<Option<CommitReceipt>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM commit_receipts WHERE effect_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| serde_json::from_str(&data).map_err(Into::into))
            .transpose()
    }
}

impl Store {
    fn effect_json<T: DeserializeOwned>(&self, sql: &str, id: &str, kind: &str) -> Result<T> {
        let data: Option<String> = self
            .connection
            .query_row(sql, [id], |row| row.get(0))
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("{kind} {id} not found"))
        })?)?)
    }
    pub fn ensure_effect_ledger(&self, reality_id: Option<&RealityId>) -> Result<EffectLedger> {
        if let Some(reality_id) = reality_id {
            let mut reality = self.reality(reality_id)?;
            if let Some(id) = &reality.effect_ledger {
                return self.effect_json(
                    "SELECT data FROM effect_ledgers WHERE id=?1",
                    &id.to_string(),
                    "Effect ledger",
                );
            }
            let ledger = EffectLedger {
                id: EffectLedgerId::new(),
                reality_id: Some(reality_id.clone()),
                created_at: Utc::now(),
            };
            let transaction = rusqlite::Transaction::new_unchecked(
                &self.connection,
                TransactionBehavior::Immediate,
            )?;
            transaction.execute(
                "INSERT INTO effect_ledgers(id,reality_id,created_at,data) VALUES(?1,?2,?3,?4)",
                params![
                    ledger.id.to_string(),
                    reality_id.to_string(),
                    ledger.created_at.to_rfc3339(),
                    serde_json::to_string(&ledger)?
                ],
            )?;
            reality.effect_ledger = Some(ledger.id.clone());
            let changed = transaction.execute(
                "UPDATE realities SET data=?2 WHERE id=?1",
                params![reality_id.to_string(), serde_json::to_string(&reality)?],
            )?;
            if changed != 1 {
                return Err(Error::NotFound(format!("Reality {reality_id} not found")));
            }
            transaction.commit()?;
            return Ok(ledger);
        }
        let ledger = EffectLedger {
            id: EffectLedgerId::new(),
            reality_id: None,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO effect_ledgers(id,reality_id,created_at,data) VALUES(?1,NULL,?2,?3)",
            params![
                ledger.id.to_string(),
                ledger.created_at.to_rfc3339(),
                serde_json::to_string(&ledger)?
            ],
        )?;
        Ok(ledger)
    }
    pub fn insert_effect(&self, effect: &Effect) -> Result<()> {
        if effect.lifecycle != EffectLifecycle::Proposed || effect.classification.is_some() {
            return Err(Error::InvalidInput(
                "New Effect must begin in PROPOSED without classification".into(),
            ));
        }
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO effects(id,ledger_id,reality_id,session_id,adapter,lifecycle,scope_hash,created_at,data)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                effect.id.to_string(),
                effect.ledger_id.to_string(),
                effect.reality_id.as_ref().map(ToString::to_string),
                effect.session_id,
                effect.adapter,
                label(&effect.lifecycle)?,
                effect.scope_hash()?,
                effect.created_at.to_rfc3339(),
                serde_json::to_string(effect)?
            ],
        )?;
        Self::insert_effect_event(
            &transaction,
            effect,
            EffectEventType::Proposed,
            &effect.evidence,
            json!({"scope_hash":effect.scope_hash()?}),
        )?;
        transaction.commit()?;
        Ok(())
    }
    fn next_event_sequence(transaction: &Transaction<'_>, effect_id: &EffectId) -> Result<u64> {
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM effect_events WHERE effect_id=?1",
            [effect_id.to_string()],
            |row| row.get(0),
        )?;
        u64::try_from(sequence)
            .map_err(|_| Error::InvalidInput("Effect event sequence overflow".into()))
    }
    fn insert_effect_event(
        transaction: &Transaction<'_>,
        effect: &Effect,
        event_type: EffectEventType,
        evidence: &[String],
        metadata: Value,
    ) -> Result<EffectEvent> {
        let event = EffectEvent {
            id: crate::core::EffectEventId::new(),
            effect_id: effect.id.clone(),
            sequence: Self::next_event_sequence(transaction, &effect.id)?,
            event_type,
            timestamp: Utc::now(),
            evidence: evidence.to_vec(),
            metadata,
        };
        transaction.execute(
            "INSERT INTO effect_events(id,effect_id,sequence,event_type,timestamp,data)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                event.id.to_string(),
                event.effect_id.to_string(),
                i64::try_from(event.sequence)
                    .map_err(|_| Error::InvalidInput("Effect event sequence overflow".into()))?,
                label(&event.event_type)?,
                event.timestamp.to_rfc3339(),
                serde_json::to_string(&event)?
            ],
        )?;
        Ok(event)
    }
    pub fn transition_effect(
        &self,
        effect: &mut Effect,
        next: EffectLifecycle,
        event_type: EffectEventType,
        metadata: Value,
    ) -> Result<EffectEvent> {
        if !effect.lifecycle.allows(next) {
            return Err(Error::InvalidInput(format!(
                "Invalid effect lifecycle transition {:?} → {:?}",
                effect.lifecycle, next
            )));
        }
        let previous = effect.lifecycle;
        effect.lifecycle = next;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE effects SET lifecycle=?3,data=?4 WHERE id=?1 AND lifecycle=?2",
            params![
                effect.id.to_string(),
                label(&previous)?,
                label(&next)?,
                serde_json::to_string(effect)?
            ],
        )?;
        if changed != 1 {
            effect.lifecycle = previous;
            return Err(Error::Intervention(
                "Effect changed concurrently; reload before transition".into(),
            ));
        }
        let event = Self::insert_effect_event(
            &transaction,
            effect,
            event_type,
            &effect.evidence,
            metadata,
        )?;
        transaction.commit()?;
        Ok(event)
    }
    pub fn append_effect_event(
        &self,
        effect: &Effect,
        event_type: EffectEventType,
        metadata: Value,
    ) -> Result<EffectEvent> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let event = Self::insert_effect_event(
            &transaction,
            effect,
            event_type,
            &effect.evidence,
            metadata,
        )?;
        transaction.commit()?;
        Ok(event)
    }
    pub fn save_prepared_effect(
        &self,
        effect: &mut Effect,
        prepared: &PreparedEffect,
    ) -> Result<()> {
        if !effect.lifecycle.allows(EffectLifecycle::Prepared) {
            return Err(Error::InvalidInput(
                "Effect is not eligible for preparation".into(),
            ));
        }
        let previous = effect.lifecycle;
        effect.lifecycle = EffectLifecycle::Prepared;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO prepared_effects(id,effect_id,expires_at,data) VALUES(?1,?2,?3,?4)",
            params![
                prepared.id.to_string(),
                prepared.effect_id.to_string(),
                prepared.expires_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(prepared)?
            ],
        )?;
        Self::insert_snapshot(&transaction, effect, "prepare", &prepared.before)?;
        let changed = transaction.execute(
            "UPDATE effects SET lifecycle=?3,data=?4 WHERE id=?1 AND lifecycle=?2",
            params![
                effect.id.to_string(),
                label(&previous)?,
                label(&effect.lifecycle)?,
                serde_json::to_string(effect)?
            ],
        )?;
        if changed != 1 {
            effect.lifecycle = previous;
            return Err(Error::Intervention(
                "Effect changed concurrently during prepare".into(),
            ));
        }
        Self::insert_effect_event(
            &transaction,
            effect,
            EffectEventType::Prepared,
            &prepared.evidence,
            json!({
                "prepared_id":prepared.id,
                "preview":prepared.preview,
                "expires_at":prepared.expires_at,
                "authoritative_mutation":false
            }),
        )?;
        transaction.commit()?;
        Ok(())
    }
    fn insert_snapshot(
        transaction: &Transaction<'_>,
        effect: &Effect,
        phase: &str,
        snapshot: &ExternalStateSnapshot,
    ) -> Result<()> {
        transaction.execute(
            "INSERT INTO external_state_snapshots(id,effect_id,phase,captured_at,data)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                snapshot.id.to_string(),
                effect.id.to_string(),
                phase,
                snapshot.captured_at.to_rfc3339(),
                serde_json::to_string(snapshot)?
            ],
        )?;
        Ok(())
    }
    pub fn insert_external_snapshot(
        &self,
        effect: &Effect,
        phase: &str,
        snapshot: &ExternalStateSnapshot,
    ) -> Result<()> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        Self::insert_snapshot(&transaction, effect, phase, snapshot)?;
        transaction.commit()?;
        Ok(())
    }
    pub fn insert_commit_authorization(&self, authorization: &CommitAuthorization) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO commit_authorizations(id,scope_hash,granted_at,expires_at,data)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                authorization.id.to_string(),
                authorization.scope_hash,
                authorization.granted_at.to_rfc3339(),
                authorization.expires_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(authorization)?
            ],
        )?;
        Ok(())
    }
    pub fn save_commit_receipt(
        &self,
        effect: &mut Effect,
        receipt: &CommitReceipt,
        snapshot: &ExternalStateSnapshot,
    ) -> Result<()> {
        if !effect.lifecycle.allows(EffectLifecycle::Committed) {
            return Err(Error::InvalidInput(
                "Effect cannot transition to committed".into(),
            ));
        }
        let previous = effect.lifecycle;
        effect.lifecycle = EffectLifecycle::Committed;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO commit_receipts(id,effect_id,committed_at,data) VALUES(?1,?2,?3,?4)",
            params![receipt.id.to_string(),effect.id.to_string(),receipt.committed_at.to_rfc3339(),serde_json::to_string(receipt)?],
        )?;
        Self::insert_snapshot(&transaction, effect, "post_commit", snapshot)?;
        let changed = transaction.execute(
            "UPDATE effects SET lifecycle=?3,data=?4 WHERE id=?1 AND lifecycle=?2",
            params![
                effect.id.to_string(),
                label(&previous)?,
                label(&effect.lifecycle)?,
                serde_json::to_string(effect)?
            ],
        )?;
        if changed != 1 {
            effect.lifecycle = previous;
            return Err(Error::Intervention(
                "Effect changed concurrently during commit".into(),
            ));
        }
        Self::insert_effect_event(
            &transaction,
            effect,
            EffectEventType::Committed,
            &effect.evidence,
            json!({"receipt_id":receipt.id,"external_reference":receipt.external_reference}),
        )?;
        transaction.commit()?;
        Ok(())
    }
    pub fn save_compensation_receipt(
        &self,
        effect: &mut Effect,
        receipt: &CompensationReceipt,
    ) -> Result<()> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO compensation_receipts(id,original_receipt,compensated_at,data) VALUES(?1,?2,?3,?4)",params![receipt.id.to_string(),receipt.original_receipt.to_string(),receipt.compensated_at.to_rfc3339(),serde_json::to_string(receipt)?])?;
        if receipt.status == CompensationStatus::Successful {
            if !effect.lifecycle.allows(EffectLifecycle::Compensated) {
                return Err(Error::InvalidInput(
                    "Effect cannot be compensated from its current state".into(),
                ));
            }
            let previous = effect.lifecycle;
            effect.lifecycle = EffectLifecycle::Compensated;
            let changed = transaction.execute(
                "UPDATE effects SET lifecycle=?3,data=?4 WHERE id=?1 AND lifecycle=?2",
                params![
                    effect.id.to_string(),
                    label(&previous)?,
                    label(&effect.lifecycle)?,
                    serde_json::to_string(effect)?
                ],
            )?;
            if changed != 1 {
                effect.lifecycle = previous;
                return Err(Error::Intervention(
                    "Effect changed concurrently during compensation".into(),
                ));
            }
            Self::insert_effect_event(
                &transaction,
                effect,
                EffectEventType::Compensated,
                &effect.evidence,
                json!({"compensation_receipt":receipt.id}),
            )?;
        } else {
            Self::insert_effect_event(
                &transaction,
                effect,
                EffectEventType::CompensationFailed,
                &effect.evidence,
                json!({"compensation_receipt":receipt.id,"status":receipt.status}),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn commit_receipt(&self, id: &CommitReceiptId) -> Result<CommitReceipt> {
        self.effect_json(
            "SELECT data FROM commit_receipts WHERE id=?1",
            &id.to_string(),
            "Commit receipt",
        )
    }
    pub fn insert_reconciliation_attempt(&self, attempt: &ReconciliationAttempt) -> Result<()> {
        self.connection.execute("INSERT INTO reconciliation_attempts(id,effect_id,attempted_at,result,data) VALUES(?1,?2,?3,?4,?5)",params![attempt.id.to_string(),attempt.effect_id.to_string(),attempt.attempted_at.to_rfc3339(),match &attempt.result {ReconciliationResult::Committed{..}=>"committed",ReconciliationResult::NotCommitted=>"not_committed",ReconciliationResult::StillUnknown{..}=>"still_unknown"},serde_json::to_string(attempt)?])?;
        Ok(())
    }
    pub fn link_effect_experience(
        &self,
        effect_id: &EffectId,
        experience_id: &crate::core::ExperienceId,
        relation: &str,
    ) -> Result<()> {
        if relation.is_empty()
            || relation.len() > 64
            || !relation
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err(Error::InvalidInput(
                "Effect Experience relation must be bounded snake_case".into(),
            ));
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO effect_experience_links(effect_id,experience_id,relation,created_at)
             VALUES(?1,?2,?3,?4)",
            params![
                effect_id.to_string(),
                experience_id.to_string(),
                relation,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }
    pub fn effect_experience_links(
        &self,
        effect_id: &EffectId,
        relation: Option<&str>,
    ) -> Result<Vec<crate::core::ExperienceId>> {
        let mut statement = self.connection.prepare(
            "SELECT experience_id FROM effect_experience_links
             WHERE effect_id=?1 AND (?2 IS NULL OR relation=?2) ORDER BY created_at,experience_id",
        )?;
        statement
            .query_map(params![effect_id.to_string(), relation], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| row?.parse())
            .collect()
    }
    pub fn insert_effect_plan(&self, plan: &EffectPlan) -> Result<()> {
        self.connection.execute(
            "INSERT INTO effect_plans(id,created_at,data) VALUES(?1,?2,?3)",
            params![
                plan.id.to_string(),
                plan.created_at.to_rfc3339(),
                serde_json::to_string(plan)?
            ],
        )?;
        Ok(())
    }
    pub fn insert_effect_group(&self, result: &EffectGroupResult) -> Result<()> {
        self.connection.execute(
            "INSERT INTO effect_groups(id,plan_id,created_at,outcome,data) VALUES(?1,?2,?3,?4,?5)",
            params![
                result.id.to_string(),
                result.plan_id.to_string(),
                result.created_at.to_rfc3339(),
                label(&result.outcome)?,
                serde_json::to_string(result)?
            ],
        )?;
        Ok(())
    }
    pub fn effect_group(&self, id: &EffectGroupId) -> Result<EffectGroupResult> {
        self.effect_json(
            "SELECT data FROM effect_groups WHERE id=?1",
            &id.to_string(),
            "Effect group",
        )
    }
    pub fn effect_plan(&self, id: &EffectPlanId) -> Result<EffectPlan> {
        self.effect_json(
            "SELECT data FROM effect_plans WHERE id=?1",
            &id.to_string(),
            "Effect plan",
        )
    }
    pub fn orphaned_prepared_effects(&self) -> Result<Vec<Effect>> {
        let mut statement=self.connection.prepare("SELECT e.data FROM effects e LEFT JOIN realities r ON r.id=e.reality_id WHERE e.lifecycle='prepared' AND e.reality_id IS NOT NULL AND (r.id IS NULL OR json_extract(r.data,'$.status')='discarded') ORDER BY e.created_at,e.id")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    pub fn detach_prepared_effect(&self, id: &EffectId) -> Result<Effect> {
        let mut effect = self.effect(id)?;
        if effect.lifecycle != EffectLifecycle::Prepared {
            return Err(Error::InvalidInput(
                "Only a selected PREPARED effect can leave an experimental Reality".into(),
            ));
        }
        let ledger = EffectLedger {
            id: EffectLedgerId::new(),
            reality_id: None,
            created_at: Utc::now(),
        };
        effect.ledger_id = ledger.id.clone();
        effect.reality_id = None;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO effect_ledgers(id,reality_id,created_at,data) VALUES(?1,NULL,?2,?3)",
            params![
                ledger.id.to_string(),
                ledger.created_at.to_rfc3339(),
                serde_json::to_string(&ledger)?
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE effects SET ledger_id=?2,reality_id=NULL,data=?3 WHERE id=?1 AND lifecycle='prepared'",
            params![
                effect.id.to_string(),
                effect.ledger_id.to_string(),
                serde_json::to_string(&effect)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention(
                "Effect changed before experiment selection".into(),
            ));
        }
        Self::insert_effect_event(
            &transaction,
            &effect,
            EffectEventType::Virtualized,
            &effect.evidence,
            json!({"selected_by_experiment":true,"commit_still_required":true}),
        )?;
        transaction.commit()?;
        Ok(effect)
    }
    pub fn insert_effect_benchmark(&self, run: &EffectBenchmarkRun) -> Result<()> {
        self.connection.execute(
            "INSERT INTO effect_benchmark_runs(id,created_at,data) VALUES(?1,?2,?3)",
            params![
                run.id.to_string(),
                run.created_at.to_rfc3339(),
                serde_json::to_string(run)?
            ],
        )?;
        Ok(())
    }
    pub fn latest_effect_benchmark(&self) -> Result<Option<EffectBenchmarkRun>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM effect_benchmark_runs ORDER BY created_at DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }
}
