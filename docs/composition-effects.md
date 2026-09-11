# Composition effects and commitment points

CompositionEffectPlan references existing effects, commitment points, compensation edges and an atomicity declaration. The initial analyzer rejects unverified fully atomic and adapter-group guarantees. Per-step semantics do not imply a multi-system transaction.

The trial engine does not execute production EffectPlans or grant commit authority. EffectPlan references can be pinned and inspected; effect execution remains at the governed existing adapter boundary. Empty local commitment points can be recorded after their invariant checks. External irreversible commitment needs real adapter receipts.

`composition_effect_state` reads the existing effect ledger and commit receipts. It reports partial commitment when some declared effects have receipts and others remain unresolved. Original receipts remain visible. Compensation is not reported as rollback, and unresolved effects are not treated as erased.

The regression fixture prepares two mock effects and commits one through explicit test authorization. Composition inspection reports one receipt, one unresolved effect and `partial_commit=true`; it reports neither rollback nor compensation-as-rollback. No real service is contacted.
