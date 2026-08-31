# Runtime decision records

Every runtime decision has an immutable `RuntimeDecisionRecord` containing its ID, session, context hash, full decision context, typed decision, structured evaluation, policy version, and creation time.

Migration `015_runtime.sql` adds:

| Table | Purpose |
| --- | --- |
| `runtime_policy_versions` | Immutable content-addressed controller configuration |
| `runtime_decisions` | Canonical context, evaluation, and decision record |
| `runtime_decision_reasons` | Ordered structured reasons |
| `runtime_decision_evidence` | Ordered typed evidence references without duplicating Experience bodies |
| `runtime_decision_feedback` | Append-only observed outcomes and disagreement |
| `runtime_abstentions` | Queryable typed abstention details |
| `runtime_control_events` | Requested, made, experiment, recovery, replan, approval, abstention, and disagreement events |

SQLite triggers reject update or deletion of immutable rows. Reads verify the context hash and duplicated decision fields. Writes re-evaluate the context with the supplied policy and reject a caller-constructed record that disagrees with deterministic policy.

The Bridge evaluates from its hot cache and enqueues the already constructed record on its existing ordered writer. The writer independently verifies policy and context before committing. This keeps synchronous pre-action control model-free and avoids a new SQLite-open/migration operation per action.

## Feedback

```bash
hardknock decision feedback decision-<uuid> \
  --outcome unnecessary-intervention --agent-disagreed
```

Outcomes are `successful`, `failed`, `avoided-failure`, `unnecessary-intervention`, and `inconclusive`. Optional `--experience` IDs provide provenance. Feedback cannot predate its decision and cannot replace it.

`UnnecessaryIntervention` on a Reflex-driven `REPLAN` creates negative learning: matching active/supported Reflexes are disabled in a new revision with reduced confidence, and a false-positive resilience test is retained. The original decision remains unchanged.

## Replay

```bash
hardknock decision replay decision-<uuid>
hardknock decision replay decision-<uuid> --policy conservative
```

Replay synthesizes a new context from current local evidence, keeps the original session identity, evaluates the selected current policy, and writes a new record. It does not edit or replace the original. This supports histories such as `UNKNOWN → EXPERIMENT → ACT` after validation, or `ACT → REPLAN` after a learned failure boundary.
