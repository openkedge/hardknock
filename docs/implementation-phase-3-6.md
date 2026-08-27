# Milestones 3–6 implementation report

Verified locally on macOS arm64 with Rust/Cargo 1.98.0. This pass adds evaluated evidence, hypotheses, Lessons, and explicit counterfactual experiments. The existing README mascot and Apache-2.0 license/NOTICE are preserved.

## Files created

- `docs/experiments.md`
- `docs/implementation-phase-3-6.md`
- `fixtures/pnpm-workspace-conflict/agent-script.sh`
- `fixtures/pnpm-workspace-conflict/hardknock-fixture.json`
- `fixtures/pnpm-workspace-conflict/package.json`
- `fixtures/pnpm-workspace-conflict/packages/demo/package.json`
- `fixtures/pnpm-workspace-conflict/pnpm-lock.yaml`
- `fixtures/pnpm-workspace-conflict/pnpm-workspace.yaml`
- `fixtures/pnpm-workspace-conflict/test.sh`
- `migrations/002_experiences.sql`
- `migrations/003_learning.sql`
- `src/cancellation.rs`
- `src/evaluation.rs`
- `src/experience.rs`
- `src/experiment.rs`
- `src/lesson.rs`
- `src/reflection.rs`
- `src/store/experiences.rs`
- `src/store/learning.rs`
- `src/workflow.rs`
- `tests/learning.rs`

## Files changed

- `CONTRIBUTING.md`
- `Cargo.toml`
- `README.md`
- `docs/architecture.md`
- `docs/cli.md`
- `docs/experience-model.md`
- `docs/roadmap.md`
- `src/agent.rs`
- `src/cli.rs`
- `src/core.rs`
- `src/dojo.rs`
- `src/error.rs`
- `src/lib.rs`
- `src/main.rs`
- `src/process.rs`
- `src/store.rs`
- `tests/substrate.rs`
- `tests/support/mod.rs`

## Database migrations

- `002_experiences.sql`: Evaluations, immutable Experiences, normalized typed artifact references and append-only triggers.
- `003_learning.sql`: Hypotheses, versioned Lessons, immutable revisions, Experiments, Trials, evidence relationships and trial artifact links.

Schema 1 remains unchanged. An integration test opens a version-1 database, migrates it, and verifies its original Execution JSON was not rewritten. Schema-version checks and transactional migration remain in place.

## Architecture decisions

- Retain one modular Rust crate and the existing process/worktree substrate. `workflow::run_once` coordinates both original runs and trials; evaluation and interpretation stay separate.
- Required command checks decide task outcome independently of process exit. Missing checks yield an inconclusive observation with the historical process-based CLI exit fallback.
- Experiences/Evaluations/Hypotheses/Trials and prior Lesson revisions are immutable. Lesson confidence/status updates use domain policies and explicit evidence.
- Replay operates on complete recorded scripts in a controlled environment, never hidden commands inside an opaque agent. Every trial verifies its commit/tree and environment fingerprint.
- Store provenance in typed records and foreign keys. Complete a comparison and revise its Lesson in one immediate transaction. Immediate write transactions also avoid SQLite snapshot-upgrade races between concurrent experiments.
- One supporting pair moves a Candidate to CounterfactuallySupported at 0.78; a contradiction lowers confidence to 0.20. Repetition does not promote to Validated, and support does not erase a prior contradiction.

See [architecture](architecture.md), [record semantics](experience-model.md), and [experimental limits](experiments.md).

## Working CLI additions

```text
run --agent-command TEMPLATE --check SCRIPT [--check SCRIPT ...] TASK
run --script SCRIPT --check SCRIPT TASK
run --agent test-agent --check ./test.sh TASK
experience list / show ID
lesson list / show ID
lesson propose --experience ID --claim TEXT --avoid SCRIPT --prefer SCRIPT
experiment list / show ID
experiment run --lesson ID
```

Existing Reality and Execution commands remain available. All additions support global human/JSON/quiet options. JSON retains one result per invocation on stdout and diagnostics on stderr. Full invocation/setup details are in [the CLI reference](cli.md).

## Representative output from the executed demo

Abridged actual output from running the commands in [the demo instructions](experiments.md#run-the-offline-demo):

```text
Evaluation: Failure · 0/1 required checks passed
Experience: exp-122baeb2-f440-4fc3-980b-b7bd631349d8
Candidate created: lesson-76121d06-b8c6-4764-a1b5-cf17aab77001 · initial confidence 0.42
  baseline · ./agent-script.sh baseline · Failure
  alternative · ./agent-script.sh alternative · Success
Conclusion: SupportsHypothesis
Lesson: CounterfactuallySupported · confidence 0.78
Original task was not retried; its evaluation remains Failure.
```

Exit code: **1**, intentionally preserving the failed original task. Follow-up list/show queries verified exactly three Experiences, one Lesson with two revisions, and one Experiment with two trials. All three Realities were discarded, the source remained clean, and SQLite integrity/foreign-key checks passed. No task retry occurred.

## Verification results

All commands completed successfully:

```text
cargo fmt --check
cargo clippy --all-targets --all-features --offline -- -D warnings
cargo test --all --offline
cargo build --locked
```

| Suite | Passed | Failed |
| --- | ---: | ---: |
| Library unit tests | 4 | 0 |
| Existing CLI integration tests | 8 | 0 |
| Learning integration tests | 14 | 0 |
| Worktree/storage substrate tests | 7 | 0 |
| Total | 33 | 0 |

There are no main-binary unit tests or doc-tests. The concurrent-experiment regression additionally passed ten consecutive runs. Tests run without network/model/package-manager access; cached dependencies are required for Cargo's offline mode. Documentation links/fences and `git diff --check` were checked. Linux/macOS CI remains configured; remote CI was not run from this pass.

The learning suite covers evaluator/process separation, all four paired outcomes, timeout-as-inconclusive, cancellation during checks/trials, complete provenance, immutable history, old-schema migration, scope/action/environment rejection, raw-secret omission, stale revisions, capture failure retention, duplicate evidence, repeated support without validation, concurrent experiments, and no automatic retry.

## Limitations and deliberate deviations

1. **No external-command reflection provider.** Manual reflection and the deterministic fixture provider prove the loop without model quality, credentials, or response-schema dependencies. External providers remain behind the `ReflectionProvider` boundary for a later pass.
2. **Single-result JSON, not a new streaming protocol.** This preserves the existing CLI contract. Complete typed Experiences, Lessons, and Experiments are added to the response.
3. **Artifact compatibility.** Keep `blake3`/`bytes`, add `kind`, and group agent/check logs in subdirectories rather than renaming existing fields. The Experience metadata mirror is not self-hashed.
4. **Limited observation schema.** Prediction/surprise/recovery observations, semantic matching, arbitrary action interception, and general perturbation engines are deferred. Actions currently represent observable process invocations. File/custom patterns are typed but not executed.
5. **Narrow equivalence.** Git files and a normalized fixed environment are checked. Clocks, network, external files, other tool binaries, kernel state, and literal absolute-path dependencies are not frozen. Only trusted scripts with controlled inputs justify the scoped comparison. Git worktrees are not security sandboxes.
6. **Retention before deletion.** Ordinary failures/timeouts/interrupts clean up; capture/storage errors retain the affected worktree to preserve uncaptured evidence. SIGKILL/power-loss recovery can leave running records; inspection and Reality cleanup are available, automatic resumption is not.
7. **Heuristic support only.** Confidence is not a calibrated probability and counterfactual support is not universal causal proof. No retrieval, retry, Validated promotion, transfer claim, named vendor integration, or performance benchmark was added.
8. **Revisions preserve the tested proposition.** Evidence/confidence/status and descriptive rationale can evolve; changing claim, scope, or actions requires a new hypothesis so old experiments are not silently reinterpreted.

## Exact next implementation plan

The next phase is scoped and ordered in [the roadmap](roadmap.md#exact-next-phase-plan):

1. Deterministic applicability-based Lesson retrieval with explanations and provenance.
2. Explicit budgeted retry in a fresh Reality, preserving the original Experience.
3. Independent repeated-evidence accounting and contradiction handling.
4. Guarded, configurable Validated promotion with auditable criteria.
5. A held-out local cross-task transfer fixture and fair comparison.

This pass does not start those steps.
