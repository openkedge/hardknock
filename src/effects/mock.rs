// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Error, Result, core::PreparedEffectId};
use chrono::{Duration, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockExternalResource {
    pub adapter: String,
    pub target: String,
    pub version: u64,
    pub state: Value,
    pub mutation_count: u64,
}

#[derive(Clone)]
pub struct MockExternalSystem {
    db: PathBuf,
}
impl MockExternalSystem {
    pub fn new(home: &Path) -> Result<Self> {
        let directory = home.join("effects");
        fs::create_dir_all(&directory)?;
        if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Effect fixture directory must not be a symlink".into(),
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let db = directory.join("mock-external.db");
        if fs::symlink_metadata(&db).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::Intervention(
                "Mock external database must not be a symlink".into(),
            ));
        }
        let system = Self { db };
        let connection = system.connection()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS resources(
                adapter TEXT NOT NULL,
                target TEXT NOT NULL,
                version INTEGER NOT NULL CHECK(version >= 0),
                state TEXT NOT NULL CHECK(json_valid(state)),
                mutation_count INTEGER NOT NULL CHECK(mutation_count >= 0),
                PRIMARY KEY(adapter,target)
            );
            CREATE TABLE IF NOT EXISTS preparations(
                token TEXT PRIMARY KEY NOT NULL,
                effect_id TEXT NOT NULL UNIQUE,
                adapter TEXT NOT NULL,
                target TEXT NOT NULL,
                expected_version INTEGER NOT NULL,
                before_state TEXT NOT NULL CHECK(json_valid(before_state)),
                desired_state TEXT NOT NULL CHECK(json_valid(desired_state)),
                expires_at TEXT,
                status TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS applied_effects(
                idempotency_key TEXT PRIMARY KEY NOT NULL,
                effect_id TEXT NOT NULL UNIQUE,
                adapter TEXT NOT NULL,
                target TEXT NOT NULL,
                receipt TEXT NOT NULL CHECK(json_valid(receipt))
            );",
        )?;
        fs::set_permissions(&system.db, fs::Permissions::from_mode(0o600))?;
        Ok(system)
    }
    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.db)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        Ok(connection)
    }
    pub fn seed(&self, adapter: &str, target: &str, state: &Value) -> Result<()> {
        self.connection()?.execute(
            "INSERT OR IGNORE INTO resources(adapter,target,version,state,mutation_count)
             VALUES(?1,?2,1,?3,0)",
            params![adapter, target, serde_json::to_string(state)?],
        )?;
        Ok(())
    }
    pub fn mutate_outside(&self, adapter: &str, target: &str, state: &Value) -> Result<()> {
        self.seed(adapter, target, state)?;
        self.connection()?.execute(
            "UPDATE resources SET version=version+1,state=?3,mutation_count=mutation_count+1
             WHERE adapter=?1 AND target=?2",
            params![adapter, target, serde_json::to_string(state)?],
        )?;
        Ok(())
    }
    pub fn resource(&self, adapter: &str, target: &str) -> Result<MockExternalResource> {
        let row: Option<(i64, String, i64)> = self
            .connection()?
            .query_row(
                "SELECT version,state,mutation_count FROM resources WHERE adapter=?1 AND target=?2",
                params![adapter, target],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (version, state, mutation_count) = row.ok_or_else(|| {
            Error::NotFound(format!(
                "Mock external resource {adapter}:{target} not found"
            ))
        })?;
        Ok(MockExternalResource {
            adapter: adapter.into(),
            target: target.into(),
            version: u64::try_from(version)
                .map_err(|_| Error::InvalidInput("Invalid external version".into()))?,
            state: serde_json::from_str(&state)?,
            mutation_count: u64::try_from(mutation_count)
                .map_err(|_| Error::InvalidInput("Invalid mutation count".into()))?,
        })
    }
    pub fn prepared_count(&self, adapter: &str, target: &str) -> Result<u64> {
        let count: i64 = self.connection()?.query_row(
            "SELECT COUNT(*) FROM preparations WHERE adapter=?1 AND target=?2 AND status='prepared'",
            params![adapter, target],
            |row| row.get(0),
        )?;
        u64::try_from(count).map_err(|_| Error::InvalidInput("Invalid prepared count".into()))
    }
}

#[derive(Clone, Copy)]
enum Flavor {
    Http,
    Database,
    Message,
    Shadow,
}
impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Self::Http => "mock-http",
            Self::Database => "mock-db",
            Self::Message => "mock-message",
            Self::Shadow => "shadow-deployment",
        }
    }
    fn schemes(self) -> &'static [&'static str] {
        match self {
            Self::Http => &["mock", "mock-http"],
            Self::Database => &["mock-db"],
            Self::Message => &["mock-message"],
            Self::Shadow => &["shadow"],
        }
    }
    fn classification(self) -> EffectClassification {
        match self {
            Self::Http => EffectClassification {
                reversibility: ReversibilityClass::Compensatable,
                idempotency: IdempotencyClass::IdempotentWithKey,
                isolation_requirement: IsolationRequirement::Staged,
                externality: ExternalityClass::ExternalSystem,
                risk: EffectRisk::Medium,
                commit_strategy: CommitStrategy::Compensating,
            },
            Self::Database => EffectClassification {
                reversibility: ReversibilityClass::Compensatable,
                idempotency: IdempotencyClass::IdempotentWithKey,
                isolation_requirement: IsolationRequirement::ProviderTransaction,
                externality: ExternalityClass::ExternalSystem,
                risk: EffectRisk::Medium,
                commit_strategy: CommitStrategy::Compensating,
            },
            Self::Message => EffectClassification {
                reversibility: ReversibilityClass::Deferrable,
                idempotency: IdempotencyClass::IdempotentWithKey,
                isolation_requirement: IsolationRequirement::Staged,
                externality: ExternalityClass::HumanVisible,
                risk: EffectRisk::High,
                commit_strategy: CommitStrategy::DeferredDispatch,
            },
            Self::Shadow => EffectClassification {
                reversibility: ReversibilityClass::Shadowable,
                idempotency: IdempotencyClass::IdempotentWithKey,
                isolation_requirement: IsolationRequirement::Shadow,
                externality: ExternalityClass::ExternalSystem,
                risk: EffectRisk::Medium,
                commit_strategy: CommitStrategy::ShadowPromote,
            },
        }
    }
}

#[derive(Clone)]
struct DeterministicAdapter {
    flavor: Flavor,
    system: MockExternalSystem,
}
impl DeterministicAdapter {
    fn new(home: &Path, flavor: Flavor) -> Result<Self> {
        Ok(Self {
            flavor,
            system: MockExternalSystem::new(home)?,
        })
    }
    fn capabilities(&self) -> EffectAdapterCapabilities {
        EffectAdapterCapabilities {
            simulation: true,
            prepare: true,
            commit: true,
            discard: true,
            compensate: !matches!(self.flavor, Flavor::Message),
            reconciliation: true,
            idempotency_keys: true,
            shadow_resources: matches!(self.flavor, Flavor::Shadow),
        }
    }
    fn classify(&self, request: &EffectRequest) -> Result<EffectClassification> {
        if !self
            .flavor
            .schemes()
            .contains(&request.target.scheme().unwrap_or_default())
        {
            return Err(Error::InvalidInput(format!(
                "Adapter {} does not support target {}",
                self.flavor.name(),
                request.target.uri
            )));
        }
        if matches!(self.flavor, Flavor::Database)
            && request
                .payload
                .pointer("/balance")
                .and_then(Value::as_i64)
                .is_some_and(|balance| balance < 0)
        {
            return Err(Error::Intervention(
                "Mock database invariant rejected a negative balance".into(),
            ));
        }
        Ok(self.flavor.classification())
    }
    fn observe(&self, effect: &Effect) -> Result<ExternalStateSnapshot> {
        self.system
            .seed(self.flavor.name(), &effect.target.uri, &json!({}))?;
        let resource = self
            .system
            .resource(self.flavor.name(), &effect.target.uri)?;
        ExternalStateSnapshot::capture(
            effect.id.clone(),
            self.flavor.name(),
            effect.target.clone(),
            Some(resource.version.to_string()),
            resource.state,
        )
    }
    fn prepare(&self, effect: &Effect) -> Result<PreparedEffect> {
        if effect.fault == EffectFault::PrepareFailure {
            return Err(Error::Intervention("Injected prepare failure".into()));
        }
        let before = self.observe(effect)?;
        let id = PreparedEffectId::new();
        let token = format!("hk-preparation:{id}");
        let expires_at = Some(if effect.fault == EffectFault::ReservationExpiry {
            Utc::now() - Duration::seconds(1)
        } else {
            Utc::now() + Duration::minutes(5)
        });
        self.system.connection()?.execute(
            "INSERT INTO preparations(token,effect_id,adapter,target,expected_version,before_state,desired_state,expires_at,status)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'prepared')",
            params![
                token,
                effect.id.to_string(),
                self.flavor.name(),
                effect.target.uri,
                before.version.as_deref().unwrap_or("0").parse::<i64>().map_err(|_| Error::InvalidInput("Invalid snapshot version".into()))?,
                serde_json::to_string(&before.state)?,
                serde_json::to_string(&effect.payload)?,
                expires_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        Ok(PreparedEffect {
            id,
            effect_id: effect.id.clone(),
            adapter: self.flavor.name().into(),
            preparation_token: token,
            expires_at,
            preview: EffectPreview {
                summary: format!(
                    "{} {} through {} (authoritative state unchanged)",
                    serde_json::to_string(&effect.operation)?,
                    effect.target.uri,
                    self.flavor.name()
                ),
                current: before.state.clone(),
                prepared: effect.payload.clone(),
            },
            before,
            scope_hash: effect.scope_hash()?,
            evidence: effect.evidence.clone(),
        })
    }
    fn existing_receipt(&self, effect: &Effect) -> Result<Option<CommitReceipt>> {
        let receipt: Option<String> = self
            .system
            .connection()?
            .query_row(
                "SELECT receipt FROM applied_effects WHERE idempotency_key=?1",
                [&effect.idempotency_key],
                |row| row.get(0),
            )
            .optional()?;
        receipt
            .map(|receipt| serde_json::from_str(&receipt).map_err(Into::into))
            .transpose()
    }
    fn commit(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<AdapterCommitOutcome> {
        if let Some(receipt) = self.existing_receipt(effect)? {
            return Ok(AdapterCommitOutcome::Committed { receipt });
        }
        if effect.fault == EffectFault::CommitFailureBeforeMutation {
            return Ok(AdapterCommitOutcome::NotCommitted {
                reason: "Injected commit failure before mutation".into(),
            });
        }
        if prepared.expired(Utc::now()) {
            return Ok(AdapterCommitOutcome::NotCommitted {
                reason: "Prepared effect expired".into(),
            });
        }
        let mut connection = self.system.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (version, before_state): (i64, String) = transaction.query_row(
            "SELECT version,state FROM resources WHERE adapter=?1 AND target=?2",
            params![self.flavor.name(), effect.target.uri],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let expected = prepared
            .before
            .version
            .as_deref()
            .unwrap_or("0")
            .parse::<i64>()
            .map_err(|_| Error::InvalidInput("Invalid prepared version".into()))?;
        if version != expected {
            return Ok(AdapterCommitOutcome::NotCommitted {
                reason: format!(
                    "Stale prepared effect: expected version {expected}, observed {version}"
                ),
            });
        }
        let result_hash = blake3::hash(&serde_json::to_vec(&effect.payload)?)
            .to_hex()
            .to_string();
        let receipt = CommitReceipt {
            id: crate::core::CommitReceiptId::new(),
            effect_id: effect.id.clone(),
            adapter: self.flavor.name().into(),
            committed_at: Utc::now(),
            external_reference: Some(format!("{}#version-{}", effect.target.uri, version + 1)),
            idempotency_key: Some(effect.idempotency_key.clone()),
            result_hash: Some(result_hash),
            metadata: json!({
                "before": serde_json::from_str::<Value>(&before_state)?,
                "after": effect.payload,
                "version_before": version,
                "version_after": version + 1,
                "strategy": effect.classification.as_ref().map(|classification| classification.commit_strategy),
            }),
        };
        transaction.execute(
            "UPDATE resources SET version=version+1,state=?3,mutation_count=mutation_count+1
             WHERE adapter=?1 AND target=?2",
            params![
                self.flavor.name(),
                effect.target.uri,
                serde_json::to_string(&effect.payload)?
            ],
        )?;
        transaction.execute(
            "UPDATE preparations SET status='committed' WHERE token=?1",
            [&prepared.preparation_token],
        )?;
        transaction.execute(
            "INSERT INTO applied_effects(idempotency_key,effect_id,adapter,target,receipt)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                effect.idempotency_key,
                effect.id.to_string(),
                self.flavor.name(),
                effect.target.uri,
                serde_json::to_string(&receipt)?
            ],
        )?;
        transaction.commit()?;
        if matches!(
            effect.fault,
            EffectFault::ResponseLossAfterMutation
                | EffectFault::ResponseLossWithReconciliationFailure
        ) {
            Ok(AdapterCommitOutcome::Unknown {
                reason: "Commit applied but response was lost".into(),
            })
        } else {
            Ok(AdapterCommitOutcome::Committed { receipt })
        }
    }
    fn discard(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<()> {
        if effect.fault == EffectFault::DiscardFailure {
            return Err(Error::Intervention("Injected discard failure".into()));
        }
        self.system.connection()?.execute(
            "UPDATE preparations SET status='discarded' WHERE token=?1 AND status='prepared'",
            [&prepared.preparation_token],
        )?;
        Ok(())
    }
    fn compensate(&self, effect: &Effect, receipt: &CommitReceipt) -> Result<CompensationReceipt> {
        if matches!(self.flavor, Flavor::Message) {
            return Ok(CompensationReceipt {
                id: crate::core::CompensationReceiptId::new(),
                original_receipt: receipt.id.clone(),
                compensated_at: Utc::now(),
                status: CompensationStatus::Unsupported,
                metadata: json!({"reason":"delivered messages cannot be rolled back"}),
            });
        }
        if effect.fault == EffectFault::CompensationFailure {
            return Ok(CompensationReceipt {
                id: crate::core::CompensationReceiptId::new(),
                original_receipt: receipt.id.clone(),
                compensated_at: Utc::now(),
                status: CompensationStatus::Failed,
                metadata: json!({"reason":"injected compensation failure"}),
            });
        }
        let before =
            receipt.metadata.get("before").cloned().ok_or_else(|| {
                Error::InvalidInput("Commit receipt lacks compensation state".into())
            })?;
        self.system.connection()?.execute(
            "UPDATE resources SET version=version+1,state=?3,mutation_count=mutation_count+1
             WHERE adapter=?1 AND target=?2",
            params![
                self.flavor.name(),
                effect.target.uri,
                serde_json::to_string(&before)?
            ],
        )?;
        Ok(CompensationReceipt {
            id: crate::core::CompensationReceiptId::new(),
            original_receipt: receipt.id.clone(),
            compensated_at: Utc::now(),
            status: CompensationStatus::Successful,
            metadata: json!({
                "restored_value": before,
                "terminology": "compensation is a new mutation, not rollback"
            }),
        })
    }
    fn reconcile(&self, effect: &Effect) -> Result<ReconciliationResult> {
        if matches!(
            effect.fault,
            EffectFault::ReconciliationFailure | EffectFault::ResponseLossWithReconciliationFailure
        ) {
            return Ok(ReconciliationResult::StillUnknown {
                reason: "Injected reconciliation failure".into(),
            });
        }
        Ok(match self.existing_receipt(effect)? {
            Some(receipt) => ReconciliationResult::Committed { receipt },
            None => ReconciliationResult::NotCommitted,
        })
    }
}

macro_rules! adapter {
    ($name:ident, $flavor:ident) => {
        #[derive(Clone)]
        pub struct $name(DeterministicAdapter);
        impl $name {
            pub fn new(home: &Path) -> Result<Self> {
                Ok(Self(DeterministicAdapter::new(home, Flavor::$flavor)?))
            }
            pub fn external_system(&self) -> MockExternalSystem {
                self.0.system.clone()
            }
        }
        impl EffectAdapter for $name {
            fn name(&self) -> &'static str {
                self.0.flavor.name()
            }
            fn schemes(&self) -> &'static [&'static str] {
                self.0.flavor.schemes()
            }
            fn capabilities(&self) -> EffectAdapterCapabilities {
                self.0.capabilities()
            }
            fn classify(&self, request: &EffectRequest) -> Result<EffectClassification> {
                self.0.classify(request)
            }
            fn observe(&self, effect: &Effect) -> Result<ExternalStateSnapshot> {
                self.0.observe(effect)
            }
            fn prepare(&self, effect: &Effect) -> Result<PreparedEffect> {
                self.0.prepare(effect)
            }
            fn commit(
                &self,
                effect: &Effect,
                prepared: &PreparedEffect,
            ) -> Result<AdapterCommitOutcome> {
                self.0.commit(effect, prepared)
            }
            fn discard(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<()> {
                self.0.discard(effect, prepared)
            }
            fn compensate(
                &self,
                effect: &Effect,
                receipt: &CommitReceipt,
            ) -> Result<CompensationReceipt> {
                self.0.compensate(effect, receipt)
            }
            fn reconcile(&self, effect: &Effect) -> Result<ReconciliationResult> {
                self.0.reconcile(effect)
            }
        }
    };
}

adapter!(MockHttpEffectAdapter, Http);
adapter!(MockDatabaseEffectAdapter, Database);
adapter!(MockMessageEffectAdapter, Message);
adapter!(ShadowDeploymentEffectAdapter, Shadow);
