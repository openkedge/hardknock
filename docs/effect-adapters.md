# Effect adapters

Transactional safety exists only for targets routed through an adapter that implements the required contract. V0.8 adapters use a separate local SQLite fixture as authoritative external state, so the default tests remain network-free and external-service-free.

| Adapter | Schemes | Strategy | Prepare | Commit | Discard | Compensate | Reconcile | Idempotency |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `mock-http` | `mock://`, `mock-http://` | compensating | yes | yes | yes | yes | yes | key |
| `mock-db` | `mock-db://` | provider-style optimistic check | yes | yes | yes | yes | yes | key |
| `mock-message` | `mock-message://` | deferred dispatch | yes | yes | yes | no after delivery | yes | key |
| `shadow-deployment` | `shadow://` | shadow promote | yes | yes | yes | yes | yes | key |

All four adapters record the observed version and fingerprint during prepare. Commit checks the current version again inside the fixture transaction. An idempotency record and resource mutation commit atomically in the mock external database.

`mock-db` also rejects a structured negative `balance` at classification. `mock-message` is human-visible and always needs external approval; delivery is irreversible in the model, so compensation reports `UNSUPPORTED`. `shadow-deployment` stages a private candidate and changes only the authoritative active pointer at commit.

Fixture fault injection covers prepare failure, commit failure before mutation, response loss after mutation, response loss with reconciliation failure, reservation expiry, discard failure, and compensation failure. These flags are deterministic test inputs, not production error simulation claims.

## Registry selection

The registry selects by an explicit adapter name or target scheme. Duplicate scheme registrations are rejected. Capabilities are queryable with `hardknock effect capabilities`; callers must not assume that every adapter supports compensation or shadow resources.

V0.8 does not include real PostgreSQL, AWS, Kubernetes, Slack, email, or payment adapters. It also does not proxy arbitrary HTTP or recognize arbitrary CLI commands as structured Effects.
