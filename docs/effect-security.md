# Effect security boundary

> **Experimentation authority and mutation authority are different capabilities.**

V0.8 separates `observe`, `propose`, `prepare`, `commit`, and `compensate`. Default agent capability permits the first three and denies the last two. Local user CLI flows can supply explicit authority. Lessons and Reflexes may advise, warn, replan, or request experiments; they never become commit policy.

The deterministic adapters enforce structured targets, bounded JSON payloads, version checks, exact authorization scope, expirations, and idempotency. Identity and effect history live in private Hardknock data directories. Receipt and snapshot evidence is hashed through existing artifact references, but the SQLite store is not claimed tamper-proof.

## What is not isolated

The Git Reality remains a worktree, not a security sandbox. Host processes, network, credentials, Git objects/configuration, and files outside the worktree remain reachable by trusted commands. Only explicit `EffectRequest`s routed through registered adapters receive V0.8 transactional semantics. Hardknock does not intercept syscalls, arbitrary HTTP libraries, arbitrary shell commands, or native agent tools it cannot deterministically substitute.

Financial effects and unknown external effects are rejected by default policy. Human-visible effects require external approval. No real payments, email, cloud mutations, Kubernetes promotion, cross-provider two-phase commit, or transparent network proxy is implemented.

V0.9 must add an actual capability isolation boundary before stronger containment claims are appropriate.
