# Controlled counterfactual experiments

An **Experience** is an immutable observation. A **Hypothesis** is a proposed explanation. A **Lesson** is a scoped, revisable interpretation. An **Experiment** compares explicit baseline and alternative scripts from equivalent recorded starting conditions.

Counterfactual support means changing the suspected variable changed the measured outcome in this comparison. **Counterfactual support is not universal causal proof.** A command replacement can alter many internal actions, and an evaluator only measures its configured checks.

## Run the offline demo

From the Hardknock repository, initialize separate fixture repositories and a shared data home:

```bash
cargo build --locked
HARDKNOCK_BIN="$PWD/target/debug/hardknock"
DEMO_ROOT="$(mktemp -d)"
for fixture in pnpm-workspace-conflict pnpm-workspace-transfer npm-ordinary pnpm-workspace-contradiction; do
  cp -R "fixtures/$fixture" "$DEMO_ROOT/$fixture"
  git -C "$DEMO_ROOT/$fixture" init -b main
  git -C "$DEMO_ROOT/$fixture" config user.name "Hardknock Demo"
  git -C "$DEMO_ROOT/$fixture" config user.email "demo@example.invalid"
  git -C "$DEMO_ROOT/$fixture" add .
  git -C "$DEMO_ROOT/$fixture" -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -m "Local deterministic fixture"
done

# A: fail, compare strategies, then retry with the supported Lesson.
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/pnpm-workspace-conflict" run \
  --agent test-agent --check './test.sh' --retry-with-experience --max-retries 1 \
  'upgrade demo dependencies'

# B control: expected exit 1. No source reset is needed between runs.
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/pnpm-workspace-transfer" run \
  --no-experience --agent test-agent --check './test.sh' 'upgrade service and worker' || test "$?" -eq 1

# B with advice: expected exit 0; Lesson becomes Validated at 0.90.
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/pnpm-workspace-transfer" run \
  --agent test-agent --check './test.sh' 'upgrade service and worker'

"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/pnpm-workspace-transfer" lesson search --action 'npm install'
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" why
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" status
```

Fixture A's `packages/demo` differs from B's `packages/service` and `packages/worker`, their versions, task output, and checks. Both have a pnpm workspace marker and a version-2 fixture-family tag. The agent parses `.hardknock/context.md`; a supported recommendation changes its strategy. The normal `run` mode emits `RETRIEVED` and `APPLIED`/`IGNORED` IDs, followed by an action trace.

Baseline simulates `npm install`, creates conflicting state, and exits zero. The required check fails. Alternative simulates `pnpm install` and passes. Neither package manager is invoked and no dependency is downloaded.

A creates four immutable Experiences: original failure, baseline failure, alternative success, successful retry. One hypothesis and one paired Experiment support the Lesson at 0.78. Its same-tree retry does not validate. Without the retry flag A creates three Experiences and exits 1, preserving the earlier workflow.

B control adds one failed Experience and one audit-only repeated mistake, without advice/reflection/retry. B advised run adds one successful, observed application and no repeated mistake. The distinct tree validates the Lesson at 0.90. All six Realities are discarded; their evidence and Lesson revisions remain. This one designed transfer comparison is not a general performance estimate.

### Irrelevance and contradiction

```bash
# C: npm is appropriate; the pnpm Lesson must be excluded.
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/npm-ordinary" run \
  --agent test-agent --check './test.sh' 'install ordinary npm app'

# Substitute the actual Lesson ID printed by A/B.
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/pnpm-workspace-contradiction" \
  lesson test lesson-<uuid>
```

C lacks the pnpm workspace marker/family tag: npm succeeds with no injected Lesson. D is a legacy fixture requiring npm-compatible output despite the pnpm marker. Retesting the matching Lesson there records baseline success / alternative failure, moves `Validated → Contradicted`, and lowers confidence to 0.20. The completed investigation exits 0 even though its conclusion contradicts the Lesson. It does not retire it. All prior support and the original Experiment remain intact.

`lesson test` can also use B for a new supporting paired comparison. It starts from the target's recorded clean state, checks target applicability, and records target task/context/checks in the plan's `retest` field. Both trials share that target state and controlled environment. It does not claim to reproduce the origin's different repository contents. Outside fixtures, provide explicit `--check` commands. It cannot retest opaque internal actions or retired Lessons.

## Manual workflow

Use `run --script` with explicit required checks, then `lesson propose --experience ... --avoid ... --prefer ...` and `experiment run --lesson ...`. The entire recorded script is the replacement unit. File-operation and custom ActionPatterns are representable but not executable mutations. Proposals from opaque generic-agent runs are inspectable Candidates; attempting to replay them fails rather than guessing their internal actions.

The deterministic reflection provider recognizes only fixture A; B/C/D do not invent new hypotheses automatically. Manual proposals work for other scripts. External-command/LLM reflection is deferred; no API call is needed to test the empirical loop.

## Equivalence and limits

Before each trial, the runner verifies:

- The full recorded Git commit and tree, with no starting worktree diff.
- The fixture files/scripts, because they are part of that committed tree.
- The source and current controlled-environment fingerprint.

Scripted source runs, all checks, and both trials use `env_clear` followed by:

| Fact | Value |
| --- | --- |
| PATH | `/usr/bin:/bin` |
| LANG / LC_ALL | `C` |
| TZ | `UTC` |
| HOME / PWD | This Reality's root |
| Additional fingerprint inputs | Environment policy version, OS, architecture, BLAKE3 of `/bin/sh` |

HOME/PWD are normalized to `$REALITY` for comparison. The actual root is recorded separately. No arbitrary caller secrets are persisted or inherited by the controlled child. Generic agents inherit the environment but are ineligible for replay.

A mismatch raises **“Counterfactual experiment cannot guarantee equivalent starting state.”** Fresh worktrees prevent the baseline's package-lock file from contaminating the alternative. Separate trials, identical checks, and explicit mutation provenance are enforced by the engine/store.

This does **not** freeze clocks, randomness, external files, services, tool binaries other than `/bin/sh`, CPU scheduling, kernel behavior, network responses, or mutable Git configuration. HOME/PWD normalization does not prove equivalence for scripts that depend on their literal absolute path. Write scripts whose relevant inputs are confined to the snapshot and recorded environment. The fixture uses shell builtins and committed local files; arbitrary real-world scripts require additional controls before stronger causal claims are justified.

A Git worktree is not a security sandbox. Host files, credentials reachable through other paths, network, processes, and Git objects/refs remain shared. Only run trusted scripts without irreversible external effects.

## Classification and confidence

| Baseline | Alternative | Conclusion |
| --- | --- | --- |
| Failure | Success | Supports hypothesis |
| Success | Failure | Contradicts hypothesis |
| Failure | Failure | Inconclusive |
| Success | Success | Inconclusive |
| Timeout / interrupted / incomplete | Any | Inconclusive |

The `PairedComparison` policy owns this rule, not CLI output code. Mismatched trial state, environments, or checks cannot produce support. Runtime failures and interrupted investigations do not update Lessons. Completed inconclusive pairs attach neutral evidence while preserving status/confidence.

One support moves a Candidate to `CounterfactuallySupported` at 0.78. A contradiction moves even a Validated Lesson to `Contradicted` at 0.20. Further paired support never silently erases a contradiction or independently validates a Lesson. Distinct observed application is a separate policy: first success 0.90, second distinct context 0.94; see [retrieval](retrieval.md). These are heuristic indicators, not calibrated probabilities. Both trial references form one experiment's evidence; they are not counted as two replications.

## Retention and failure handling

Plans are persisted before trials. Each trial has its own Experience, evaluation, execution, Reality, artifact references, and a `counterfactual_of` link to the source. Retries have `retry_of` links, and observed applications add `transfer_from` provenance. Terminal Experiment and Lesson evidence updates commit together. Store reads reconstruct partial trials from their own immutable rows, even if an Experiment was interrupted before finalization.

Normal failed checks, timeouts, and Ctrl-C retain evidence and remove worktrees. If artifact capture or database persistence fails, the affected worktree is retained to avoid losing the only copy of changes. The error reports its location. This is a deliberate exception to automatic cleanup. Stop abandoned commands before `reality cleanup`; kept/retained worktrees need explicit inspection/discard.

SIGKILL/power loss can leave an Experiment marked running. Inspection and orphan Reality cleanup work; automatic experiment recovery, resumption, artifact pruning, and cross-filesystem transactions are deferred.
