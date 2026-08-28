# Implementation roadmap

The local passive learning and active resilience loops are implemented. Evidence remains limited to trusted local fixtures and explicit scripts. The older generic context contract has one historical Codex CLI smoke test; V0.2 adds no new real-agent integration.

| Milestone | Status | Deliverable |
| --- | --- | --- |
| 0–2 — Bootstrap, Reality, execution | Implemented | Rust CLI, detached worktrees, controlled scripts, artifacts, process groups and cleanup |
| 3–6 — Evaluation through counterfactual evidence | Implemented | Immutable Experiences, hypotheses, versioned Lessons, fresh paired trials |
| 7–8 — Retrieval and transfer | Implemented | Scope gates, explained scores, context files, bounded retry, distinct application validation and contradiction |
| 9 — Local chaos | Implemented | Healthy control, four local perturbations, bounded campaigns, three deterministic fixtures |
| 10 — Operating envelopes and Skills | Implemented locally | Explicit tested points, unknown untested space, manual supported procedure registration |
| 11 — Reflexes | Implemented locally | Scoped precursor matching, paired response tests, false positives, separate activation and historical explanations |
| 12 — Recovery | Implemented locally | Failure reproduction/precheck, typed steps, paired Experiences, support/contradiction and metrics |
| 13 — External-agent integration | Next | Stable integration contracts, adapters/hooks, cross-agent validation |

## Verified acceptance boundaries

The V0.1 fixtures retain observed transfer on distinct task B, scope rejection on C, and controlled contradiction on D. V0.2 observes the explicit retry delay points 0/100/500ms PASS, 1000ms DEGRADED, and 2000ms FAIL. A paired Reflex test replans after three failures and succeeds; a transient three-failure negative case identifies a false positive. Stale simulated credentials and planned/config generation drift demonstrate failure reproduction and restoration. Source repositories remain unchanged.

See [the V0.2 report](implementation-v02.md), [transfer report](implementation-transfer.md), and historical [Milestones 3–6 report](implementation-phase-3-6.md). Local verification is on macOS; configured Linux/macOS CI is not a report of a remote CI run. No external model is required by the test suite.

## Exact next-phase plan

**Stable real-agent integration surfaces**, building on the deterministic local loop:

1. Specify versioned portable contracts for Experience Query, Experiment Request, Reflex Evaluation, and Evidence Reporting, preserving scope, agent identity, immutable artifacts, and observed/self-reported distinctions.
2. Expose an explicitly permissioned MCP/API layer and adapter capability negotiation. Do not imply a query grants permission to execute, block, or access credentials.
3. Add opt-in lifecycle hooks for selected real agents (Claude, Codex, Hermes, OpenClaw), with clear observation boundaries and explicit integration tests. No vendor should gain confidence merely from its identity.
4. Validate influence on related tasks across different agents, retain disabled controls and contradictory evidence, and account for tokens, retries, and experiment costs.
5. Strengthen artifact verification, crash reconciliation, environment manifests, and isolation before broadening the adversity model or making stronger causal claims.

These integrations have not begun in V0.2. Keep real network/privileged/cloud chaos, external financial effects, arbitrary credential interception, browser transaction isolation, WASM, tournaments, GUI, hosted services, organization-wide sharing, and automatic blocking out of this local release.
