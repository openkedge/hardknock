# Implementation roadmap

The local passive learning and active resilience loops are implemented. Evidence remains limited to trusted local fixtures and explicit scripts. V0.3 adds a local authenticated Bridge and native lifecycle adapters with deterministic cross-agent transfer coverage. Two-agent successful live acceptance is not complete.

| Milestone | Status | Deliverable |
| --- | --- | --- |
| 0–2 — Bootstrap, Reality, execution | Implemented | Rust CLI, detached worktrees, controlled scripts, artifacts, process groups and cleanup |
| 3–6 — Evaluation through counterfactual evidence | Implemented | Immutable Experiences, hypotheses, versioned Lessons, fresh paired trials |
| 7–8 — Retrieval and transfer | Implemented | Scope gates, explained scores, context files, bounded retry, distinct application validation and contradiction |
| 9 — Local chaos | Implemented | Healthy control, four local perturbations, bounded campaigns, three deterministic fixtures |
| 10 — Operating envelopes and Skills | Implemented locally | Explicit tested points, unknown untested space, manual supported procedure registration |
| 11 — Reflexes | Implemented locally | Scoped precursor matching, paired response tests, false positives, separate activation and historical explanations |
| 12 — Recovery | Implemented locally | Failure reproduction/precheck, typed steps, paired Experiences, support/contradiction and metrics |
| 13 — External-agent integration | Preview; live acceptance pending | Common Bridge, native adapters, deterministic cross-agent transfer |
| 14 — Agent-native experimentation | Implemented locally in V0.4 | Explicit bounded requests, equivalent starts, parallel candidates, quality, replay/cancel/lineage and patch export |
| 15 — Skill hardening and curriculum | Implemented locally in V0.5 | Deterministic gaps, bounded trials, profile coverage/maturity, Experience Packages, held-out fixture benchmark |
| 16 — Persistent development | Implemented locally in V0.6 | Scoped profiles, immutable snapshots/episodes, Skill/package revisions, freshness, revalidation and three-arm longitudinal fixtures |
| 17 — Experience federation | Implemented locally in V0.7 | Node identity, signed/redacted bundles, peer trust, advisory import, context matching, local reproduction, provenance/conflicts, filesystem transport and three-node benchmark |

## Verified acceptance boundaries

The V0.1 fixtures retain observed transfer on distinct task B, scope rejection on C, and controlled contradiction on D. V0.2 observes the explicit retry delay points 0/100/500ms PASS, 1000ms DEGRADED, and 2000ms FAIL. A paired Reflex test replans after three failures and succeeds; a transient three-failure negative case identifies a false positive. Stale simulated credentials and planned/config generation drift demonstrate failure reproduction and restoration. Source repositories remain unchanged.

See [the V0.2 report](implementation-v02.md), [transfer report](implementation-transfer.md), and historical [Milestones 3–6 report](implementation-phase-3-6.md). Local verification is on macOS; configured Linux/macOS CI is not a report of a remote CI run. No external model is required by the test suite.

## Exact next-phase plan

**V0.8 — Governed External Effects and Transactional Realities**, with remaining integration acceptance tracked separately. V0.4–V0.7 do not retroactively complete V0.3 live acceptance. V0.7 measures local designed fixtures, not production team reliability.

1. Run installed Claude and Codex through the common layer with the user's native permissions intact. Demonstrate context delivery, action advice, evaluated completion, and observed transfer on distinct disposable repositories.
2. Load the Hermes/OpenClaw plugins in real hosts, verify their SDK/version compatibility, and exercise the documented missing-ID and timeout behavior.
3. Add a native user-approval callback for the noninteractive Codex client; never auto-approve from experience.
4. Harden sustained usage: session retention, crash reconciliation, bounded artifact/Git capture, host-specific strict availability policy, and automatic environment-version revalidation.
5. Add governed effect intents, adapter-scoped prepare/commit semantics, immutable receipts, stale preparation checks, idempotency and reconciliation. A Git Reality cannot reverse external effects.
6. Strengthen durable execution before adding scheduling: crash reconciliation, bounded artifact capture, large-inventory retrieval, per-session shared resource policy, and stronger provider isolation. Background work remains deferred until explicit permissions and resource accounting are designed and tested.

MCP may later be an optional facade over the same Bridge; it is not the architecture. Hosted services, multi-user authentication, remote sharing/sync, marketplaces, privileged/cloud/network chaos, financial effects, arbitrary transaction virtualization, VM/WASM backends, tournaments, and GUI remain out of scope.

See [the V0.3 report](implementation-v03.md) for remaining acceptance work and measured results.
