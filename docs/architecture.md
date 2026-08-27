# Architecture

## Implemented: Milestones 0–2

Hardknock currently provides a local execution substrate. It does not yet evaluate task success, create Experiences, infer Lessons, or run counterfactual experiments.

```text
CLI / flags
    ↓
Clean Git snapshot → StateRef (repository + commit + tree hash)
    ↓
GitRealityProvider → detached worktree + persistent Reality
    ↓
GenericShellAdapter → CommandSpec (program + argv)
    ↓
ProcessRunner → process group + deadline + stdout/stderr artifacts
    ↓
Filesystem diff + ExecutionRecord + BLAKE3 artifact references
    ↓
SQLite persistence → discard worktree, or keep for inspection
```

### Module boundaries

| Module | Responsibility |
| --- | --- |
| `core` | Typed identifiers, snapshot references, Realities, command specifications, process observations, artifact references |
| `dojo` | `RealityProvider` and Git implementation: create, fork, diff, discard |
| `agent` | `AgentAdapter`; generic command templates without a vendor API dependency |
| `process` | Noninteractive subprocess execution, timeout, cancellation, file capture |
| `store` | SQLite migrations, Reality metadata, append-only execution records, advisory locks, artifact hashes |
| `cli` | Command parsing, lifecycle coordination, human/JSON output, stable exit codes |

A single Rust crate keeps the first implementation easy to navigate. These modules can become workspace crates when independent consumers or compilation boundaries justify it. SQLite is bundled through `rusqlite`; there is no database service. `tokio` handles process waits and signals, not a distributed runtime.

## Snapshot semantics

`StateRef` identifies a canonical repository path, full commit object ID, and full tree object ID. Capturing a starting state rejects unborn, bare, dirty, and submodule repositories. Git-ignored files are not copied; they are not part of the recorded snapshot. Git SHA-1 and SHA-256 object ID lengths are accepted.

Every Reality is a detached Git worktree. Forking recreates the **recorded starting commit**, even if the parent has changed or was discarded. It does not clone the parent's current modifications. The source branch and index are not intentionally changed by Hardknock's lifecycle operations.

Diff capture uses a temporary index, so it includes tracked changes and nonignored new files without changing the agent's index. It compares against the original commit, including changes the agent committed in its detached worktree. Git's binary patch format is retained as bytes; ignored files are omitted. Git hooks and filesystem-monitor hooks are disabled for Hardknock's own Git commands; repository filters and other Git configuration can still affect execution.

This reproduces repository content, not the full environment. Dependencies, tool versions, clocks, randomness, Git configuration, network responses, and host state are not frozen. Future evaluators and experiments must record and constrain these inputs before treating trials as comparable.

## Execution and retention

1. Validate the repository and command template before creating trial state.
2. Acquire a Reality lease and persist creation intent before adding the worktree.
3. Start a new Unix process group with stdin closed and stdout/stderr redirected to files.
4. Wait for exit, timeout, SIGINT, or SIGTERM. Terminate the group before artifact collection.
5. Capture the diff, hash artifacts, write `metadata.json`, and insert the execution record.
6. Discard the worktree unless `--keep` was requested. Normal nonzero exits, timeouts, and interrupts also produce records and clean up.

If evidence capture or persistence fails, retain the worktree and report its ID/path instead of destroying uncaptured changes. A missing executable cleans up its worktree and reports a runtime error. Partial artifact directories may remain after runtime failures.

The runner uses SIGKILL for prompt group termination; graceful shutdown hooks are not implemented. Ordinary background descendants are terminated even when the main command exits successfully. Processes that create new sessions/process groups can escape this mechanism. Hard termination of Hardknock itself, host failure, or an escaped process cannot be made transactional by this backend.

`reality cleanup` removes only unlocked automatic-run Realities. It leaves manual/kept Realities alone and skips active leases. It never runs a blanket `git worktree prune` or deletes arbitrary directories. Stop any abandoned commands before orphan cleanup; a released lease is not proof that every descendant has stopped. Filesystem and SQLite updates are not one atomic transaction, so cleanup is explicit and retryable.

## Safety boundary

**A Git worktree is not a secure sandbox.** The network, home directory, credentials, processes, host filesystem, Git objects, Git refs, and repository configuration are shared. Commands may modify the source repository through absolute paths or Git operations. Hardknock cannot roll those effects back.

Use trusted commands on disposable tasks. Do not run untrusted code or commands with irreversible external effects. The CLI prints this warning before creating a Reality for execution; quiet mode does not hide it. Environment variables are inherited but not copied into records or debug logs. Commands, tasks, diffs, and output artifacts may contain secrets and are stored without general redaction. The dedicated data directory is private to the local user; this is not authentication or a policy engine.

The V0.1 substrate currently targets Linux and macOS. Windows, containers, VM isolation, remote execution, and named vendor adapters are deferred.

## Planned learning loop

```text
Agent → Adapter → Reality → Execution → Evaluation → Experience
                                                        ↓
                                                   Reflection
                                                        ↓
                                                Candidate Lesson
                                                        ↓
                                                   Experiment
                                                        ↓
                                                    Evidence
                                                        ↓
                                                Lesson Promotion
```

Milestone 3 adds evaluation without changing the process runner. Milestone 4 builds immutable Experiences from execution and evaluation evidence. Reflection, counterfactual planning, confidence, retrieval, and retry remain separate later modules. Process exit zero is currently only an observation; it must not be promoted into task success by assumption.

## Dependency references

Command substitution follows [shell-words' literal argument parsing](https://docs.rs/shell-words/latest/shell_words/fn.split.html). Process lifecycle handling uses [Tokio's process API](https://docs.rs/tokio/latest/tokio/process/struct.Command.html), with explicit process-group cleanup beyond `kill_on_drop`. Persistence uses [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/) and local migration files.
