# CLI reference

This page describes **implemented Milestones 0–2**. Requires Git and a current stable Rust toolchain to build; runtime support is Linux/macOS. There is no published release package yet.

```bash
cargo build --locked
./target/debug/hardknock --version
./target/debug/hardknock --help
```

The examples below use `hardknock` as shorthand for the built binary's absolute path or a binary on your `PATH`.

## Run a command

```bash
hardknock --repo /path/to/clean-repository run \
  --agent-command 'sh -c "{task}"' \
  'printf "hello from the Dojo\n"'
```

The source must have a commit and no staged, unstaged, or untracked changes. Hardknock does not auto-commit, stash, or reset your checkout. Ignored files are not copied. Submodules are not supported yet.

`--agent-command` uses shell-style **quoting for argument splitting only**. Exactly one complete argument must be `{task}`. It is replaced after splitting, so task quotes, whitespace, dollar signs, and semicolons remain literal within that argument. There is no implicit shell, variable expansion, pipe handling, or command substitution.

```bash
# A custom agent receives the task as one literal argument.
hardknock run --agent-command 'my-agent --prompt {task}' 'fix the build'

# Shell execution is an explicit choice: the task is then shell code.
hardknock run --agent-command 'bash -c "{task}"' 'echo hello'
```

Use noninteractive flags appropriate for your installed agent. The runner closes stdin. Named `--agent claude`, `--agent codex`, and `--agent test-agent` adapters are **not implemented** in this pass.

| Flag | Behavior |
| --- | --- |
| `--agent-command TEMPLATE` | Required executable/argument template |
| `--timeout-secs N` | Deadline in seconds; default 300, range 1–86400 |
| `--keep` | Retain the worktree for inspection; otherwise save artifacts then discard it |

Human output identifies the process status, exit code, Reality, execution ID, and artifact paths. Child output is captured to files, not streamed or mixed into CLI output. A zero child exit code does **not** establish task success; there is no `--check` evaluator yet.

## Reality commands

```bash
hardknock reality create
hardknock reality list
hardknock reality show r-<uuid>
hardknock reality fork r-<uuid>
hardknock reality diff r-<uuid>
hardknock reality discard r-<uuid>
hardknock reality cleanup
```

Replace IDs with actual values from command output. Create uses `--repo`; other operations use the persisted source repository reference. List includes discarded records so history stays inspectable.

- **Fork** recreates the parent's original commit; it does not copy current changes.
- **Diff** includes tracked changes and nonignored new files. Human mode writes patch bytes; JSON mode returns an artifact reference. A discarded Reality's saved diff is available through its execution record.
- **Discard** explicitly deletes the managed worktree and its uncommitted changes. It is idempotent for already discarded Realities. It retains metadata and artifacts. It refuses active leases, unmanaged paths, and symlink replacements.
- **Cleanup** deletes only unlocked automatic-run Realities left after interrupted cleanup. It skips active runs, manual Realities, `--keep` runs, and Realities retained after capture failures. Stop abandoned processes first: Hardknock cannot detect or undo arbitrary escaped descendants or external effects.

## Execution inspection

```bash
hardknock execution list
hardknock execution show exec-<uuid>
hardknock --json execution show exec-<uuid>
```

Execution records are append-only process observations, not the future Experience abstraction. `show` exposes the task, argv, agent identity, starting state, timing, exit/signal, and hashed stdout/stderr/diff references.

## Global flags and output

| Flag | Behavior |
| --- | --- |
| `--repo PATH` | Source repository, default current directory |
| `--home PATH` | Dedicated storage directory; overrides `HARDKNOCK_HOME` |
| `--json` | One JSON result on stdout; newline-delimited JSON diagnostics on stderr |
| `--quiet` | Suppress normal stdout, but not safety warnings/errors; conflicts with JSON and verbose |
| `--verbose` | Debug logs on stderr, with arguments/environment omitted |
| `--no-emoji` | Remove emoji from human output |

Flags can precede or follow subcommands. No spinners or ANSI status codes are used. `--help` and `--version` retain their normal text output. Runtime/usage errors leave stdout empty and emit an `error` object on stderr in JSON mode. Process failure is a completed run result with a nonzero CLI exit code, not a runtime exception.

```json
{"event":"error","message":"Record r-… not found","exit_code":2}
```

JSON result events include `run_completed`, `reality`, `realities`, `reality_diff`, `execution`, `executions`, and `cleanup_completed`. `run_completed` contains both the Reality and the complete execution record. Diagnostic events include `isolation_warning` and `error`; verbose tracing is also JSON in JSON mode. The schema is experimental.

## Storage and logging

```text
~/.hardknock/
├── hardknock.db
├── artifacts/
│   └── exec-<uuid>/
│       ├── stdout.log
│       ├── stderr.log
│       ├── diff.patch
│       └── metadata.json
├── realities/
├── locks/
└── logs/
```

`HARDKNOCK_HOME` overrides the default. Use a dedicated directory outside the source repository. The directory is created with owner-only access; SQLite is owner read/write. SQLite may create WAL/SHM sidecars. `logs/` is reserved; current tracing goes to stderr.

```bash
HARDKNOCK_HOME=/tmp/hardknock-demo hardknock reality list
RUST_LOG=hardknock=debug hardknock --no-emoji reality list
```

Environment variables are not logged, but the child inherits them. Command arguments, tasks, and captured outputs are raw and can contain secrets. Review artifacts before sharing. Outputs are written directly to disk without a memory-sized buffer or automatic size cap; provision disk space and use a sensible deadline.

Repository/global TOML configuration and automatic artifact garbage collection are deferred. No configuration file is currently read.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Requested management operation succeeded, or process exited zero; no task evaluation yet |
| `1` | Child process failed, received a signal, or exceeded its deadline |
| `2` | Usage error or Hardknock runtime/internal failure |
| `3` | Reserved: experiment inconclusive |
| `4` | Reserved: policy/invariant failure |
| `5` | User intervention required, including dirty input, an active Reality lease, SIGINT, or SIGTERM cancellation |
| `6` | Reserved: no successful candidate |

Ctrl-C and SIGTERM terminate the process group, capture the interrupted execution, and clean up unless `--keep` was requested. Capture/persistence failures preserve trial state and report its location. SIGKILL or power loss cannot run cleanup; inspect `reality list` and use `reality cleanup` only after stopping abandoned commands.

## Planned commands

`--check`, `experience`, `lesson`, `experiment`, `reflex`, `recovery`, `try`, `why`, and chaos operations are not implemented. They are not placeholder commands that silently succeed. See the [roadmap](roadmap.md) for the next implementation steps.
