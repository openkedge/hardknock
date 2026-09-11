# Recovery composition

CompositionRecoveryPlan associates a failure point with versioned Recoveries, required/established conditions, invariants and effect reconciliation references. Preflight detects a recovery establishing a state that conflicts with another recovery's requirement. Recovery revisions and retirement are checked alongside primary components.

Recovery procedures execute as explicit pinned Recovery steps through the existing ToolRouter wrapper. The composition layer does not invent an unbounded retry or recovery scheduler. Shell procedures are supported; environment mutation/replanning steps need an explicit registered Tool binding.

A successful recovery process is only local evidence. Final global contract/invariant observations distinguish FullyRecovered, LocallyRecovered, Compensated, Failed and Inconclusive. Unknown global state remains inconclusive. Compensation requires ledger evidence and must not erase an irreversible receipt.

The credential model demonstrates a deployment requiring rollback after the original credential was revoked. The normal rollout passes, the delayed rollback fails, and preserving the old credential makes the counterfactual pass. This is a local model of the interaction, not a real credential rotation or proof of arbitrary service recovery.

Failure injection is explicit and bounded to named steps and typed state transitions. The assurance report exposes untested failure points and recovery gaps. Definitions and successful component tests cannot self-declare whole-composition recovery validation.
