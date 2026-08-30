// SPDX-License-Identifier: Apache-2.0

use hardknock::effects::*;
use serde_json::json;

fn request(payload: serde_json::Value) -> EffectRequest {
    EffectRequest {
        session_id: "postgres-structure-test".into(),
        reality_id: None,
        source_action: ActionRef {
            id: "update-inventory".into(),
            kind: "test".into(),
        },
        kind: EffectKind::Database,
        target: EffectTarget {
            uri: "postgres://inventory_test/inventory".into(),
        },
        operation: EffectOperation::Update,
        payload,
        adapter: Some("postgres".into()),
        evidence: vec![],
        fault: EffectFault::None,
    }
}

fn adapter(connection: &str, table: &str, receipt: &str) -> PostgresEffectAdapter {
    PostgresEffectAdapter::new(vec![PostgresTargetConfig {
        alias: "inventory_test".into(),
        connection: connection.into(),
        schema: "public".into(),
        allowed_tables: vec![table.into()],
        receipt_table: receipt.into(),
    }])
    .unwrap()
}

#[test]
fn structured_postgres_scope_rejects_raw_sql_table_escalation_and_operation_mismatch() {
    let adapter = adapter("postgresql://127.0.0.1:1/test", "inventory", "hk_receipts");
    let valid = request(json!({
        "table":"inventory",
        "operation":"update",
        "key":{"sku":"ABC"},
        "expected_version":5,
        "changes":{"quantity":9},
        "non_negative":["quantity"]
    }));
    assert!(adapter.classify(&valid).is_ok());
    let raw_sql = request(json!({"sql":"UPDATE inventory SET quantity=9"}));
    assert!(adapter.classify(&raw_sql).is_err());
    let escaped = request(json!({
        "table":"users",
        "operation":"update",
        "key":{"id":1},
        "expected_version":1,
        "changes":{"admin":true}
    }));
    assert!(adapter.classify(&escaped).is_err());
    let mismatch = request(json!({
        "table":"inventory",
        "operation":"delete",
        "key":{"sku":"ABC"},
        "expected_version":5
    }));
    assert!(adapter.classify(&mismatch).is_err());
}

#[test]
fn optional_real_postgres_fixture_rejects_invariant_and_stale_commit_then_commits_reprepared() {
    let Ok(connection) = std::env::var("HARDKNOCK_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping optional real PostgreSQL integration: HARDKNOCK_TEST_POSTGRES_URL is unset"
        );
        return;
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let table = format!("inventory_{}", &suffix[..12]);
    let receipts = format!("hk_receipts_{}", &suffix[..12]);
    let mut client = ::postgres::Client::connect(&connection, ::postgres::NoTls).unwrap();
    client
        .batch_execute(&format!(
            "CREATE TABLE public.\"{table}\"(sku text PRIMARY KEY, quantity bigint NOT NULL, version bigint NOT NULL);\
             CREATE TABLE public.\"{receipts}\"(idempotency_key text PRIMARY KEY,effect_id text NOT NULL,receipt_json text NOT NULL);\
             INSERT INTO public.\"{table}\"(sku,quantity,version) VALUES('ABC',10,5);"
        ))
        .unwrap();
    let adapter = adapter(&connection, &table, &receipts);
    let make = |quantity, version| {
        let mut request = request(json!({
            "table":table,
            "operation":"update",
            "key":{"sku":"ABC"},
            "expected_version":version,
            "changes":{"quantity":quantity},
            "non_negative":["quantity"]
        }));
        request.target.uri = format!("postgres://inventory_test/{table}");
        Effect::from_request(
            request,
            hardknock::core::EffectLedgerId::new(),
            "postgres".into(),
        )
    };
    let invalid = make(-10, 5);
    assert!(adapter.prepare(&invalid).is_err());
    let first = make(9, 5);
    let prepared = adapter.prepare(&first).unwrap();
    client
        .execute(
            &format!("UPDATE public.\"{table}\" SET quantity=8,version=6 WHERE sku='ABC'"),
            &[],
        )
        .unwrap();
    assert!(matches!(
        adapter.commit(&first, &prepared).unwrap(),
        AdapterCommitOutcome::NotCommitted { .. }
    ));
    let second = make(7, 6);
    let prepared = adapter.prepare(&second).unwrap();
    let outcome = adapter.commit(&second, &prepared).unwrap();
    let receipt = match outcome {
        AdapterCommitOutcome::Committed { receipt } => receipt,
        other => panic!("expected committed PostgreSQL outcome, got {other:?}"),
    };
    let retried = adapter.commit(&second, &prepared).unwrap();
    match retried {
        AdapterCommitOutcome::Committed {
            receipt: retry_receipt,
        } => assert_eq!(retry_receipt.id, receipt.id),
        other => panic!("expected idempotent receipt, got {other:?}"),
    }
    match adapter.reconcile(&second).unwrap() {
        ReconciliationResult::Committed {
            receipt: reconciled,
        } => assert_eq!(reconciled.id, receipt.id),
        other => panic!("expected reconciled receipt, got {other:?}"),
    }
    let row = client
        .query_one(
            &format!("SELECT quantity,version FROM public.\"{table}\" WHERE sku='ABC'"),
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 7);
    assert_eq!(row.get::<_, i64>(1), 7);
    let receipt_count: i64 = client
        .query_one(&format!("SELECT count(*) FROM public.\"{receipts}\""), &[])
        .unwrap()
        .get(0);
    assert_eq!(receipt_count, 1);
    client
        .batch_execute(&format!(
            "DROP TABLE public.\"{table}\"; DROP TABLE public.\"{receipts}\";"
        ))
        .unwrap();
}
