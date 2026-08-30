# Governed external effects

An Action records what an agent or runner invoked. An Effect describes the structured mutation that invocation intends to make. Effect commitment is a separate authority decision.

```text
Action → EffectRequest → PROPOSED → CLASSIFIED → PREPARED
                                                   │
                                  explicit authority + state recheck
                                                   │
                                      COMMITTED or UNKNOWN
```

`PREPARED` means the adapter created an isolated, staged, deferred, or shadow representation. It never means the authoritative mutation happened. Experiments can prepare and discard effects. They cannot commit them.

## Domain model

An `Effect` records its session and optional Reality, source action, kind, target URI, operation, JSON payload, adapter, classification, idempotency key, evidence, lifecycle, and fixture fault profile. The exact adapter, effect ID, target, operation, payload, and idempotency key form the authorization scope hash.

Classification keeps independent dimensions:

| Dimension | Values used in V0.8 |
| --- | --- |
| Reversibility | naturally reversible, compensatable, shadowable, deferrable, irreversible, unknown |
| Idempotency | idempotent, idempotent with key, non-idempotent, unknown |
| Isolation | Reality-local, staged, shadow, provider transaction, unsupported |
| Externality | Reality-local, host-local, external system, human-visible, financial, unknown |
| Risk | read-only, low, medium, high, critical |
| Commit strategy | direct, deferred dispatch, shadow promote, reserve/commit, compensating, unsupported |

Adapters classify structured requests. Hardknock does not infer a cloud mutation by parsing arbitrary shell text.

## Lifecycle and evidence

Domain logic permits only guarded transitions. Current state is materialized in `effects`; canonical history is an ordered immutable `effect_events` stream. Prepared records, external snapshots, authorizations, commit receipts, compensation receipts, reconciliation attempts, plans, groups, and Effect-to-Experience links remain inspectable.

A successful authoritative operation produces a separate immutable Experience. An experiment-selected commit uses `COMMIT_OF`; compensation uses `COMPENSATION_OF`; a recovered unknown outcome uses `RECONCILIATION_OF`.

## CLI

```bash
hardknock effect list
hardknock effect show effect-<uuid>
hardknock effect propose --kind http-api --operation update \
  --target mock://deployment/service-a --payload '{"version":3}' --prepare
hardknock effect prepare effect-<uuid>
hardknock effect commit effect-<uuid> --yes
hardknock effect discard effect-<uuid>
hardknock effect reconcile effect-<uuid>
hardknock effect compensate effect-<uuid> --yes
hardknock effect orphans
hardknock effect cleanup
hardknock effect capabilities
```

For CI, `effect commit` accepts a bounded regular JSON authorization file. Its `effect_ids` and `scope_hash` must match the current Effect exactly and it must not be expired.

Mock fixture commands are explicit test support:

```bash
hardknock effect fixture-set --adapter mock-http \
  --target mock://deployment/service-a --state '{"version":1}'
hardknock effect fixture-show --adapter mock-http \
  --target mock://deployment/service-a
```

These commands do not configure real providers. See [adapters](effect-adapters.md), [commit semantics](commit-semantics.md), and [security](effect-security.md).
