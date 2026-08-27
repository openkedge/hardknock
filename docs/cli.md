# CLI reference

Milestones 0–6 are implemented for Linux/macOS. Build from source with stable Rust, Git, and a C compiler:

```bash
cargo build --locked
./target/debug/hardknock --help
```

Below, `hardknock` means the built binary on PATH or its absolute path. Use a committed repository without staged, unstaged, or untracked changes. Hardknock does not stash, commit, or reset the source checkout. Ignored files are not copied; submodules are unsupported.

## Run and evaluate

Choose exactly one adapter:

```bash
# Opaque agent: task is substituted into one literal argv element.
hardknock --repo /path/to/repo run \
  --agent-command 'my-agent --prompt {task}' \
  --check 'make lint' --check 'make test' 'fix the build'

# Explicit script: eligible for manual counterfactual replay.
hardknock run --script './fix.sh' --check './test.sh' 'fix the build'

# Local fixture: failed evaluation triggers deterministic reflection + two trials.
hardknock run --agent test-agent --check './test.sh' 'upgrade dependencies'
```

Initialize the fixture as described in [experiments.md](experiments.md#run-the-offline-demo). `test-agent` requires its marker at the Git repository root. It simulates package-manager behavior without installing npm/pnpm or downloading anything.

`--agent-command` uses shell quoting only to split argv. Exactly one complete argument must be `{task}`; substitution occurs afterward. No implicit shell expansion takes place. An explicit template such as `sh -c '{task}'` intentionally treats the task as shell code. Generic agents inherit the environment and cannot be replayed automatically.

`--script` and checks use `/bin/sh -c`. The script is executed verbatim; the task string is a recorded goal, **not** substituted into the script. Scripted runs use the controlled environment documented in [experiments.md](experiments.md#equivalence-and-limits). All adapters are noninteractive with stdin closed. Named Claude/Codex/Hermes adapters are not implemented.

| Flag | Behavior |
| --- | --- |
| `--agent-command TEMPLATE` | Opaque executable/argv adapter |
| `--script SCRIPT` | Explicit replayable shell script |
| `--agent test-agent` | Deterministic fixture adapter and automatic experimental comparison |
| `--check SCRIPT` | Required shell check; repeat to run several in order |
| `--timeout-secs N` | Per agent/check process deadline; default 300, range 1–86400 |
| `--keep` | Keep the original run's Reality; experiment trials are still discarded |

All required checks must pass. An agent exiting zero does not establish task success. A failed agent with passing checks can have a successful task evaluation. No checks means `inconclusive`; the CLI preserves its historical process-based exit code in that case. Normal failed checks do not skip later checks; timeouts or cancellation do.

Output identifies the process result, evaluation, source Experience, and artifacts. The fixture also reports its Candidate, trials, conclusion, and updated Lesson. Successful counterfactual support does **not** change the original failed task result or retry the task; that demo intentionally exits 1.

## Evidence and Lesson inspection

```bash
hardknock execution list
hardknock execution show exec-<uuid>
hardknock experience list
hardknock experience show exp-<uuid>
hardknock lesson list
hardknock lesson show lesson-<uuid>
hardknock experiment list
hardknock experiment show experiment-<uuid>
```

Use full IDs returned by the CLI. Executions are raw process records. Experiences include evaluation, context, failure signatures, and durable artifact references. Lesson details include the source Experience, hypothesis ID/provider, scope, evidence IDs, confidence, and revision. Experiment details include both actions, trials, Reality/Experience IDs, state, and conclusion. JSON detail responses retain the complete typed record.

## Manual hypotheses and experiments

```bash
hardknock lesson propose \
  --experience exp-<uuid> \
  --claim 'The baseline script may create conflicting state in this repository' \
  --avoid './agent-script.sh baseline' \
  --prefer './agent-script.sh alternative'

hardknock experiment run --lesson lesson-<uuid>
```

Propose does not execute anything. It creates a scoped Candidate at confidence 0.42. Run reconstructs the source Experience's starting commit and uses its original checks/deadlines. `--avoid` must match the **entire** recorded script after trimming outer whitespace; `--prefer` replaces that script. No interception of commands inside an opaque live agent is attempted. Unsupported replay conditions fail clearly before trials begin.

The CLI does not accept arbitrary Lesson statuses or confidence values. An inconclusive comparison exits 3; support or contradiction exits 0 because the investigation completed. The Lesson's status conveys which conclusion was reached.

## Reality management

```bash
hardknock reality create
hardknock reality list
hardknock reality show r-<uuid>
hardknock reality fork r-<uuid>
hardknock reality diff r-<uuid>
hardknock reality discard r-<uuid>
hardknock reality cleanup
```

Create uses `--repo`; subsequent operations use persisted repository references. Fork recreates the original commit, not the parent's modifications. Diff includes tracked/nonignored files against that commit, with patch bytes in human mode and an artifact reference in JSON mode. Saved diffs remain available after disposal.

Discard removes only the selected managed worktree, retains records/artifacts, and refuses active leases or unsafe paths. Cleanup removes unlocked automatic-run orphans, skipping manual, kept, or capture-failure Realities. Stop abandoned commands before cleanup; escaped descendants and external effects cannot be undone.

## Global flags and JSON

| Flag | Behavior |
| --- | --- |
| `--repo PATH` | Source Git repository; default current directory |
| `--home PATH` | Dedicated storage directory outside the source; overrides `HARDKNOCK_HOME` |
| `--json` | One JSON result on stdout; newline-delimited JSON diagnostics on stderr |
| `--quiet` | Suppress stdout, not warnings/errors; conflicts with JSON and verbose |
| `--verbose` | Debug tracing on stderr; does not dump argv/environment |
| `--no-emoji` | Remove emoji from human output |

Global flags can precede or follow subcommands. `--help` and `--version` remain text. Child logs are captured to files, never mixed into CLI stdout. Runtime/usage errors leave stdout empty and emit an `error` object on stderr:

```json
{"event":"error","message":"Record exp-… not found","exit_code":2}
```

The existing single-result JSON contract is preserved, rather than introducing a streaming stdout protocol. `run_completed` retains `execution`/`reality` and adds `experience`, nullable `lesson`, and nullable `experiment`. Other events include `experience(s)`, `lesson(s)`, `experiment(s)`, `experiment_completed`, `execution(s)`, `reality`, `realities`, `reality_diff`, and `cleanup_completed`. Parentheses here denote singular/plural event names, not literal syntax. Diagnostics include `isolation_warning`, `error`, and optional structured tracing.

On partial experiment runtime failure, inspect `experiment list/show` and `experience list`: evidence may have been persisted even though stdout contains no success response. The error identifies the Experiment and any retained Reality. Schemas remain pre-alpha and may evolve through migrations.

## Storage

```text
~/.hardknock/
├── hardknock.db
├── artifacts/
│   └── exp-<uuid>/
│       ├── agent/{stdout.log,stderr.log}
│       ├── check-0/{stdout.log,stderr.log}
│       ├── check-1/{stdout.log,stderr.log}
│       ├── agent.diff.patch
│       ├── diff.patch
│       ├── execution.json
│       └── metadata.json
├── realities/
├── locks/
└── logs/
```

Each original run and trial has its own Experience artifact directory. Existing `exec-<uuid>` directories remain readable. Hash references retain the fields `blake3`/`bytes` and add `kind`. The final diff includes check effects; the agent diff does not. The Experience mirror is not self-hashed.

The data directory is owner-only and SQLite is owner read/write; WAL/SHM sidecars may appear. `logs/` is reserved and tracing currently goes to stderr. General redaction, artifact quotas/garbage collection, and TOML configuration are not implemented. Tasks, scripts, and logs can contain secrets; review before sharing.

## Exit codes and cancellation

| Code | Meaning |
| --- | --- |
| `0` | Management succeeded, checks passed, or a classified experiment completed; with no checks, agent exited zero |
| `1` | Task evaluation failed/timed out; with no checks, agent failed/timed out |
| `2` | Usage or runtime/internal failure |
| `3` | Explicit experiment completed inconclusively |
| `4` | Reserved for future policy/invariant failures |
| `5` | Intervention required, invalid replay conditions, active lease, SIGINT/SIGTERM interruption |
| `6` | Reserved for future candidate selection |

Cancellation terminates the active process group, records available evidence, and skips pending work. Normal failures, timeouts, and interruptions clean up. Capture/storage failures preserve the affected Reality. SIGKILL/power loss can leave running Experiments and orphan worktrees; automatic experiment resumption is deferred.

## Deferred commands

Retrieval, retry/`try`, `why`, reflexes, recovery, chaos, skill synthesis, arbitrary action interception, named vendor adapters, and `Validated` promotion are not implemented. See [the next-phase plan](roadmap.md#exact-next-phase-plan).
