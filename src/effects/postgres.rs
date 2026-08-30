// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Error, Result};
use ::postgres::{Client, NoTls, Transaction, types::ToSql};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path};

const CONFIG_FILE: &str = "postgres-targets.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresTargetConfig {
    pub alias: String,
    /// Host-plane connection string. It is read only from a private local file
    /// and is never placed in an Effect, Experience, token, or container.
    pub connection: String,
    #[serde(default = "default_schema")]
    pub schema: String,
    pub allowed_tables: Vec<String>,
    #[serde(default = "default_receipt_table")]
    pub receipt_table: String,
}

fn default_schema() -> String {
    "public".into()
}
fn default_receipt_table() -> String {
    "hardknock_effect_receipts".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostgresEffectOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresMutation {
    pub table: String,
    pub operation: PostgresEffectOperation,
    #[serde(default)]
    pub key: BTreeMap<String, Value>,
    pub expected_version: i64,
    #[serde(default)]
    pub changes: BTreeMap<String, Value>,
    #[serde(default = "default_version_column")]
    pub version_column: String,
    #[serde(default)]
    pub non_negative: Vec<String>,
}

fn default_version_column() -> String {
    "version".into()
}

#[derive(Clone)]
pub struct PostgresEffectAdapter {
    targets: BTreeMap<String, PostgresTargetConfig>,
}

impl PostgresEffectAdapter {
    pub fn from_home(home: &Path) -> Result<Option<Self>> {
        let path = home.join("effects").join(CONFIG_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 256 * 1024
        {
            return Err(Error::Intervention(
                "PostgreSQL target configuration must be a regular private (0600) file under HARDKNOCK_HOME/effects"
                    .into(),
            ));
        }
        let targets: Vec<PostgresTargetConfig> = serde_json::from_slice(&fs::read(path)?)?;
        Ok(Some(Self::new(targets)?))
    }

    pub fn new(targets: Vec<PostgresTargetConfig>) -> Result<Self> {
        if targets.is_empty() || targets.len() > 64 {
            return Err(Error::InvalidInput(
                "PostgreSQL adapter requires 1–64 scoped targets".into(),
            ));
        }
        let mut indexed = BTreeMap::new();
        for target in targets {
            validate_identifier(&target.alias)?;
            validate_identifier(&target.schema)?;
            validate_identifier(&target.receipt_table)?;
            if target.connection.trim().is_empty()
                || target.connection.len() > 4096
                || target.connection.contains(['\0', '\n', '\r'])
                || target.allowed_tables.is_empty()
            {
                return Err(Error::InvalidInput(
                    "PostgreSQL target connection and table scope must be bounded".into(),
                ));
            }
            for table in &target.allowed_tables {
                validate_identifier(table)?;
            }
            if indexed.insert(target.alias.clone(), target).is_some() {
                return Err(Error::InvalidInput(
                    "PostgreSQL target aliases must be unique".into(),
                ));
            }
        }
        Ok(Self { targets: indexed })
    }

    pub fn config_path(home: &Path) -> std::path::PathBuf {
        home.join("effects").join(CONFIG_FILE)
    }

    fn target<'a>(&'a self, effect: &Effect) -> Result<&'a PostgresTargetConfig> {
        let (alias, table) = target_parts(&effect.target.uri)?;
        let target = self.targets.get(alias).ok_or_else(|| {
            Error::Intervention("PostgreSQL target alias is not configured".into())
        })?;
        if !target.allowed_tables.iter().any(|allowed| allowed == table) {
            return Err(Error::Intervention(format!(
                "PostgreSQL table {table} is outside target {alias} scope"
            )));
        }
        Ok(target)
    }

    fn mutation(&self, effect: &Effect) -> Result<PostgresMutation> {
        let mutation: PostgresMutation = serde_json::from_value(effect.payload.clone())?;
        let operation_matches = matches!(
            (&mutation.operation, &effect.operation),
            (PostgresEffectOperation::Insert, EffectOperation::Create)
                | (PostgresEffectOperation::Update, EffectOperation::Update)
                | (PostgresEffectOperation::Delete, EffectOperation::Delete)
        );
        if !operation_matches {
            return Err(Error::Intervention(
                "Structured PostgreSQL operation does not match the Effect operation".into(),
            ));
        }
        validate_identifier(&mutation.table)?;
        validate_identifier(&mutation.version_column)?;
        for identifier in mutation
            .key
            .keys()
            .chain(mutation.changes.keys())
            .chain(&mutation.non_negative)
        {
            validate_identifier(identifier)?;
        }
        if mutation.expected_version < 0 || mutation.key.is_empty() {
            return Err(Error::InvalidInput(
                "PostgreSQL mutation requires a nonnegative expected version and a structured key"
                    .into(),
            ));
        }
        if mutation.key.contains_key(&mutation.version_column)
            || mutation.changes.contains_key(&mutation.version_column)
        {
            return Err(Error::InvalidInput(
                "Version column is controlled by the PostgreSQL adapter".into(),
            ));
        }
        let (_, target_table) = target_parts(&effect.target.uri)?;
        if mutation.table != target_table {
            return Err(Error::Intervention(
                "Structured mutation table does not match the scoped Effect target".into(),
            ));
        }
        let target = self.target(effect)?;
        if !target
            .allowed_tables
            .iter()
            .any(|table| table == &mutation.table)
        {
            return Err(Error::Intervention(
                "Structured mutation table is not allowed for this target".into(),
            ));
        }
        Ok(mutation)
    }

    fn connect(&self, effect: &Effect) -> Result<Client> {
        Client::connect(&self.target(effect)?.connection, NoTls).map_err(|error| {
            Error::Intervention(format!(
                "PostgreSQL adapter could not connect to its scoped target: {error}"
            ))
        })
    }

    fn observe_row(&self, effect: &Effect) -> Result<(Option<i64>, Value)> {
        let mutation = self.mutation(effect)?;
        let target = self.target(effect)?;
        let mut client = self.connect(effect)?;
        let (where_sql, values) = predicate(&mutation.key, 1)?;
        let references = references(&values);
        let sql = format!(
            "SELECT row_to_json(t)::text, {}::bigint FROM {}.{} AS t WHERE {}",
            quote(&mutation.version_column),
            quote(&target.schema),
            quote(&mutation.table),
            where_sql
        );
        let row = client.query_opt(&sql, &references).map_err(pg_error)?;
        match row {
            Some(row) => {
                let text: String = row.get(0);
                let version: i64 = row.get(1);
                Ok((Some(version), serde_json::from_str(&text)?))
            }
            None => Ok((None, Value::Null)),
        }
    }

    fn receipt(
        &self,
        transaction: &mut Transaction<'_>,
        target: &PostgresTargetConfig,
        idempotency_key: &str,
    ) -> Result<Option<CommitReceipt>> {
        let sql = format!(
            "SELECT receipt_json FROM {}.{} WHERE idempotency_key=$1",
            quote(&target.schema),
            quote(&target.receipt_table)
        );
        let row = transaction
            .query_opt(&sql, &[&idempotency_key])
            .map_err(pg_error)?;
        row.map(|row| {
            let data: String = row.get(0);
            serde_json::from_str(&data).map_err(Into::into)
        })
        .transpose()
    }

    fn commit_mutation(
        &self,
        transaction: &mut Transaction<'_>,
        target: &PostgresTargetConfig,
        mutation: &PostgresMutation,
    ) -> Result<Option<i64>> {
        match mutation.operation {
            PostgresEffectOperation::Update => {
                if mutation.changes.is_empty() {
                    return Err(Error::InvalidInput(
                        "PostgreSQL update requires at least one changed column".into(),
                    ));
                }
                let mut values = Vec::new();
                let mut assignments = Vec::new();
                for (column, value) in &mutation.changes {
                    values.push(pg_value(value)?);
                    assignments.push(format!("{}=${}", quote(column), values.len()));
                }
                let (where_sql, key_values) = predicate(&mutation.key, values.len() + 1)?;
                values.extend(key_values);
                values.push(Box::new(mutation.expected_version));
                let version_parameter = values.len();
                let sql = format!(
                    "UPDATE {}.{} SET {}, {}={}+1 WHERE {} AND {}=${} RETURNING {}::bigint",
                    quote(&target.schema),
                    quote(&mutation.table),
                    assignments.join(","),
                    quote(&mutation.version_column),
                    quote(&mutation.version_column),
                    where_sql,
                    quote(&mutation.version_column),
                    version_parameter,
                    quote(&mutation.version_column)
                );
                transaction
                    .query_opt(&sql, &references(&values))
                    .map_err(pg_error)
                    .map(|row| row.map(|row| row.get(0)))
            }
            PostgresEffectOperation::Delete => {
                if !mutation.changes.is_empty() {
                    return Err(Error::InvalidInput(
                        "PostgreSQL delete does not accept changes".into(),
                    ));
                }
                let (where_sql, mut values) = predicate(&mutation.key, 1)?;
                values.push(Box::new(mutation.expected_version));
                let version_parameter = values.len();
                let sql = format!(
                    "DELETE FROM {}.{} WHERE {} AND {}=${} RETURNING {}::bigint",
                    quote(&target.schema),
                    quote(&mutation.table),
                    where_sql,
                    quote(&mutation.version_column),
                    version_parameter,
                    quote(&mutation.version_column)
                );
                transaction
                    .query_opt(&sql, &references(&values))
                    .map_err(pg_error)
                    .map(|row| row.map(|row| row.get(0)))
            }
            PostgresEffectOperation::Insert => {
                if mutation.expected_version != 0 {
                    return Err(Error::InvalidInput(
                        "PostgreSQL insert uses expected_version 0".into(),
                    ));
                }
                let mut columns = Vec::new();
                let mut values = Vec::new();
                for (column, value) in mutation.key.iter().chain(&mutation.changes) {
                    if columns.iter().any(|existing| existing == column) {
                        return Err(Error::InvalidInput(
                            "PostgreSQL insert key and changes overlap".into(),
                        ));
                    }
                    columns.push(column.clone());
                    values.push(pg_value(value)?);
                }
                columns.push(mutation.version_column.clone());
                values.push(Box::new(1_i64));
                let parameters: Vec<_> = (1..=values.len()).map(|i| format!("${i}")).collect();
                let sql = format!(
                    "INSERT INTO {}.{}({}) VALUES({}) RETURNING {}::bigint",
                    quote(&target.schema),
                    quote(&mutation.table),
                    columns
                        .iter()
                        .map(|column| quote(column))
                        .collect::<Vec<_>>()
                        .join(","),
                    parameters.join(","),
                    quote(&mutation.version_column)
                );
                transaction
                    .query_opt(&sql, &references(&values))
                    .map_err(pg_error)
                    .map(|row| row.map(|row| row.get(0)))
            }
        }
    }
}

impl EffectAdapter for PostgresEffectAdapter {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn schemes(&self) -> &'static [&'static str] {
        &["postgres"]
    }

    fn capabilities(&self) -> EffectAdapterCapabilities {
        EffectAdapterCapabilities {
            simulation: true,
            prepare: true,
            commit: true,
            discard: true,
            compensate: false,
            reconciliation: true,
            idempotency_keys: true,
            shadow_resources: false,
        }
    }

    fn classify(&self, request: &EffectRequest) -> Result<EffectClassification> {
        if request.kind != EffectKind::Database {
            return Err(Error::Intervention(
                "PostgreSQL adapter accepts only database Effects".into(),
            ));
        }
        if !matches!(
            request.operation,
            EffectOperation::Create | EffectOperation::Update | EffectOperation::Delete
        ) {
            return Err(Error::Intervention(
                "PostgreSQL adapter accepts only structured insert/update/delete operations".into(),
            ));
        }
        // Validate target and payload before an Effect is accepted into the lifecycle.
        let probe = Effect::from_request(
            request.clone(),
            crate::core::EffectLedgerId::new(),
            self.name().into(),
        );
        self.mutation(&probe)?;
        Ok(EffectClassification {
            reversibility: ReversibilityClass::Irreversible,
            idempotency: IdempotencyClass::IdempotentWithKey,
            isolation_requirement: IsolationRequirement::ProviderTransaction,
            externality: ExternalityClass::ExternalSystem,
            risk: EffectRisk::Medium,
            commit_strategy: CommitStrategy::ReserveCommit,
        })
    }

    fn observe(&self, effect: &Effect) -> Result<ExternalStateSnapshot> {
        let (version, state) = self.observe_row(effect)?;
        ExternalStateSnapshot::capture(
            effect.id.clone(),
            self.name(),
            effect.target.clone(),
            version.map(|version| version.to_string()),
            state,
        )
    }

    fn prepare(&self, effect: &Effect) -> Result<PreparedEffect> {
        let mutation = self.mutation(effect)?;
        let before = self.observe(effect)?;
        match mutation.operation {
            PostgresEffectOperation::Insert if before.state != Value::Null => {
                return Err(Error::Intervention(
                    "PostgreSQL insert target already exists".into(),
                ));
            }
            PostgresEffectOperation::Update | PostgresEffectOperation::Delete
                if before.state == Value::Null =>
            {
                return Err(Error::Intervention(
                    "PostgreSQL mutation target does not exist".into(),
                ));
            }
            _ => {}
        }
        if before
            .version
            .as_deref()
            .map(str::parse::<i64>)
            .transpose()
            .map_err(|_| Error::Intervention("PostgreSQL version is not an integer".into()))?
            .is_some_and(|version| version != mutation.expected_version)
        {
            return Err(Error::Intervention(
                "PostgreSQL prepare rejected stale expected_version".into(),
            ));
        }
        let mut prepared = match &before.state {
            Value::Object(value) => value.clone(),
            Value::Null => Map::new(),
            _ => {
                return Err(Error::Intervention(
                    "PostgreSQL row snapshot was not an object".into(),
                ));
            }
        };
        for (key, value) in &mutation.key {
            prepared.insert(key.clone(), value.clone());
        }
        for (column, value) in &mutation.changes {
            prepared.insert(column.clone(), value.clone());
        }
        let next_version = match mutation.operation {
            PostgresEffectOperation::Insert => 1,
            _ => mutation.expected_version + 1,
        };
        prepared.insert(
            mutation.version_column.clone(),
            Value::Number(next_version.into()),
        );
        for field in &mutation.non_negative {
            let allowed = prepared
                .get(field)
                .and_then(Value::as_i64)
                .is_some_and(|value| value >= 0);
            if !allowed {
                return Err(Error::Intervention(format!(
                    "PostgreSQL invariant rejected {field}: expected a nonnegative integer"
                )));
            }
        }
        let prepared_value = match mutation.operation {
            PostgresEffectOperation::Delete => Value::Null,
            _ => Value::Object(prepared),
        };
        Ok(PreparedEffect {
            id: crate::core::PreparedEffectId::new(),
            effect_id: effect.id.clone(),
            adapter: self.name().into(),
            preparation_token: blake3::hash(&serde_json::to_vec(&(
                effect.scope_hash()?,
                &before.fingerprint,
            ))?)
            .to_hex()
            .to_string(),
            expires_at: Some(Utc::now() + Duration::minutes(15)),
            preview: EffectPreview {
                summary: format!(
                    "structured PostgreSQL {:?} on scoped table {}",
                    mutation.operation, mutation.table
                ),
                current: before.state.clone(),
                prepared: prepared_value,
            },
            before,
            scope_hash: effect.scope_hash()?,
            evidence: vec![
                "no database mutation occurred during prepare".into(),
                "invariants evaluated against a versioned row snapshot".into(),
            ],
        })
    }

    fn commit(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<AdapterCommitOutcome> {
        let mutation = self.mutation(effect)?;
        let target = self.target(effect)?;
        let mut client = self.connect(effect)?;
        let mut transaction = client.transaction().map_err(pg_error)?;
        if let Some(receipt) = self.receipt(&mut transaction, target, &effect.idempotency_key)? {
            transaction.commit().map_err(pg_error)?;
            return Ok(AdapterCommitOutcome::Committed { receipt });
        }
        let committed_version = self.commit_mutation(&mut transaction, target, &mutation)?;
        let Some(version) = committed_version else {
            transaction.rollback().map_err(pg_error)?;
            return Ok(AdapterCommitOutcome::NotCommitted {
                reason: "PostgreSQL compare-and-swap rejected stale or missing row".into(),
            });
        };
        let receipt = CommitReceipt {
            id: crate::core::CommitReceiptId::new(),
            effect_id: effect.id.clone(),
            adapter: self.name().into(),
            committed_at: Utc::now(),
            external_reference: Some(format!(
                "postgres://{}/{}@{}",
                target.alias, mutation.table, version
            )),
            idempotency_key: Some(effect.idempotency_key.clone()),
            result_hash: Some(
                blake3::hash(&serde_json::to_vec(&(
                    prepared.scope_hash.clone(),
                    version,
                ))?)
                .to_hex()
                .to_string(),
            ),
            metadata: json!({
                "target":target.alias,
                "table":mutation.table,
                "version":version,
                "database_boundary_only":true
            }),
        };
        let receipt_sql = format!(
            "INSERT INTO {}.{}(idempotency_key,effect_id,receipt_json) VALUES($1,$2,$3)",
            quote(&target.schema),
            quote(&target.receipt_table)
        );
        transaction
            .execute(
                &receipt_sql,
                &[
                    &effect.idempotency_key,
                    &effect.id.to_string(),
                    &serde_json::to_string(&receipt)?,
                ],
            )
            .map_err(pg_error)?;
        transaction.commit().map_err(pg_error)?;
        Ok(AdapterCommitOutcome::Committed { receipt })
    }

    fn discard(&self, _effect: &Effect, _prepared: &PreparedEffect) -> Result<()> {
        // Preparation is a read-only staging representation; there is no provider
        // transaction or lock to discard.
        Ok(())
    }

    fn compensate(&self, _effect: &Effect, receipt: &CommitReceipt) -> Result<CompensationReceipt> {
        Ok(CompensationReceipt {
            id: crate::core::CompensationReceiptId::new(),
            original_receipt: receipt.id.clone(),
            compensated_at: Utc::now(),
            status: CompensationStatus::Unsupported,
            metadata: json!({"reason":"structured PostgreSQL compensation requires a new explicitly reviewed Effect"}),
        })
    }

    fn reconcile(&self, effect: &Effect) -> Result<ReconciliationResult> {
        let target = self.target(effect)?;
        let mut client = self.connect(effect)?;
        let sql = format!(
            "SELECT receipt_json FROM {}.{} WHERE idempotency_key=$1",
            quote(&target.schema),
            quote(&target.receipt_table)
        );
        let row = client
            .query_opt(&sql, &[&effect.idempotency_key])
            .map_err(pg_error)?;
        Ok(match row {
            Some(row) => ReconciliationResult::Committed {
                receipt: serde_json::from_str(&row.get::<_, String>(0))?,
            },
            None => ReconciliationResult::NotCommitted,
        })
    }
}

type PgValue = Box<dyn ToSql + Sync>;

fn pg_value(value: &Value) -> Result<PgValue> {
    match value {
        Value::String(value) => Ok(Box::new(value.clone())),
        Value::Bool(value) => Ok(Box::new(*value)),
        Value::Number(value) if value.is_i64() => Ok(Box::new(value.as_i64().unwrap_or_default())),
        Value::Number(value) if value.is_f64() => Ok(Box::new(value.as_f64().unwrap_or_default())),
        Value::Null => Ok(Box::new(None::<String>)),
        _ => Err(Error::InvalidInput(
            "PostgreSQL structured values must be scalar string/bool/number/null".into(),
        )),
    }
}

fn predicate(
    key: &BTreeMap<String, Value>,
    first_parameter: usize,
) -> Result<(String, Vec<PgValue>)> {
    let mut values = Vec::new();
    let mut clauses = Vec::new();
    for (column, value) in key {
        values.push(pg_value(value)?);
        clauses.push(format!(
            "{}=${}",
            quote(column),
            first_parameter + values.len() - 1
        ));
    }
    Ok((clauses.join(" AND "), values))
}

fn references(values: &[PgValue]) -> Vec<&(dyn ToSql + Sync)> {
    values.iter().map(|value| value.as_ref()).collect()
}

fn target_parts(uri: &str) -> Result<(&str, &str)> {
    let value = uri.strip_prefix("postgres://").ok_or_else(|| {
        Error::InvalidInput("PostgreSQL Effect target must use postgres://".into())
    })?;
    let (alias, table) = value.split_once('/').ok_or_else(|| {
        Error::InvalidInput("PostgreSQL target must be postgres://<alias>/<table>".into())
    })?;
    if table.contains('/') {
        return Err(Error::InvalidInput(
            "PostgreSQL Effect target identifies exactly one table".into(),
        ));
    }
    validate_identifier(alias)?;
    validate_identifier(table)?;
    Ok((alias, table))
}

fn validate_identifier(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    if value.len() > 63
        || !bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(Error::InvalidInput(format!(
            "PostgreSQL identifier {value:?} must be lowercase ASCII and bounded"
        )));
    }
    Ok(())
}

fn quote(identifier: &str) -> String {
    format!("\"{identifier}\"")
}

fn pg_error(error: ::postgres::Error) -> Error {
    Error::Intervention(format!("PostgreSQL adapter operation failed: {error}"))
}
