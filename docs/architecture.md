# Architecture

## Implemented: Milestones 0–6

One modular Rust crate now runs the local empirical loop. Reflection proposes a hypothesis; only recorded trials can supply counterfactual evidence. Neither retrieval, task retry, nor `Validated` promotion is implemented.

```text
                   Agent
                     │
               Agent Adapter
                     │
                  Reality
                     │
                 Execution
                     │
                 Evaluation
                     │
                 Experience (immutable)
                     │
                  Reflection
                     │
             Candidate Hypothesis
                     │
              Candidate Lesson
                     │
        Counterfactual Experiment
              /              \
         baseline         alternative
           FAIL               PASS
              \              /
                   Evidence
                     │
       Counterfactually Supported Lesson
```

| Module | Responsibility |
| --- | --- |
| `core` | Typed IDs, state references, Realities, process observations, artifact types |
| `dojo` | `RealityProvider`; detached Git worktrees, starting-state verification, diff, disposal |
| `agent` | `AgentAdapter`; literal argv template parsing for opaque CLIs |
| `process`, `cancellation` | Unix process groups, deadlines, file capture, sticky cancellation |
| `evaluation` | Async `Evaluator`, `CommandEvaluator`, required check results |
| `experience` | Immutable evidence, bounded failure signatures, context and environment fingerprints |
| `workflow` | Shared run → evaluate → persist → cleanup lifecycle for original runs and trials |
| `reflection` | `ReflectionProvider`, deterministic fixture rule, manual hypotheses |
| `lesson` | Scope and action matching, evidence relationships, guarded lifecycle, confidence policy |
| `experiment` | Explicit replay plan, fresh baseline/alternative runs, comparison policy |
| `store` | SQLite migrations, typed store traits, immutable records, revisions, provenance keys |
| `cli` | Parsing, adapter selection, human/JSON presentation; no conclusion or confidence rules |

SQLite is bundled through `rusqlite`. Tokio handles subprocess waits and cancellation. No service, model API, or additional runtime is needed for the fixture.

## State and environment equivalence

`StateRef` contains a canonical repository path, full commit ID, and full tree ID. Initial capture rejects unborn, bare, dirty, and submodule repositories. Forking recreates the recorded commit, even after the parent changes or is discarded. Ignored files are not copied or included in diffs.

Every run verifies the new worktree's HEAD and compares its files against the recorded snapshot before executing commands. Diff collection uses a temporary Git index, includes tracked and nonignored new files, and does not rewrite the agent's index. Hooks and filesystem-monitor hooks are disabled for Hardknock's own Git commands. Repository filters and other shared Git configuration can still affect checkout; a resulting content mismatch is rejected.

Opaque `--agent-command` runs inherit the caller's environment and cannot be replayed automatically. `--script`, the test adapter, and trials clear the inherited environment and set a small fixed environment. The recorded fingerprint covers that policy, OS, architecture, and the BLAKE3 digest of `/bin/sh`; per-Reality HOME/PWD paths are normalized. Both trial worktrees are verified before execution, and their fingerprints must match the source Experience. See [experimental limits](experiments.md#equivalence-and-limits).

## Evaluation and persistence lifecycle

1. Validate input and acquire an advisory Reality lease before worktree creation.
2. Reconstruct and verify the starting snapshot; collect context before any task effects.
3. Run the agent with stdin closed, outputs redirected to files, and a deadline.
4. Stop its process group, save its diff, and persist an immutable `ExecutionRecord`.
5. Run required checks sequentially in the same Reality. Ordinary check failure does not skip later checks; cancellation or timeout does.
6. Save the final diff including evaluator effects, hash artifacts, and insert the Evaluation and Experience atomically.
7. Discard the Reality unless kept or required to preserve evidence after a capture/storage error.

Process success and task success are independent. A failed process can still satisfy all required checks; an exit-zero process can fail evaluation. No checks means an inconclusive task observation; the CLI retains its old process-based exit behavior for compatibility.

Experiments persist their plan before running trials. Each trial records its own immutable Experience and artifact links. Finishing a successful comparison commits the terminal Experiment, evidence relationships, and next Lesson revision in one immediate SQLite transaction. It loads the latest Lesson revision so concurrent experiments do not overwrite each other's evidence. Failed/interrupted investigations retain completed trials without promoting a Lesson.

## Data and migration boundaries

| Migration | Contents |
| --- | --- |
| `001_substrate.sql` | Existing Realities and append-only Executions; unchanged |
| `002_experiences.sql` | Evaluations, immutable Experiences, typed artifact references |
| `003_learning.sql` | Hypotheses, Lessons, immutable revisions, Experiments, Trials, evidence and artifact links |

Foreign keys represent Lesson → source Experience/Hypothesis, Experiment → Lesson/Hypothesis/source, Trial → Experiment/Reality/Execution/Evaluation/Experience, and Trial → artifacts. Store validation checks the structured records agree with these links. Triggers reject updates/deletes of immutable history. Terminal experiments cannot be rewritten. Lessons use checked versions; updates preserve creation time and existing evidence. Changing the tested claim, scope, or commands requires a new hypothesis; a rationale can be revised through the store API.

Migrations are additive, transactional, and applied once. Existing execution JSON is not rewritten. Old artifact references default to kind `other`, and old commands to inherited environment. Unknown newer database schemas are rejected. There is no automatic backfill of old Executions into Experiences.

## Retention, cancellation, and crashes

A shared cancellation token stays set throughout the command. SIGINT/SIGTERM stops the active process group; pending checks/trials are skipped. Ordinary background descendants are also stopped when the parent exits. The runner uses SIGKILL, not graceful shutdown hooks. Processes that establish new sessions/process groups may escape.

Normal success, check failure, timeout, and cancellation retain evidence and discard trial worktrees. Capture/storage failures preserve the affected worktree and report its path so uncaptured changes are not destroyed. Earlier trial evidence remains queryable. Pre-execution verification failures clean up without starting a child.

`reality cleanup` only removes unlocked automatic-run worktrees. It leaves manual/kept/capture-failure Realities alone, and rechecks paths after taking a lease. It never prunes arbitrary Git worktrees. Stop abandoned processes before cleanup: a released lease is not proof that all descendants stopped.

SIGKILL or power loss can leave a running Experiment and an orphan worktree. `experiment list/show` exposes the plan and persisted partial trials; `reality list/cleanup` provides inspection and cleanup. Automatic resumption/reconciliation is deferred. Filesystem and SQLite writes are not a single transaction.

## Safety boundary

**Git worktrees are not secure sandboxes.** Host files, processes, network, Git objects/refs/configuration, and externally accessible credentials remain shared. Clearing environment variables and moving HOME for scripted trials does not prevent access to the user's real home or credentials through other paths. Commands can modify the source repository or perform external effects that Hardknock cannot roll back.

Use trusted scripts on disposable tasks. Warnings appear before execution and are not hidden by quiet mode. Arguments, tasks, diffs, and logs can contain secrets; there is no general redaction or disk quota. Raw environment secrets are not copied into records. Data directories are private to the current OS user, not authenticated storage or an enforcement policy.

Linux and macOS are supported targets; Windows, containers, remote workers, and vendor-specific adapters remain deferred.
