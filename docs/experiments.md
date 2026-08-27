# Controlled counterfactual experiments

An **Experience** is an immutable observation. A **Hypothesis** is a proposed explanation. A **Lesson** is a scoped, revisable interpretation. An **Experiment** compares explicit baseline and alternative scripts from equivalent recorded starting conditions.

Counterfactual support means changing the suspected variable changed the measured outcome in this comparison. **Counterfactual support is not universal causal proof.** A command replacement can alter many internal actions, and an evaluator only measures its configured checks.

## Run the offline demo

From the Hardknock repository, after building:

```bash
cargo build --locked
HARDKNOCK_BIN="$PWD/target/debug/hardknock"
DEMO_ROOT="$(mktemp -d)"
cp -R fixtures/pnpm-workspace-conflict "$DEMO_ROOT/repo"
git -C "$DEMO_ROOT/repo" init -b main
git -C "$DEMO_ROOT/repo" config user.name "Hardknock Demo"
git -C "$DEMO_ROOT/repo" config user.email "demo@example.invalid"
git -C "$DEMO_ROOT/repo" add .
git -C "$DEMO_ROOT/repo" -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -m "Local deterministic fixture"
"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/repo" run \
  --agent test-agent --check "./test.sh" "upgrade dependencies"
```

The command intentionally exits **1**: the original task still failed. The supported alternative is experimental evidence, not an automatic retry or task completion. Use the printed IDs with `experience show`, `lesson show`, and `experiment show`, passing the same `--home`.

The fixture has a tracked pnpm workspace marker, lockfile, and package. Baseline emits `ACTION shell npm install`, creates a simulated `package-lock.json`, and exits zero. `./test.sh` reports `package_manager_conflict` and fails. Alternative emits `ACTION shell pnpm install`, preserves the original state, and passes evaluation. Neither package manager is invoked.

The complete run creates **three** Experiences: original failure, baseline failure, alternative success. It creates one immutable hypothesis, a Candidate Lesson revision at 0.42, a two-trial Experiment, and a supported Lesson revision at 0.78. All three Realities are discarded by default while their artifacts remain.

## Manual workflow

Use `run --script` with explicit required checks, then `lesson propose --experience ... --avoid ... --prefer ...` and `experiment run --lesson ...`. The entire recorded script is the replacement unit. File-operation and custom ActionPatterns are representable but not executable mutations. Proposals from opaque generic-agent runs are inspectable Candidates; attempting to replay them fails rather than guessing their internal actions.

The deterministic provider recognizes only the local fixture. Manual proposals work for other scripts. External-command/LLM reflection is deferred; no API call is needed to test the empirical loop.

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

One support moves a Candidate to `CounterfactuallySupported` at 0.78. A contradiction moves it to `Contradicted` at 0.20. Further support never silently erases a contradiction or promotes to `Validated`. These are heuristic indicators, not calibrated probabilities. Both trial references form one experiment's evidence; they are not counted as two replications.

## Retention and failure handling

Plans are persisted before trials. Each trial has its own Experience, evaluation, execution, Reality, and artifact references. Terminal Experiment and Lesson evidence updates commit together. Store reads reconstruct partial trials from their own immutable rows, even if an Experiment was interrupted before finalization.

Normal failed checks, timeouts, and Ctrl-C retain evidence and remove worktrees. If artifact capture or database persistence fails, the affected worktree is retained to avoid losing the only copy of changes. The error reports its location. This is a deliberate exception to automatic cleanup. Stop abandoned commands before `reality cleanup`; kept/retained worktrees need explicit inspection/discard.

SIGKILL/power loss can leave an Experiment marked running. Inspection and orphan Reality cleanup work; automatic experiment recovery, resumption, artifact pruning, and cross-filesystem transactions are deferred.
