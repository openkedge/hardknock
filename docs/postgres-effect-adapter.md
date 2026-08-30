# PostgreSQL Effect adapter

The V0.9 PostgreSQL adapter is the first non-mock Effect adapter. It is a host-side, credential-owning adapter for narrowly structured row mutations against configured test targets. The agent receives neither the connection string nor arbitrary SQL execution.

## Configuration and target scope

Targets are loaded from `$HARDKNOCK_HOME/effects/postgres-targets.json`. The file must be a regular, non-symlink file with mode `0600` on Unix. Each alias declares a connection string, schema, allowed table names, and a receipt table. Configuration lives outside the repository and capability-visible workspace.

Effects use `postgres://<alias>/<table>`. Aliases, schema, table, version column, receipt table, key columns, and change columns accept only bounded lowercase ASCII SQL identifiers. The alias/table must exist in configuration and in the Reality's Effect target patterns.

```json
{
  "operation": "update",
  "table": "inventory",
  "key": {"sku": "axolotl"},
  "changes": {"quantity": 9},
  "expected_version": 5,
  "version_column": "version",
  "non_negative": ["quantity"]
}
```

Supported operations are structured insert, update, and delete. The structured operation must match the Effect operation. Arbitrary SQL, expressions, joins, DDL, transactions spanning aliases, and user-selected connection strings are rejected.

## Prepare and commit

Prepare opens a normal connection, reads the scoped row, validates the expected version and declared non-negative values, captures a fingerprint/snapshot, and closes without holding a transaction or long-lived lock. `PREPARED` grants no mutation authority.

Commit revalidates the Effect authorization and capability scope in the host Effect Manager, opens a transaction, checks the idempotency receipt table, executes a compare-and-swap mutation against `expected_version`, writes the receipt in the same database transaction, and commits. A missing CAS row is a stale conflict. Retrying the same idempotency key returns the existing receipt rather than repeating the mutation. Reconcile reads that receipt. Discard is a no-op at the database. Compensation is unsupported and must be represented as a new reviewed Effect.

PostgreSQL transactionality covers only this adapter's database boundary. V0.9 does not use PostgreSQL prepared transactions as distributed two-phase commit and makes no cross-system atomicity claim. The configured receipt table must already exist; schema creation is an operator/test-fixture responsibility.

## Test fixture contract

The optional real integration test reads `HARDKNOCK_TEST_POSTGRES_URL`, creates an isolated test schema/table/receipt table, and demonstrates invariant rejection, preparation, stale conflict after a concurrent update, reprepare, authorized commit, one receipt, and idempotent retry. It skips clearly when the variable is absent.

During the V0.9 pass the Rust adapter and optional test compiled, while no PostgreSQL service/URL was available, so no live database transaction result is claimed. The pure security suite covers structured target/table/operation validation, raw-SQL-shaped payload rejection, scope escalation, and Effect Manager commit authority with mock storage.
