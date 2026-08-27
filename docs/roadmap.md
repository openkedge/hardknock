# Implementation roadmap

This pass deliberately stops at Milestones 0–2. A working execution substrate is not yet the empirical learning vertical slice.

| Milestone | Status | Deliverable |
| --- | --- | --- |
| 0 — Bootstrap | Implemented | Modular Rust crate, CLI, typed errors/IDs, SQLite migration infrastructure, tests, Linux/macOS CI definition |
| 1 — Reality | Implemented | Clean Git state capture, detached worktrees, recreate-starting-state forks, diffs, disposal, persistence, explicit orphan cleanup |
| 2 — Agent execution | Implemented | Generic adapter, explicit argv templates, output artifacts and hashes, raw execution records, deadlines and signal handling |
| 3 — Evaluation | Planned next | `Evaluator`, command/test checks, separate agent-exit and task-evaluation results, `--check` |
| 4 — Experience recording | Planned | Typed immutable Experiences that reference execution and evaluation evidence; `experience list/show` |
| 5 — Candidate Lesson | Planned | Manual/deterministic reflection, structured scoped hypotheses, Candidate storage; no automatic truth promotion |
| 6 — Counterfactual experiment | Planned | Same-snapshot baseline/alternative trials, command replacement, differential outcome rules, durable provenance |
| 7 — Lesson promotion | Planned | One supporting pair → CounterfactuallySupported; configurable replications → Validated; a dedicated heuristic confidence policy |
| 8 — Retry and retrieval | Planned | Retrieve an applicable lesson, retry in a fresh Reality, record improvement and later-task transfer |
| 9 — Named agent adapter | Planned | Optional Claude Code or Codex noninteractive integration behind runtime detection |

## Next acceptance tests

1. Milestone 3: an agent process exits zero while the test command fails. The evaluation must report failure without altering the raw execution record. Persist check stdout/stderr, timing, and evaluator configuration.
2. Milestone 4: reopening the store exposes the same Experience and its complete execution/evaluation references. Later interpretations cannot rewrite that evidence.
3. Milestones 5–8: add a deterministic pnpm-mismatch fixture and test agent, without package downloads or LLM calls. Baseline fails, alternative passes, one experiment supports the hypothesis, replication validates it, and a later related task retrieves it. Both-pass/both-fail and contradiction cases must be covered.
4. Milestone 9: detect an installed real CLI and run an opt-in integration smoke test. Missing credentials or executables must not affect the offline test suite.

The current tests are real subprocess/worktree tests, not a simulated learning demo. They do not exercise lesson extraction, confidence, or transfer because those systems do not exist yet.

## Later directions

Configuration files, richer context retrieval, cross-agent replication, chaos trials, operating envelopes, reflex advice, recovery procedures, and stronger isolation backends follow the working learning loop. Autonomous blocking, external-effect virtualization, hosted services, and universal rollback remain outside the current scope.
