# Hardknock V0.8 implementation report

This report describes the deterministic adapter implementation on 2026-08-29. It does not claim transparent interception or production-provider transactionality.

## 1. Files created and changed

The pass adds `effects::{model,adapter,mock,manager,benchmark}`, effect storage/CLI code, migration 011, four fixture manifests, 18 focused integration tests, a committed benchmark summary, seven semantic guides, and updates to Reality, experiments, Bridge, configuration, CLI, README, architecture, and roadmap.

## 2. Schema migration

Migration 011 adds ledgers, Effects, immutable events, prepared records, external snapshots, authorizations, commit/compensation receipts, plans/groups, reconciliation attempts, Effect-to-Experience links, and benchmark runs. It extends immutable Experience relations with `commit_of`, `compensation_of`, and `reconciliation_of` without rewriting Experience JSON.

## 3. Effect domain model

`Effect` separates the source Action from kind, target, operation, payload, adapter, classification, lifecycle, idempotency key, evidence, and Reality ledger. Payloads are bounded structured JSON and targets require explicit schemes.

## 4. Lifecycle state machine

Domain transitions guard `PROPOSED → CLASSIFIED → PREPARED`, terminal discard/failure/rejection, `PREPARED → COMMITTED|UNKNOWN`, `UNKNOWN → COMMITTED|FAILED`, and `COMMITTED → COMPENSATED`. CLI and adapters cannot assign state directly.

## 5. Classification model

Classification independently records reversibility, idempotency, isolation requirement, externality, risk, and commit strategy. Structured adapters classify requests; arbitrary command text is not treated as authoritative effect metadata.

## 6. Adapter interface and capabilities

The local deterministic interface exposes classification, observation, preparation, commit, discard, compensation, and reconciliation. The registry reports simulation, staging, commit, discard, compensation, reconciliation, idempotency-key, and shadow support per adapter. It is synchronous because every V0.8 provider is local; remote asynchronous providers are deferred.

## 7. Mock adapters

`mock-http`, `mock-db`, `mock-message`, and `shadow-deployment` use a separate local external-state database. They implement compensating update, optimistic database commit, deferred human-visible dispatch, and shadow promotion semantics respectively. Message compensation is accurately unsupported after delivery.

## 8. Transactional Reality changes

`Reality` optionally stores an `EffectLedgerId`. `reality show` includes effect counts. `reality discard` discards attached staging before removing files and fails with exact leftovers if cleanup is incomplete or an Effect remains `UNKNOWN`.

## 9. Effect Ledger semantics

Current lifecycle is indexed for bounded lookup; ordered Effect events are canonical and immutable. Events preserve proposal, classification, preparation, authorization/rejection, commitment, unknown outcomes, reconciliation, discard, and compensation failure/success.

## 10. Commit authorization design

Authority types are user, policy, CI, and external approval system. Agent self-approval is absent. Authorization binds a sorted exact set of Effect IDs and each Effect scope hash, with an optional expiration. CLI `--yes` creates local user authority; a bounded regular JSON file supports CI fixtures.

## 11. Stale-state and TOCTOU protections

Prepare stores version/fingerprint and the scope hash. Commit re-observes external state and verifies version, fingerprint, expiration, authorization membership, and scope. Drift returns `reprepare: true` without adapter commit. Payload tampering invalidates authorization before mutation.

## 12. Idempotency design

Every Effect receives `hk-effect:<effect-id>`. The mock external transaction stores the resource mutation and receipt under that key. Retrying an `UNKNOWN` Effect first uses the same key and returns the prior receipt; the measured mutation count remains one.

## 13. UNKNOWN outcome handling

The response-loss fault commits external state and idempotency evidence, then returns no receipt. Hardknock records `UNKNOWN` and refuses discard. It never converts timeout into “not committed.”

## 14. Reconciliation results

Lookup by idempotency key returns committed receipt, not committed, or still unknown. The deterministic demonstration recovers `UNKNOWN → COMMITTED`, persists the attempt and reconstructed receipt, and creates a reconciliation Experience. An injected lookup failure remains UNKNOWN.

## 15. Compensation semantics

Compensation is a second mutation, never described as rollback. Successful and failed receipts are immutable. A successful outcome moves an Effect to `COMPENSATED`; failure leaves the committed state visible and requires review.

## 16. Partial-commit handling

A topologically ordered compensating group demonstrates A committed, B committed, C failed; reverse compensation succeeds for B and fails for A. The group retains `PARTIALLY_COMMITTED`, final `PARTIALLY_COMPENSATED`, every receipt, the failed Effect, and `manual_intervention_required: true`.

## 17. Multi-effect plan design

Plans use explicit Effect IDs, simple dependencies, cycle detection, and honest atomicity classes. V0.8 supports best-effort and compensating groups. It does not claim atomic commit across adapters.

## 18. Bridge and agent integration

Bridge events expose prepare, commit request, discard, status, and reconcile. Agent proposal prepares through the same manager and returns `committed:false`. Commit requests from agents record rejection and return `authorization_required`. Explicit tools do not imply transparent interception.

## 19. CLI commands

`hardknock effect` provides list/show/propose/prepare/commit/discard/compensate/reconcile/capabilities/orphans/cleanup, plan create/commit, and mock fixture inspection. `benchmark transactional-effects` persists and can export the benchmark.

## 20. Deterministic no-leak experiment

An actual `CandidateExecution::EffectPlan` comparison runs the strategy-choice fixture. The direct candidate fails and its Effect is discarded. The shadow candidate passes and remains only PREPARED. Authoritative mock service state remains v1 until a later authorized commit changes it to v3.

## 21. Stale commit tests

HTTP and database fixtures both prepare against version 1, observe an external version-2 change, and reject commit. The database fixture also rejects a negative structured balance before preparation. Reservation expiry follows the reprepare path.

## 22. Authorization-binding tests

The suite approves v2, modifies the stored payload to v3, and observes scope rejection with zero mutations. Expired authorization also fails closed. A Bridge agent with a valid user token still cannot exercise commit authority itself.

## 23. Unknown-outcome tests

Response loss produces one external mutation, `UNKNOWN`, and a recovered receipt. A same-key retry also produces one mutation. A combined reconciliation-loss profile remains UNKNOWN and inspectable.

## 24. Partial-commit and compensation tests

The group fixture verifies two receipts before the third failure, reverse compensation order, one successful compensation, one injected failure, unchanged failed target, and explicit manual intervention.

## 25. ExternalMistakeEscapeRate benchmark

The committed deterministic run has four experimental failures per arm:

| Arm | Failed-candidate authoritative mutations | Escape rate |
| --- | ---: | ---: |
| Direct agent | 4/4 | 1.0 |
| Filesystem sandbox only | 4/4 | 1.0 |
| Hardknock supported transactional adapters | 0/4 | 0.0 |

It also records 1/1 stale conflict, 1/1 unknown-outcome recovery, zero duplicate mutation, and the required partial-compensation state. See `docs/benchmarks/v08-transactional-effects-summary.json`.

## 26. Known limitations

Git commands remain trusted host processes. No syscall/network proxy or credential isolation exists. The adapters are deterministic local fixtures, not real remote systems. Authorization files are unsigned local records. Event SQLite is append-only by trigger, not tamper-proof. Reconciliation is manual. Provider crash atomicity is demonstrated only by the mock database.

## 27. Deviations and rationale

The adapter interface is synchronous because V0.8 performs no network I/O; this keeps Bridge handling deterministic and avoids hiding a runtime in local methods. `EffectPlan` adds Reality-local simulation steps so existing evaluators can test candidate behavior while structured external requests stay staged. Selected Effects are detached to standalone ledgers after their worktrees are discarded. General invariants are represented, while V0.8 executes the mandatory version/fingerprint and mock database balance checks.

## 28. Recommended V0.9 direction

Add an actual container Reality provider with deny-by-default network, capability manifests, scoped mounts and credentials, execution receipts, and honest fallback behavior. Then add narrowly scoped real adapters, beginning with PostgreSQL transactions and provider-supported dry-run/shadow operations. Preserve the V0.8 rule: preparation evidence never grants mutation authority.
