# Transactional Realities

A V0.8 Reality can own two isolated histories:

```text
Reality
├── detached Git worktree
└── Effect Ledger
    ├── Effect A: PREPARED
    └── Effect B: DISCARDED
```

The Reality stores its `EffectLedgerId`; each Effect row also records its Reality. Preparing through the deterministic adapters leaves authoritative fixture state unchanged. `reality show` reports proposed, prepared, committed, discarded, and unknown counts.

`reality discard` first asks every attached adapter to discard uncommitted prepared effects and clean staged resources. Only after that succeeds does it remove the worktree. Cleanup failure stops the discard and identifies the remaining Effect. An `UNKNOWN` effect cannot be discarded because the mutation may already have happened.

## EffectPlan experiments

`CandidateExecution::EffectPlan` combines structured `EffectRequest`s with Reality-local simulation steps. The experiment engine:

1. creates equivalent Git Realities;
2. prepares each candidate's structured effects;
3. runs its local simulation and the common evaluator;
4. discards all effects for failed and losing candidates;
5. detaches only the selected passing candidate's prepared Effects into standalone ledgers;
6. returns a recommendation with zero automatic commits.

The selected effect remains `PREPARED`. The user may inspect or commit it later. A passing evaluator therefore means “candidate behavior passed”; it does not mean “production mutation succeeded.”

## Crash and orphan behavior

Prepared Effects attached to a missing or discarded Reality appear under `effect orphans`. `effect cleanup` invokes only adapter discard operations. It does not reconcile `UNKNOWN` Effects, compensate committed Effects, or guess whether an external mutation happened. Standalone Effects prepared through the Bridge or detached after selection are intentional and are not classified as orphans.
