# Composition contracts

CompositionContract reuses BehavioralCondition and ForbiddenOutcome. It adds cross-step requirements, sequence invariants and capability/effect policies. OperationalAssumption records an owner, scope and condition. Declared expected outputs are predictions for preflight, not observations.

The deterministic preflight reports unsatisfied requirements, predicted assumption invalidation, overlapping writes needing order, incompatible simultaneous constraints, recovery conflicts, sensitive capability flows and unsupported atomicity. Results are Compatible, CompatibleWithConditions, Conflict or Unknown. Every result remains subject to empirical validation.

The initial evaluator supports primitive state predicates (equality, integer comparison and string containment). Unsupported behavioral predicates and missing observations remain unknown. Unknown does not count as success. Requirements scoped after a step are not incorrectly combined with preconditions that only hold before it.

Experiments persist actual Tool attestations, component revisions, runtime decision IDs, invariant evaluations, starting-state proof, intervention digest, evaluator digest and outcome. Renaming an evaluator cannot create independent evidence: the digest uses its checks and implementation version. The epistemic dependency set also retains the common evaluator engine and environment family.

The composition-basic-v1 promotion gate requires current components, passing control/failure-point evidence, invariant coverage, distinct evaluator specifications and no contradictory evidence or preflight authority conflicts. Distinct specifications are not a proof of independent software implementations. Assurance is scoped to the tested primitive contracts; it is not universal correctness or a production certificate.

Fail/pass comparisons with identical starting proof, revision and evaluator can create an interaction candidate. They do not establish causation. `investigate_composition_interaction` passes a concrete investigation to the existing causal engine; adjacency alone cannot validate a causal mechanism.
