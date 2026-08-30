# Compensation is not rollback

Rollback restores prior state as part of an operation whose provider guarantees that semantic. Compensation performs a new action intended to offset a prior committed action.

```text
commit A          external mutation 1
compensate A      external mutation 2
```

The mutation count therefore increases during successful compensation. The original receipt and Experience remain immutable. A successful compensation receives its own receipt, lifecycle event, and `COMPENSATION_OF` Experience.

Compensation may be partial, fail, or be unsupported. Delivered mock messages are unsupported because a delivered human-visible effect cannot be made unseen. A failed group compensation remains `PARTIALLY_COMPENSATED` with `manual_intervention_required: true`; Hardknock does not hide it behind “rolled back.”

Compensation requires a separate capability and explicit `--yes` in the CLI because it changes external state again.
