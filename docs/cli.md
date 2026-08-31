# CLI reference

## Behavioral contracts and assurance

```text
hardknock contract list
hardknock contract show <id-or-name>
hardknock contract validate <id-or-file>
hardknock contract history <id-or-name>
hardknock contract diff <id-or-name> --from 1 --to 2
hardknock contract register .hardknock/contracts/deploy.toml --skill deploy

hardknock skill certify deploy --profile basic-behavior-v1 --dry-run
hardknock skill certify deploy --profile resilience-basic-v1

hardknock assurance show deploy
hardknock assurance gaps deploy --profile resilience-basic-v1
hardknock assurance history deploy
hardknock assurance diff <certificate-a> <certificate-b>
hardknock assurance export deploy --profile basic-behavior-v1 --output deploy.hkcert
hardknock assurance verify deploy.hkcert
hardknock assurance revoke <certificate-id> --reason "critical regression"
```

`contract validate FILE` is read-only. `contract register` creates a new
immutable revision and binding. `skill certify --dry-run` never persists a
manifest or certificate. A non-dry run persists only when the recommendation
is `eligible`; blocked and additional-evidence results use distinct exit codes.
Certification never runs curriculum trials or commits effects automatically.

## Capability-isolated execution (V0.9)

```text
hardknock capability list
hardknock capability show <profile>
hardknock capability validate <json-or-toml>
hardknock capability explain <reality-id> --request '<CapabilityRequest JSON>'
hardknock capability explain <micro-sandbox-id>
hardknock capability audit [--reality <id>]
hardknock capability diff <left-profile> <right-profile>
hardknock capability revoke --reality <id> <network|process|credentials|effects>
hardknock capability benchmark [--output <json>]

hardknock --repo <repo> reality create --provider container [--profile coding-offline] [--image <reference>]
hardknock reality inspect <id>
hardknock reality execute <id> -- <argv...>
hardknock reality freeze <id>

hardknock --repo <repo> run --provider container [--capabilities <profile>] [--image <reference>] <agent options> <task>
```

Container runs default to `coding-offline` and `debian:bookworm-slim`. Git worktrees reject `--capabilities`/`--image` because they cannot enforce the manifest. A container run records one isolated Experience; V0.9 rejects `--retry-with-experience` and a non-default retry count because retry/reflection subprocesses are not yet routed through the execution proxy.

`reality inspect` reports provider security claims, manifest hash/revision, image digest, runtime metadata, running processes, pending Effects, credentials, violations, network policy, and diff. `freeze` stops new process execution, revokes token/credentials, prevents new Effect preparation, and preserves inspection state. Resume is not implemented.

Inside a container image that includes the separate `hk-effect` binary, `hk-effect propose`, `hk-effect status`, and `hk-effect discard` use `/run/hardknock/bridge.sock` and the Reality token. The default Debian image does not bundle Hardknock. There is deliberately no `hk-effect commit`. Bridge must be running to publish the per-Reality relay. See [capabilities](capabilities.md) and [container Realities](container-realities.md).

## Governed effects (V0.8)

```text
hardknock effect list [--reality <id>]
hardknock effect show <effect-id>
hardknock effect propose --kind <kind> --operation <operation> --target <scheme-uri> [--payload <json>] [--reality <id>] [--prepare]
hardknock effect prepare <effect-id>
hardknock effect commit <effect-id> [--yes | --authorization-file <json>]
hardknock effect discard <effect-id>
hardknock effect compensate <effect-id> --yes
hardknock effect reconcile <effect-id>
hardknock effect capabilities
hardknock effect orphans
hardknock effect cleanup
hardknock effect plan-create --effect <id>... [--dependency BEFORE:AFTER] [--compensate-on-failure]
hardknock effect plan-commit <plan-id> --yes
hardknock benchmark transactional-effects [--output <json>]
```

`propose --prepare` is a convenience for two guarded lifecycle operations, not commit. Mock fixture setup is available through `effect fixture-set` and `effect fixture-show`; `--inject-fault` is limited to deterministic adapters. `reality show` includes effect counts, and `reality discard` refuses to remove the worktree if attached effect cleanup is incomplete.

Human output includes an explicit “prepared only” message. JSON retains the complete Effect, preview, receipts, and event stream. See [effects](effects.md).

## V0.7 federation

```text
peer list | add --name NAME --public-key FILE | show PEER | trust PEER | block PEER | remove PEER
federate status
federate export (--lesson ID | --skill NAME | --reflex ID | --external ID) [--output FILE | --dry-run]
federate import FILE
federate test FEDERATED_ID [--check SCRIPT ...]
federate promote FEDERATED_ID --experience EXPERIENCE_ID
federate publish OBJECT --target DIRECTORY [--namespace team/NAME] [--dry-run]
federate search [--kind KIND] [--marker MARKER]
federate search --repository DIRECTORY [--producer NODE] [--task-family FAMILY] [--marker MARKER]
federate backlog | audit | compare LEFT RIGHT
provenance OBJECT
conflict list | show ID | test ID [--check SCRIPT ...]
profile federation
benchmark federation [--output FILE]
```

Export requires explicit publication and refuses overwrite. `--dry-run` returns the complete redacted, signed payload without writing or publishing it. Raw artifacts remain excluded; `--include-artifacts` is deliberately refused in this version. Import rejects invalid signatures, hash/ID mismatches, unsafe paths, bad references, excessive depth, and oversized files. A bundle from an unknown signing key is surfaced as `unknown_key`; add its public key as a peer to establish a local administrative relationship.

Import creates new `federated-<uuid>` IDs while retaining origin node/object/bundle mappings. External items remain advisory. `federate test` is an explicit request to execute the imported Lesson's baseline and alternative as trusted local shell candidates in Git worktrees; the normal isolation warning applies. A supporting test reaches `locally_supported`. Only a separate later successful Experience in a compatible context can be supplied to `promote` for `locally_validated`.

`lesson search --include-federated` appends separately labeled external candidates after local results. Federated evidence never outranks local validated evidence in the combined display. Remote Reflexes show requested and effective behavior separately; effective behavior is always `ADVISE` on import.

## V0.5 curriculum

```text
curriculum plan (--skill NAME | --task-family NAME) [--profile resilience-basic] [--budget 5] [--replicate]
curriculum run ID
curriculum list | show ID | why ID | report ID | cancel ID
skill harden NAME [--profile PROFILE] [--budget N] [--replicate]
skill package NAME [--profile PROFILE]
task-family register NAME --experience ID [--experience ID ...]
task-family list | show NAME
```

Planning creates no Realities. `harden` explicitly plans and runs. JSON uses `event: curriculum` and a typed `result`; full plans retain policy explanations, engine/evidence references, coverage/maturity before/after, reservations and recorded usage. Normal `skill show` displays the package; `skill list` retains the registered Skill status plus latest derived metadata. `--budget` counts curriculum trial slots, with controls/response arms additionally charged to Reality and agent-run limits. An exact replication is allowed with `--replicate`; all other policy gates remain in force.

Exit codes: planned/completed 0, partially completed 3, cancelled 5; rejected input/policy errors use 2. A tested failure can be a completed, useful curriculum observation. Cancellation is a request; inspect terminal status for cleanup confirmation. [Complete configuration, demo and benchmark](curriculum.md).

## V0.4 strategy experiments

`hardknock try [QUESTION] --candidate NAME=STRATEGY --candidate NAME=STRATEGY --check COMMAND` runs shell alternatives; `--agent NAME` interprets them as task prompts. `--session ID` uses the active native Bridge contract. Budget flags are `--budget-realities`, `--budget-agent-runs`, `--budget-duration` (`500ms`, `30s`, `5m`), and shell-only `--max-commands-per-reality`. Optional tie breakers are `--minimize-diff-size` and `--minimize-duration`; `--allow-network` declares advisory intent, not isolation.

Inspection adds `experiment list --agent`, `experiment show`, `why --experiment`, `experiment replay [--all | --candidate NAME]`, `experiment fork --candidate NAME=STRATEGY`, `experiment cancel`, `reality tree`, and `reality export ID --patch PATH`. Export refuses overwrite and never applies changes. No `reality commit`/adopt command is provided.

New experiment responses use `event: experimentation` and a tagged `result`. A completed experiment exits 0 even if candidates fail or tie; inspect each evaluator outcome. Rejected/failed experiments exit 2, cancelled experiments 5. `experiment list` retains legacy `experiments` and adds `strategy_experiments`; legacy `show` JSON is preserved. Human progress goes to stderr; JSON output remains one final result. See [the runnable guide](agent-experiments.md) for examples, configuration, and safety boundaries.

The first empirical transfer loop is implemented for Linux/macOS. Build from source with stable Rust, Git, and a C compiler:

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

`--script` and checks use `/bin/sh -c`. The script is executed verbatim; the task string is a recorded goal, **not** substituted into the script. Scripted runs use the controlled environment documented in [experiments.md](experiments.md#equivalence-and-limits). All adapters are noninteractive with stdin closed. Native adapters use the separate integration commands below; `run --agent claude` is not a supported shorthand.

| Flag | Behavior |
| --- | --- |
| `--agent-command TEMPLATE` | Opaque executable/argv adapter |
| `--script SCRIPT` | Explicit replayable shell script |
| `--agent test-agent` | Deterministic fixture adapter and automatic experimental comparison |
| `--check SCRIPT` | Required shell check; repeat to run several in order |
| `--timeout-secs N` | Per agent/check process deadline; default 300, range 1–86400 |
| `--keep` | Retain task-attempt Realities; experiment trials are still discarded |
| `--with-experience` | Opt generic/script adapters into context-file advice |
| `--no-experience` | Disable advice, fixture reflection and retries; audit matches measure repeated mistakes |
| `--retry-with-experience` | Opt into fresh-state retries with applicable supported Lessons |
| `--experience-budget N` | Cap additional paired experiment trials plus retries; initial task is outside this budget |
| `--max-retries N` | Budget 0–10, default 1; only active with the retry flag |
| `--action SCRIPT` | Proposed action for relevance; repeatable |
| `--min-relevance N` | Retrieval minimum, default 0.50 |
| `--recommend-threshold N` | Delivery threshold, default 0.70 |
| `--strong-threshold N` | Strong relevance threshold, default 0.85 |

All required checks must pass. An agent exiting zero does not establish task success. A failed agent with passing checks can have a successful task evaluation. No checks means `inconclusive`; the CLI preserves its historical process-based exit code in that case. Normal failed checks do not skip later checks; timeouts or cancellation do.

Output identifies process result, evaluation, Experience, applications, repeated mistakes, and artifacts. Delivered advice appears on stderr before execution. The fixture reports any new hypothesis/trials, updated Lesson and retry results. Counterfactual support does not change the original task result: without `--retry-with-experience`, that failed run exits 1. With retries, the final attempt determines the exit code; original evidence stays unchanged. `--keep` retains each task attempt, not experiment trials.

```bash
hardknock run --agent test-agent --check './test.sh' \
  --retry-with-experience --max-retries 1 'upgrade dependencies'
```

Timeout, interruption, inconclusive evaluation, or missing applicable advice do not trigger automatic retries. Failed evaluated tasks retry within the explicit budget. Experiment trials are additional executions, not counted as retries. `--no-experience` conflicts with advice/retry flags. See [retrieval](retrieval.md).

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

## Search, retest, retirement, and explanation

```bash
hardknock lesson search --repo /path/to/B --action 'npm install'
hardknock lesson search --include-candidates --action './agent-script.sh baseline'
hardknock lesson test lesson-<uuid> --repo /path/to/related-fixture
hardknock lesson test lesson-<uuid> --repo /path/to/repo \
  --check './test.sh' --task 'revalidate after a dependency change'
hardknock lesson retire lesson-<uuid> --reason 'superseded after dependency change'
hardknock lesson list --include-retired
hardknock why
hardknock why --experience exp-<uuid>
hardknock status
```

Search accepts the same relevance thresholds as run and explains matches/exclusions. Task text (`--task`) is recorded but not scored. Candidates are debugging results only and are never injected.

`lesson test` makes a new paired Experiment in the target snapshot using the explicit avoid/prefer scripts. Scope must match. Supplied checks take precedence; fixture repositories default to `./test.sh`, while others require `--check`. A supporting retest alone is not an observed application and cannot validate. A controlled contradiction lowers confidence and excludes the Lesson from default retrieval. Retesting retired Lessons is refused.

Retirement accepts an optional reason and records time/reason in a new revision, preserving evidence. Repeating retirement returns the existing revision without replacing its reason. Retired Lessons need `--include-retired` for listing and are never injected.

`why` selects the latest applied influence, falling back to the latest Experience if none exists. It distinguishes `Observed` from `SelfReported`, shows context, action, Lesson revision at use, current status/confidence, origin and experiments. `--experience` selects a specific run, including ignored/control cases. `status` reports counts, not a benchmark. All support `--json`.

## Reality management

```bash
hardknock reality create
hardknock reality create --provider container --profile coding-offline
hardknock reality list
hardknock reality show r-<uuid>
hardknock reality inspect r-<uuid>
hardknock reality execute r-<uuid> -- /bin/sh -lc 'make test'
hardknock reality freeze r-<uuid>
hardknock reality fork r-<uuid>
hardknock reality diff r-<uuid>
hardknock reality discard r-<uuid>
hardknock reality cleanup
```

Create uses `--repo`; subsequent operations use persisted repository references. Fork recreates the original commit, not the parent's modifications. Diff includes tracked/nonignored files against that commit, with patch bytes in human mode and an artifact reference in JSON mode. Saved diffs remain available after disposal.

Discard removes the selected provider resources while retaining records/artifacts, and refuses active leases or unsafe paths. For a container it also removes its internal network, token, relay, credentials, and underlying managed worktree. Cleanup removes unlocked automatic-run orphans, skipping manual, kept, or capture-failure Realities. Stop abandoned commands before cleanup; escaped descendants and external effects cannot be undone.

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

The existing single-result JSON contract is preserved, rather than introducing a streaming stdout protocol. `run_completed` retains `execution`/`reality`, `experience`, nullable `lesson` and `experiment`, and adds `retries`, `retry_stop_reason`, and `interrupted`. Each retry carries its own execution, Reality, and Experience. Other events include `experience(s)`, `lesson(s)`, `experiment(s)`, `experiment_completed`, `execution(s)`, `reality`, `realities`, `reality_diff`, and `cleanup_completed`. Parentheses here denote singular/plural event names, not literal syntax. New result events are `lesson_search`, `why`, and `status`. Diagnostics include `isolation_warning`, `relevant_experience` (before execution), `error`, and optional structured tracing.

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
│       ├── context.md / context.json  # when experience enabled
│       ├── agent-usage.json            # valid opaque-agent report
│       ├── execution.json
│       └── metadata.json
├── realities/
├── fixtures/                      # versioned bundled sources for replay
├── locks/
└── logs/
```

Each original run and trial has its own Experience artifact directory. Existing `exec-<uuid>` directories remain readable. Hash references retain the fields `blake3`/`bytes` and add `kind`. The final diff includes check effects; the agent diff does not. The Experience mirror is not self-hashed.

The data directory is owner-only and SQLite is owner read/write; WAL/SHM sidecars may appear. `logs/` is reserved and tracing currently goes to stderr. Bridge/native capture adds bounded redaction and `config.toml`; generic runner logs are not retroactively sanitized. Artifact quotas/garbage collection remain deferred. Tasks, scripts, and logs can contain secrets; review before sharing.

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

Autonomous skill synthesis and arbitrary action interception remain deferred. `try` and `benchmark longitudinal` are implemented; see [agent experiments](agent-experiments.md) and [persistent development](development.md). Native integration commands remain a preview with live acceptance pending. The generic adapter has a tested context-file contract; see [agent integration](agent-integration.md).

## V0.10 tools and attestations

```bash
hardknock tool list [--include-disabled]
hardknock tool show NAME_OR_ID
hardknock tool verify NAME_OR_ID
hardknock tool validate hardknock-tool.toml
hardknock tool register hardknock-tool.toml
hardknock tool disable NAME_OR_ID
hardknock tool audit [--sandbox ID]
hardknock tool benchmark [--output FILE]
hardknock tool run NAME [--reality ID] [--runtime container|host]
hardknock tool run NAME --explain-capabilities [--reality ID]
hardknock capability explain MICRO_SANDBOX_ID
hardknock attestation list
hardknock attestation show ID
hardknock attestation verify ID
hardknock attestation replay ID
```

The default runtime is a disposable container and does not silently downgrade.
`--runtime host` requires `--allow-host-fallback` and records `Observed`,
non-isolated evidence. Attestation replay reports when the original input was
not retained because only its hash is available.

## V0.2 resilience commands

```text
chaos run --fixture KIND --profile PROFILE [--trials N] [--max-duration SECONDS]
chaos run --agent test-agent --check SCRIPT --perturb CONDITION TASK
chaos run --command SCRIPT --check SCRIPT --perturb CONDITION TASK
chaos run --skill NAME_OR_ID --perturb-sweep delay=0,100,500,1000,2000
chaos list | show ID | report ID
envelope list | show ID
reflex list | show ID | test ID [--perturb CONDITION] | enable ID | disable ID
recovery list | show ID | test ID
skill list | show NAME_OR_ID | register NAME --experience ID
```

Kinds: `retry-resilience`, `stale-credential`, `config-drift`. Profiles: `latency`, `command-failure`, `config-drift`, `credential`. Conditions: `delay:100ms`, `command-failure:once|N|always`, `env:KEY=VALUE`, `file:relative-path=content`. Repeat `--perturb` for separate campaign trials; in `reflex test`, repeated values form one compound paired condition. Read [the chaos guide](chaos.md) for defaults, limits, exact behavior, JSON events, and exit semantics. Bundle mode uses a managed fixture source, not `--repo`.

`why` additionally explains historical Reflex match → Lesson → chaos Trial → source Experience, including scope, precursor, confidence, and test-only/active status. `status` includes new resource counts. Resilience commands emit `event: resilience` with a typed `result`; campaign progress is NDJSON on stderr, leaving stdout as one final object. Fixture action logs are in `agent-N/`, rather than the single-process `agent/` directory.

## V0.3 Bridge and integrations

```bash
hardknock bridge start [--foreground] [--tcp PORT]
hardknock bridge status
hardknock bridge sessions
hardknock bridge inspect hk-s-<id>
hardknock bridge stop
hardknock integrate list
hardknock integrate doctor
hardknock agent capabilities
hardknock events tail [--follow] [--after SEQUENCE]
hardknock integrate claude install [--config /absolute/settings.json]
hardknock integrate claude uninstall
hardknock integrate hermes install [--config /absolute/plugin-directory]
hardknock integrate openclaw install [--config /absolute/plugin-directory]
hardknock integrate codex check [--executable /path/to/codex] [--allow-untested]
hardknock --repo /path/to/project integrate codex run [--resume THREAD] 'task'
```

Brackets indicate optional arguments. Integration commands return JSON even without `--json`. `doctor` verifies managed files, Bridge reachability, local configuration, and Codex version/schema/initialization if installed; it does not claim native plugin enablement. Codex run returns a queued recording ID; task evaluation completes asynchronously. Poll `run_status` with `bridge call` or inspect event telemetry. Successful submission is not successful evaluation.

`integration-event --agent claude` consumes one native hook payload on stdin. `bridge call` consumes one authenticated-transport payload without credentials in input. `hardknock-test-adapter` accepts JSONL events with `HARDKNOCK_HOME` set; all session/action/run IDs remain explicit. See the [integration guide](integrations.md) for configuration, privacy and exact capability limits.

## V0.6 development commands

```text
profile show | rebuild | snapshot | gaps [--agent KIND | --task-family NAME | --shared]
  [--agent-version VERSION] [--model MODEL] [--since DATE|30d | --last-days N | --last-experiences N]
profile history [--agent KIND | --task-family NAME | --shared]
profile compare [subject flags] --from DATE_OR_SNAPSHOT --to DATE_OR_SNAPSHOT
profile export [profile flags] --output NEW_JSON_FILE
growth [subject flags]
timeline [--skill NAME | --lesson ID] [--agent KIND] [--since DATE|30d] [--limit N]
episode start NAME [subject flags] | finish ID | list
experience health [subject flags] | maintain [subject flags]
revalidation list | run ID
lesson history ID
skill history NAME | revise NAME --experience ID
skill package NAME [--profile PROFILE]
skill package history NAME | diff NAME --from N --to N
skill package export NAME [--revision N] --output NEW_JSON_FILE
benchmark longitudinal [--output NEW_JSON_FILE] | list
doctor
```

The global `--repo` selects the default Repository subject. Agent subjects default to the whole local store. Health/maintain need a clean repository context; read-only profile/history commands do not. JSON uses `event: development` with a `result.kind` discriminator. `doctor` is local evidence/database health; `integrate doctor` still diagnoses native integrations. [Full semantics and examples](development.md).
