# Experiment quality and comparison

The result answers: **Which candidate performed better under this starting state and evaluator?** It does not establish that a strategy is universally better or that an agent identity caused an outcome.

## Equivalent starts

Before any execution, the orchestrator prepares every leased Reality, verifies its Git commit and clean diff, and rechecks the common starting fingerprint. Each worker verifies its worktree again before execution. The fingerprint binds:

- Canonical repository, full commit and tree IDs, including tracked fixture/input files.
- OS, architecture, normalized controlled environment policy and `/bin/sh` hash.
- Resolved executor binaries, templates, reported version/model labels and environment modes.
- The common evaluator command specification.

Every candidate Experience stores the experiment ID, candidate ID and the same composite fingerprint before insertion. A mismatched expected fingerprint, a drifted candidate worktree, or changed measured runtime refuses comparison. Tests exercise both input-state drift and post-fork worktree drift before any candidate process.

This proof has a scope, not omniscience: ignored/untracked dependencies, host services, clock/random state, inherited native settings, remote models, executable dependencies and Git configuration are not fully frozen. Git worktrees share host resources and repository metadata. Test oracles must be trusted; the same evaluator command can still call candidate-modified tests or external dependencies. This release does not make adversarial candidate code or mutable test oracles trustworthy.

## Changed variables

Derived variables include strategy, agent/version, model, executor configuration and environment mode. Strategies are represented by hashes in variable summaries; candidate names alone do not imply a changed strategy. Different versions/models supplied by native metadata are descriptive, not independently verified vendor attestations.

| Quality | Meaning | Lesson treatment |
| --- | --- | --- |
| Controlled | Measured equivalent starts; no more than one derived variable changes; local shell/known fixture execution | A single failing/passing pair may propose a scoped Candidate Lesson |
| PartiallyControlled | No multi-variable confound, but a nonfixture agent or inherited environment is involved | Observational evidence; no automatic causal Lesson |
| Confounded | More than one variable differs, including agent **and** strategy | May report a trial winner, but no causal strategy attribution or automatic Lesson |
| Invalid | Starting equivalence or required execution/evidence collection failed | No recommendation or Lesson |

Controlled describes the declared measured scope. A strategy can itself contain many edits; this label is not proof about which internal edit caused success. Cancellation/completeness is represented separately by experiment, execution and evaluation status; interrupted comparisons do not recommend a winner even if their preparation was controlled.

The `confounded-comparison` fixture uses fake-agent-A/strategy-A and fake-agent-B/strategy-B. B passes and A fails, but the result is Confounded, with no Candidate Lesson generated. The `strategy-choice` fixture uses one fake agent for both strategies and reports Controlled.

## Deterministic comparison policy

`EvaluatorSuccessFirst` validates evaluations and refuses different fingerprints/specifications. It orders completed evidence by evaluator success, then fewer failed required checks. A recommendation also requires successful candidate execution. Incomplete, timed-out, unconfigured or one-candidate comparisons do not produce a winner. `require_success` defaults true: when every candidate fails, there is no recommended solution.

Passing ties stay ties. Diff size and duration are used only when explicitly enabled. Text diff size counts insertions plus deletions, not file count; missing or binary diffs make that criterion unavailable for the whole comparison. Smaller diffs are not intrinsically better. Duration is an observed, noisy metric and can include worker overhead; it is not a benchmark guarantee.

```bash
hardknock try --candidate 'a=./strategy-a' --candidate 'b=./strategy-b' \
  --check './test.sh' --minimize-diff-size
```

`confidence` is `null`: V0.4 does not invent calibrated probabilities from a single trial. `comparison.evidence_weight` explains controlled, partial, confounded or inconclusive evidence qualitatively.

## Learning boundary

Every normally executed/evaluated candidate records an immutable Experience and hashed artifacts, including interrupted outcomes. Reflection may create a new hypothesis and **Candidate** Lesson for a completed controlled pair with one failure and one passing recommendation. The rationale links both Experience IDs. Its action patterns refer to the candidate IDs, not an automatically executable generalized recipe. It is not automatically retried, promoted to Supported/Validated, or converted to a Reflex. Candidate-specific results require a separately designed validation recipe before becoming reusable command advice.

Confounded or invalid results do not generate a causal Lesson. Existing counterfactual and distinct-application promotion rules remain separate and unchanged. Persistence/capture failures can leave raw artifacts or an execution without a completed Experience; the experiment fails and cannot claim successful comparison. Replay creates new evidence, never repairs history by rewriting it.
