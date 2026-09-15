# Plan validity under changing evidence

An execution plan is an explicitly supplied, bounded DAG of at most 100 steps. Hardknock checks the next step against current evidence; it does not invent goals or schedule agents. Definitions and revisions are immutable in history. A changed definition must advance the revision exactly once.

Assumptions identify predicates and dependent steps. Declared `Supported` values are descriptive: evaluation derives support from authoritative observations, freshness, and external resource versions. Missing evidence and stale truth require verification. A contradicted critical dependency requires replanning. Unrelated assumptions do not interrupt the next step. A lower-trust report cannot refresh a higher-trust observation.

The default evidence ages are 30 minutes generally, five minutes for critical assumptions, and one minute at commitment. Before-next-mutation observations are tied to a mutation epoch. Gates require fresh assumptions, invariants, effect-bound external approvals, available recoveries, and any configured evidence diversity. They never supply commit authority. Hard policy and capability checks retain precedence in the RuntimeController.

Checkpoint snapshots preserve the observed state. Resume evaluates the current run rather than replacing it with the snapshot. Replanning preserves completed steps and crossed commitments, clears approvals, and checks the explicitly saved newer revision. Failure reconciliation reads the effect ledger, including partial completion, instead of inferring that failed steps had no effects.

## Commands

`hardknock plan` provides `list`, `import`, `show`, `inspect`, `assumptions`, `checkpoints`, `start`, `status`, `why`, `history`, `replay`, `diff`, `checkpoint`, `resume`, `replan`, `recovery`, `forecast`, and `validate`. Run each command with `--help` for argument shapes.

`why` accepts a serialized RuntimeDecisionContext. It produces an assessment; publication through RuntimeStore resolves authoritative context and rechecks validity inside the decision transaction. Bridge-supplied validity and commitment claims are discarded.

`validate` accepts concrete local shell ExperimentRequests, a target Skill, and a curriculum budget. It uses the existing curriculum and controlled-experiment lifecycle. It does not execute a production deployment or grant authority. Plan observations feed a V0.15 trajectory; causal drift investigations use the V0.14 engine, and plan evidence gaps use V0.16 opportunity scoring.

## Boundaries

Tool, shell-Skill, observation, approval, composition, experiment and effect completion are reconciled against recorded decisions and engine evidence. Shell Skills require the registered, verified V0.19 procedure wrapper and its exact input hash. Custom steps fail closed. Nested composition assumptions and entire-sequence invariants are checked before use; nested commitments tighten evidence freshness and their receipts remain visible in recovery and checkpoint state. There is no automatic step scheduler.

Attestation-backed arbitrary observation claims are rejected until an adapter binds them to verified output artifacts. Runtime observation ingestion is an internal trusted-adapter API, not a source-verification mechanism for arbitrary external JSON.

The optional `plan-continuity-basic-v1` profile requires controlled evidence, recovery coverage and plan-family continuity evidence. The latter remains inconclusive until a family-specific evaluator is supplied; runtime instance validity cannot produce a certificate. There is no automatic Guard promotion. The existing hierarchy and GuardCandidate review mechanisms remain in force. Forecast integration records drift and mutation features; no newly validated predictive signature is claimed. Replan appropriateness remains inconclusive without controlled counterfactual evidence.
