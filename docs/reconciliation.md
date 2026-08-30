# Unknown outcomes and reconciliation

A process or network failure after a commit request does not prove the mutation failed. When the mock adapter applies the mutation but loses the response, Hardknock transitions the Effect to `UNKNOWN` and says that a mutation may have occurred.

`UNKNOWN` is not discardable. The safe recovery path queries the adapter by the stable idempotency key:

```text
UNKNOWN
   ↓ reconcile(idempotency key)
found receipt     → COMMITTED
not found         → FAILED / safe to reconsider
still ambiguous   → UNKNOWN
```

Successful reconciliation reconstructs the real stored receipt, captures current external state, creates a `RECONCILIATION_OF` Experience, and transitions `UNKNOWN → COMMITTED`. A retry of the same commit is also safe when the adapter advertises idempotency keys: the existing receipt is returned and the authoritative mutation count remains one.

Every reconciliation attempt is immutable and inspectable. A deterministic failure profile demonstrates `StillUnknown`; Hardknock retains the state and requires operator attention rather than guessing.
