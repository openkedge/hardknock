# Scoped handoffs and plan responsibility

A handoff is a local, immutable record of structured references. It identifies the
source contribution, sender role/member, recipient assignment, review, optional
knowledge snapshot, timestamp and expiry. Its content hash covers this metadata
and payload. The local store and authenticated adapter remain the trust boundary;
an unkeyed content hash is not a remote signature.

Payloads contain only typed claim, evidence-path and contribution IDs. There are no
fields for raw scratchpads, prompts, credentials, arbitrary logs or file contents.
The API does not dereference or transmit the referenced objects. Unknown fields
are rejected. Sensitive and restricted classifications require a separate explicit
disclosure adapter and are rejected by this initial implementation. This reference
boundary is not a claim of general data-loss prevention or per-object access control
for every existing local store API.

Creation checks the sender's authenticated session, the current team revision,
both assignments' observation authority and scope, the exact reviewed operation,
and the referenced contributions/evidence. A recipient must authenticate separately
and still satisfy its role scope at delivery. Expiry is bounded by the review and
both assignments. A team revision change stops delivery while preserving the
historical record. Delivery creates no new evidence paths, observations, delegation,
external approval or effect authority.

```text
hardknock team handoff create request.json --context sender-context.json
hardknock team handoff show <handoff-id>
hardknock team handoff receive <handoff-id> --context recipient-context.json
```

These commands use the existing local administrative CLI boundary. Bridge-driven
automatic delivery and a sensitive-data disclosure policy are not implemented.

## Plan bindings

`PlanStep.responsible_role` and `PlanStep.executing_member` are optional. When
present, runtime context must identify the matching current role assignment and
member. Member IDs distinguish authenticated instances that share the same model
or executable descriptor. Empty fields are omitted from serialized plans to preserve
legacy revision hashes.

Delegations may also pin a `PlanRevisionRef`. A child cannot remove or replace its
parent's plan binding. The store checks that the pinned revision remains current;
runtime also requires the corresponding plan context. Updating a plan invalidates
its old pinned delegation without rewriting its history.

Plan step completion rechecks live team authority, responsibility and review state.
If authority changed after the recorded decision, completion requires explicit outcome
reconciliation. Existing effect receipts remain in the effect store; this check does
not infer rollback or erase completed external effects.

Recovery handoffs add a validated Recovery ID, plan run, failure evidence, committed
effect IDs and the exact recovery action hash. The recipient must hold the current
Recovery role. Referenced effects require receipts and must already appear in the
plan's committed state; the handoff grants no new commit capability.
