# Agent experiments: stop guessing, try it

V0.4 accepts explicit alternatives, runs them in disposable Git Realities, evaluates them, and returns evidence. A recommendation never applies a patch, commits, or changes the source checkout on Hardknock's behalf. Candidate commands themselves are trusted code: Git worktrees do not prevent them from reaching the host or source repository.

## Run the deterministic demo

From the Hardknock checkout:

```bash
cargo build --locked
HARDKNOCK_BIN="$PWD/target/debug/hardknock"
DEMO_ROOT="$(mktemp -d)"
cp -R fixtures/strategy-choice "$DEMO_ROOT/project"
git -C "$DEMO_ROOT/project" init -b main
git -C "$DEMO_ROOT/project" config user.name 'Hardknock Demo'
git -C "$DEMO_ROOT/project" config user.email 'demo@example.invalid'
git -C "$DEMO_ROOT/project" add .
git -C "$DEMO_ROOT/project" -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -m 'Strategy fixture'

"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/project" try \
  --agent test-agent \
  --candidate 'direct=direct-upgrade' \
  --candidate 'staged=staged-upgrade' \
  --check './test.sh'
```

The direct strategy updates the API but leaves its consumer incompatible. The staged strategy updates both. One required check command fails for direct and passes for staged. Both agent processes exit successfully; the evaluator distinguishes their task outcomes. Expected result: **CONTROLLED**, staged recommended, two immutable Experiences, one **Candidate** Lesson, two discarded Realities. No package manager, model, or network is needed.

The question is an optional positional argument. Explicit `--candidate NAME=STRATEGY` is required; two ambiguous positional alternatives are not supported. Without `--agent`, each value is a shell script. Each shell entry runs in its own `/bin/sh -c` process, stopping that candidate on the first unsuccessful process. Shell variables and `cd` do not carry between entries. Agent tasks are passed as literal argv elements to a configured executor.

```bash
hardknock --repo /path/to/clean/project try 'Which migration passes?' \
  --candidate 'direct=./migrate.sh direct' \
  --candidate 'staged=./migrate.sh staged' \
  --check './test.sh' --budget-realities 2 --budget-duration 5m

hardknock --json --repo /path/to/clean/project try \
  --agent codex --candidate 'direct=try the direct upgrade' \
  --candidate 'staged=try a staged upgrade' --check './test.sh'
```

The Codex example uses the installed `codex exec -- {task}` command and its existing native settings. Hardknock does not add approval bypass, automatic approval, or sandbox-relaxation flags. Noninteractive native approval requirements can stop a candidate. No live Codex/Claude experiment success is claimed for this release.

### Configured agents

`test-agent` and the two confounded fixture agents are deterministic built-ins. `codex` has the explicit default above. Other agents require a trusted local template, for example:

```toml
[experiments.agents.claude]
command = "claude -p -- {task}"
environment = "inherited"

[experiments.agents.my-agent]
command = "/absolute/path/to/my-agent --task {task}"
environment = "controlled"
version = "locally-verified-version"
```

Templates need exactly one complete `{task}` argument. A program must be absolute or resolvable on PATH; no implicit shell is added. Templates are user-controlled code and must preserve the native permission boundary. A `model` label, when supplied, must describe the model actually selected by that template; this is not automatic model routing. Explicit candidate model labels conflicting with configuration are rejected. Native settings, dependent libraries, remote model versions, and service state are not frozen, so nonfixture agents are at most PartiallyControlled.

## Structured request

`ExperimentRequest` contains:

| Field | Meaning |
| --- | --- |
| `id`, `session_id`, `created_at` | Stable typed request UUID, opaque session ID, timestamp |
| `question`, `hypothesis` | Question and optional proposed explanation |
| `candidates` | Unique candidate UUID/name, description, execution, optional expected outcome |
| `starting_state` | Canonical `StateRef`, optional expected fingerprint/parent Reality, snapshot source |
| `evaluator` | Identical explicit required check commands, at most 16 |
| `budget` | Requested ceilings; `effective_budget` records configuration-clamped ceilings separately |
| `requested_by`, `origin`, `intent` | Provenance; requester identity is distinct from each executing agent |
| `criteria` | Success requirement; optional duration/text-diff tie breakers |
| `capabilities` | Declared Reality-only scope, network intent, external-effect declarations |

IDs use `request-<uuid>`, `candidate-<uuid>`, `experiment-<uuid>`, `r-<uuid>`, and `exp-<uuid>`. There are 1–32 candidates per request, a 128 KiB request limit, a 4 KiB question limit, and unique bounded names. One candidate is useful for replay, but cannot produce a comparative winner. `Shell {commands}` and `AgentTask {prompt, agent?}` are implemented. Script-specific execution is deferred. Custom comparison checks, map-boundary, recovery and lesson-test intents are rejected by this strategy entry point; use the existing dedicated engines.

## During an integrated session

Claude's hook context and Codex's turn context describe the same helper:

```bash
hardknock --home /same/hardknock/home try --session hk-s-<session> \
  --candidate 'direct=./migrate.sh direct' \
  --candidate 'staged=./migrate.sh staged' --check './test.sh'
```

Use the session ID supplied in context. The daemon must be running; the helper does not silently create an unregistered session. The helper sends `experiment_requested`, polls cursor-based progress, and returns structured evaluation evidence. `--agent` selects task prompts instead of shell scripts. Both adapters use identical request/result semantics; there is no MCP server or vendor-specific orchestration engine.

The Bridge selects the session's recorded Git commit. **It does not snapshot the running agent, its conversation, uncommitted files, ignored dependencies, or process memory.** This fallback is disclosed in the acceptance response. To include new files or edits, explicitly establish a clean committed starting snapshot and refresh context between runs. Automatic experiment initiation is off and unsupported; the agent must deliberately request the experiment.

The local token authenticates an OS user, not an agent identity. Session checks prevent accidental cross-session access, not impersonation by another process with the same token. Raw operational candidate prompts, commands, checks and diffs are persisted in the private experiment store for reproducibility. They are not opaque conversation transcripts; do not put secrets in them. Bridge result summaries omit raw outputs and task prompts. Ordinary runner artifacts have no general redaction or disk quota.

## Inspect, replay, fork, cancel, export

```bash
hardknock experiment list --agent claude
hardknock experiment show experiment-<uuid>
hardknock why --experiment experiment-<uuid>
hardknock experiment replay experiment-<uuid> --all
hardknock experiment replay experiment-<uuid> --candidate staged
hardknock experiment fork experiment-<uuid> --candidate 'third=another strategy'
hardknock experiment cancel experiment-<uuid>
hardknock reality tree
hardknock reality export r-<uuid> --patch staged.patch
```

Replay and extension create new request/candidate/experiment IDs and immutable lineage. They use the original commit, remeasure local runtime fingerprints, and report detected environment changes. They do not claim to recreate inherited agent settings. Extension preserves the original candidates and appends alternatives of the first candidate's execution type; the full new set must fit the budget. A one-candidate replay has no recommendation. Previous evidence is never rewritten.

Human runs print progress to stderr; JSON runs return one final object. `experiment show` includes persisted progress and any finished partial candidates. For new experiments the JSON envelope is `event: experimentation`, with `result.kind: experiment` and `result.experiment.result` holding evidence. `experiment list` preserves the legacy `experiments` array and adds `strategy_experiments`. Legacy lesson experiment `show` keeps its earlier shape.

Exit 0 means the experiment completed, including ties or all-failed evaluations; inspect candidate outcomes. Rejection/infrastructure failure exits 2; cancelled experiments exit 5. Native helper cancellation polls for terminal confirmation. `experiment cancel` itself acknowledges the request; inspect `show` for completion.

Cancellation stops ordinary child process groups, awaits launched workers, skips pending checks/candidates, captures interrupted Experiences, and discards managed trial worktrees. Session end cancels its agent-origin requests by default. User-origin CLI requests are independent. Duration cancellation includes preparation and execution; synchronous Git/bookkeeping and final evidence capture/cleanup can exceed the deadline. SIGKILL/power loss is different: orphan processes and nonterminal rows can remain. Automatic crash resumption is not implemented; stop abandoned processes before `reality cleanup` and use replay for new evidence.

Saved patches are the **agent diff before evaluator effects**. Export works after a Reality is discarded, verifies its artifact hash, writes atomically, and refuses to overwrite an existing destination. Patch export does not apply or commit anything. Reality commit/adoption is deferred; there is no unsafe fast-apply path needing an implicit conflict resolution policy.

See [budgets](experience-budget.md), [quality](experiment-quality.md), and the [implementation report](implementation-v04.md).
