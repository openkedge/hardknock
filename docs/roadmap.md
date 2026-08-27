# Implementation roadmap

The local Experience → Hypothesis → Experiment → Evidence → Retrieval → Application → Validation loop is implemented. The evidence is limited to trusted local fixtures and explicit scripts; the generic context contract has one successful Codex CLI smoke test.

| Milestone | Status | Deliverable |
| --- | --- | --- |
| 0 — Bootstrap | Implemented | Rust crate, CLI, typed errors/IDs, SQLite migrations, Linux/macOS CI definition |
| 1 — Reality | Implemented | Detached worktrees, clean snapshots, forks/diffs/disposal, leases and cleanup |
| 2 — Agent execution | Implemented | Generic argv adapter, outputs/diffs, immutable Executions, deadlines/signals |
| 3 — Evaluation | Implemented | Required command checks, evaluation distinct from process status |
| 4 — Experience | Implemented | Immutable observations, context, signatures, artifact/provenance links |
| 5 — Candidate Lesson | Implemented | Manual/fixture hypotheses, scoped versioned Lessons |
| 6 — Counterfactual | Implemented | Fresh controlled paired trials, equivalence checks, classification |
| 7 — Lesson promotion | Implemented | Distinct application validation, contradiction, explicit retirement |
| 8 — Retrieval and retry | Implemented | Explained scoring, context injection, bounded retries, lineage, `why` |
| 9 — Agent integration | Partial | Vendor-neutral files and generic Codex smoke test; named adapters deferred |

## Verified acceptance boundaries

Fixture A fails, proposes a Lesson, compares explicit strategies, and succeeds on an opt-in retry. B differs in tree, packages and task: its experience-disabled control fails with one repeated mistake; its observed advised application succeeds without that mistake and validates the Lesson. C rejects irrelevant pnpm advice. D supplies a controlled contradiction that lowers confidence without erasing support or retiring the Lesson.

The suite also covers duplicate contexts, self-reports, candidate/retired exclusion, cancellation, retry limits, context collisions, concurrent evidence writes, immutable history, migration, and Reality cleanup. No external model is required for tests. Local verification is on macOS; configured Linux/macOS CI is not evidence of a completed remote CI run.

See [the transfer phase report](implementation-transfer.md) and the historical [Milestones 3–6 report](implementation-phase-3-6.md). The historical report describes the earlier boundary, not current functionality.

## Exact next-phase plan

**Active resilience building in trusted local fixtures**, proceeding through:

```text
Validated Skill → Deliberate Perturbations → Failure Boundary Discovery
                                                      ↓
                                              Operating Envelope
                                                      ↓
                                            Advisory Reflex → Recovery
```

1. Define a Skill as an explicit replayable procedure, evaluator, applicability scope and source Experiences. Establish its clean baseline before calling it validated.
2. Add bounded, deterministic, reversible local perturbations one factor at a time: fixture files, explicit environment inputs or command replacement. Record seeds, trial budget, manifests and deviations. Do not begin with network/cloud faults.
3. Compare perturbed trials with matched controls, retaining all failures and inconclusive observations. Discover boundaries only within the tested factor ranges; preserve correlations and duplicate-trial limits.
4. Derive an operating envelope that links each tested condition to its evidence. It must distinguish observed regions, unknown regions and contradicted expectations.
5. Derive disabled-by-default advisory reflex candidates from reproducible precursors. Activation must be explicit; advice or replanning does not authorize blocking or extra permissions.
6. Test a bounded recovery procedure from each captured failure state with explicit restoration checks. Measure recovery success, repeated mistakes and total experiment/retry cost against controls, preserving every Experience.

This is a recommendation for the next pass, not work started here. Keep the first resilience work offline and deterministic. Stronger isolation, fuller environment manifests, crash reconciliation, artifact verification/retention, broader tasks and verified external-agent observers remain focused follow-ups. Do not add hosted services, arbitrary action interception, automatic policy enforcement, or universal rollback claims.
