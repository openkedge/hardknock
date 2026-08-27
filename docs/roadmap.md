# Implementation roadmap

The local Experience → Hypothesis → Experiment → Evidence pipeline is implemented. Retrieval, retry, repeated-evidence validation, and transfer remain the next phase.

| Milestone | Status | Deliverable |
| --- | --- | --- |
| 0 — Bootstrap | Implemented | Rust crate, CLI, typed errors/IDs, SQLite migrations, Linux/macOS CI definition |
| 1 — Reality | Implemented | Detached worktrees, clean snapshots, forks/diffs/disposal, leases and orphan cleanup |
| 2 — Agent execution | Implemented | Generic argv adapter, outputs/diffs, immutable Executions, deadlines/signals |
| 3 — Evaluation | Implemented | Async required command checks, evaluation distinct from process status |
| 4 — Experience | Implemented | Immutable observations, context, bounded signatures, artifact/provenance links, inspection |
| 5 — Candidate Lesson | Implemented | Manual and fixture reflection, scoped hypotheses, versioned Lessons |
| 6 — Counterfactual experiment | Implemented | Fresh controlled script trials, equivalence checks, classification, durable evidence |
| 7 — Lesson promotion | Partial | Candidate → CounterfactuallySupported/Contradicted; centralized heuristic confidence; no Validated promotion |
| 8 — Retrieval and retry | Deferred | Applicability-based retrieval and explicit fresh-Reality task retries |
| 9 — Named agent integration | Deferred | Optional vendor-specific runtime detection and noninteractive adapters |

## Verified acceptance boundaries

The deterministic fixture creates an original failed Experience, Candidate Hypothesis/Lesson, failed baseline and passing alternative Experiences, supporting Experiment, and revised Lesson. It does not create a fourth task attempt. The integration suite covers all four outcome pairs, immutable evidence, reopen/query provenance, old-schema migration, cleanup, interrupted evaluation/trials, capture failure retention, scope/replay rejection, and stale Lesson updates.

Repeated supporting comparisons stay at 0.78 and do not establish validation. There are no benchmark results, transfer claims, or published binary packages. Local verification is on macOS; the existing CI definition targets Linux and macOS, but a local test run is not evidence of a remote CI pass.

## Exact next-phase plan

1. **Deterministic Lesson retrieval.** Add a query API and `lesson find` that filters repository, markers, tags, environment constraints, and eligible status before ranking. Return supporting/contradicting evidence IDs and explain exclusions. Test changed commits/markers, unrelated repositories, contradictory Lessons, and stable ranking. No vector database is needed.
2. **Explicit retry using retrieved evidence.** Add an opt-in retry command/configuration with a trial budget. Start from a fresh recorded state, select an applicable Lesson, provide its scoped advice through a structured script/adapter, and persist the new Experience plus a retry relationship. The original Experience stays unchanged. Test success, unsuccessful advice, cancellation, budget exhaustion, and cleanup.
3. **Repeated evidence policy.** Track independent comparisons by task, snapshot, evaluator, agent, and environment. Define duplicate/nonindependent evidence and prevent repeated identical runs from masquerading as independent replications. Preserve contradiction and test confidence decreases.
4. **Guarded Validated promotion.** Specify configurable replication and contradiction criteria in a domain policy, record the promotion rationale and policy version, and expose an auditable command. Add tests proving reflection, a single pair, and duplicated evidence cannot promote a Lesson. Do not introduce automatic enforcement.
5. **Cross-task transfer fixture.** Add a second held-out local task whose applicable lesson can be retrieved without hardcoded task identity. Compare an unaided run with an explicit evidence-informed retry under equal budgets. Report success/failure, cost, provenance, and cases where scope rejects transfer.

Keep the next phase offline and deterministic. External-command reflection, broader environment manifests, crash reconciliation, artifact integrity/retention tools, and optional real-agent smoke tests can be separate focused changes. Do not add cloud services, arbitrary command interception, reflex enforcement, or chaos while proving retrieval and transfer.
