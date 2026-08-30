# Effect commit semantics

Commit is a deliberate transition across the authoritative boundary.

```text
PREPARED
   ↓ exact-scope authorization
observe authoritative state
   ↓ fingerprint/version/expiry gate
adapter commit with stable idempotency key
   ↓
COMMITTED + receipt + post-commit snapshot + Experience
```

## Authorization

`CommitAuthorization` names its authority (`user`, `policy`, `ci`, or `external_approval_system`), exact Effect IDs, grant/expiration times, and scope hash. Agent self-approval is not an authority. Bridge agents can propose, prepare, inspect, discard, and request commit, but a commit request returns `authorization_required`.

The scope hash includes the Effect ID, adapter, kind, target, operation, payload, and idempotency key. Changing a payload or substituting another Effect makes authorization validation fail before the adapter runs. This is the V0.8 TOCTOU boundary; it is not a production signature system.

## Stale state and expiration

Prepare captures an external version and fingerprint. Commit observes again. A mismatch returns `Rejected { reprepare: true }` while leaving the Effect prepared and the changed authoritative state untouched. An expired preparation follows the same reprepare path.

## Groups

An `EffectPlan` has explicit dependencies and an honest atomicity class. V0.8 topologically commits simple DAGs. Across adapters it never claims an atomic transaction. If A and B commit and C fails, the group records `PARTIALLY_COMMITTED`. A compensating group then applies new compensation mutations in reverse commit order and separately reports `FULLY_COMPENSATED` or `PARTIALLY_COMPENSATED`.

`--yes` is explicit local user authorization for the exact stored scope. An authorization file supports deterministic noninteractive use. Neither mode is a multi-user approval system.
