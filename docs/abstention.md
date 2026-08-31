# Evidence-based abstention

`ABSTAIN` means Hardknock cannot justify proceeding safely and cannot resolve the uncertainty with a bounded experiment or validated Recovery. It is a structured runtime decision, not a generic model refusal and not a substitute for security policy.

An abstention records:

- the typed reason;
- missing assurance evidence;
- unresolved risk, authority, capability, isolation, contradiction, or budget blockers;
- possible next steps that could change the decision;
- the complete hashed context, policy version, and evidence references.

Reasons include `critical_unknown`, `unsupported_effect`, `insufficient_isolation`, `no_commit_authority`, `unresolved_contradiction`, `no_validated_recovery`, `inconclusive_assurance`, `budget_exhausted`, `unsafe_to_experiment`, and external policy prohibition.

## Approval is different

Use `REQUIRE_APPROVAL` when empirical support is strong enough to prepare a consequential action but authority belongs to a user or external system. The approval payload explains the action, risk, evidence summary, requested authority, and alternatives.

Use `ABSTAIN` when the evidence or execution conditions are themselves insufficient. Asking a user to approve an unsupported irreversible action would only move uncertainty across the interface.

Examples:

```text
Prepared deployment + current assurance + missing commit authority
  → REQUIRE_APPROVAL

Irreversible notification + no Effect adapter + no safe test
  → ABSTAIN (unsupported_effect)

High-risk unknown + disposable, effect-safe Reality + budget
  → EXPERIMENT

High-risk unknown + no safe Reality or exhausted budget
  → ABSTAIN
```

Hard external policy prohibition is evaluated before runtime evidence and remains a security block when translated through the Bridge. A runtime abstention never weakens that authority.

## Resolving an abstention

Possible next steps are explicit and non-executing: inspect evidence, configure a staged or deferred Effect adapter, create sufficient isolation, run a bounded experiment, obtain the specific missing authority, collect a local reproduction, or choose a reversible alternative. Runtime gap aggregation can turn repeated abstention contexts into a curriculum recommendation, but V0.12 never starts that curriculum automatically.

Inspect decisions with:

```bash
hardknock decision show decision-<uuid>
hardknock why --decision decision-<uuid>
hardknock runtime audit
hardknock runtime gaps
```

Abstention precision is measured only when decision feedback exists. A benchmark label is not automatically promoted into production evidence.
