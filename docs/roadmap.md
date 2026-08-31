# Implementation roadmap

The local passive learning and active resilience loops are implemented. V0.9 adds a capability model, container Reality provider, scoped token/credential paths, an execution proxy, and a PostgreSQL adapter. Evidence remains limited by the runtime on which it was observed. V0.3 adds a local authenticated Bridge and native lifecycle adapters with deterministic cross-agent transfer coverage; two-agent successful live acceptance is not complete.

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
| 18 — Governed external effects | Implemented for deterministic adapters in V0.8 | Effect lifecycle/ledger, transactional Realities, mock HTTP/database/message/shadow adapters, explicit commit authority, receipts, idempotency, reconciliation, compensation, groups and benchmark |
| 19 — Capability-isolated execution | Implemented in V0.9; live runtime acceptance pending | Immutable manifests, truthful provider requirements, Docker/Podman provider, signed Reality tokens, shell/file proxy, test credential broker, scoped Effect relay, structured PostgreSQL adapter and security benchmark |
| 20 — Micro-sandboxed tools and attestation | Implemented locally; live container/WASI acceptance pending | Portable tool manifests and registry, Reality ∩ Tool ∩ Grant capability resolution, per-invocation provider lifecycle, lifecycle persistence, execution receipts/attestations, raw exposure benchmark and explicit host/WASI limitations |

## Verified acceptance boundaries

The V0.1 fixtures retain observed transfer on distinct task B, scope rejection on C, and controlled contradiction on D. V0.2 observes the explicit retry delay points 0/100/500ms PASS, 1000ms DEGRADED, and 2000ms FAIL. A paired Reflex test replans after three failures and succeeds; a transient three-failure negative case identifies a false positive. Stale simulated credentials and planned/config generation drift demonstrate failure reproduction and restoration. Source repositories remain unchanged.

See [the V0.2 report](implementation-v02.md), [transfer report](implementation-transfer.md), and historical [Milestones 3–6 report](implementation-phase-3-6.md). Local verification is on macOS; configured Linux/macOS CI is not a report of a remote CI run. No external model is required by the test suite.

## Remaining V0.9 runtime acceptance

V0.9 code and pure security coverage do not retroactively complete V0.3 live acceptance. The checked-in V0.9 benchmark has an unobserved container arm and the optional PostgreSQL test was skipped on the development host. Before making production security claims:

1. Run the container integration layer on rootless-compatible Docker and Podman hosts, covering mount escape, network none, internal allow-list fixtures, non-root execution, resource limits, freeze, and crash cleanup.
2. Run the PostgreSQL fixture against a disposable server, covering invariant rejection, concurrent version conflict, reprepare, authorized transaction, receipt, and idempotent retry.
3. Run a fake and at least one installed agent with host reasoning plus containerized execution, while preserving native user approvals.
4. Measure provider creation/execution/disposal latency and resource overhead; publish denominators and runtime/image versions.
5. Route retry/reflection, candidate, curriculum, recovery, and evaluator commands through the same capability boundary before describing those workflows as isolated.

## Exact next-phase plan

**V0.10 — Micro-Sandboxes, Portable Capability Tools, and Execution Attestation.** The local foundation is implemented: portable named tools, Reality/tool/grant intersection, short-lived provider lifecycle, credentialless Effect requests, execution attestations, CLI inspection, and raw authority-surface reporting. Live container and WASI acceptance remains below.

Move from a container-scale Reality toward per-tool capability sandboxes, short-lived execution environments, portable tool manifests, stronger provider isolation, and independently inspectable execution receipts. Candidate components are a WASM/WASI tool runtime, a microVM provider, capability-signed tools, tool-level filesystem/network grants, shorter-lived credential exchange, and artifact/effect attestation. Preserve provider truth: container evidence is not microVM evidence, and signed local metadata is not remote attestation.

Remaining cross-version integration work continues in parallel:

1. Run installed Claude and Codex through the common layer with the user's native permissions intact. Demonstrate context delivery, action advice, evaluated completion, and observed transfer on distinct disposable repositories.
2. Load the Hermes/OpenClaw plugins in real hosts, verify their SDK/version compatibility, and exercise the documented missing-ID and timeout behavior.
3. Add a native user-approval callback for the noninteractive Codex client; never auto-approve from experience.
4. Harden sustained usage: session retention, crash reconciliation, bounded artifact/Git capture, host-specific strict availability policy, and automatic environment-version revalidation.
5. Strengthen durable execution before adding scheduling: crash reconciliation, bounded artifact capture, large-inventory retrieval, per-session shared resource policy, and stronger provider isolation. Background work remains deferred until explicit permissions and resource accounting are designed and tested.

MCP may later be an optional facade over the same Bridge; it is not the architecture. Hosted services, multi-user authentication, remote sharing/sync, marketplaces, privileged/cloud/network chaos, financial effects, arbitrary transaction virtualization, production AWS/Kubernetes mutation, arbitrary HTTP interception, organization RBAC, tournaments, and GUI remain out of scope.

See [the V0.3 report](implementation-v03.md) for remaining acceptance work and measured results.

## Remaining V0.10 runtime acceptance

Run the optional container layer on Docker/Podman hosts and add a WASI build
before claiming live micro-sandbox isolation. Measure startup/disposal overhead,
capability duration, credential lifetime, tool tampering, artifact mutation,
replay match/divergence, and the flagship multi-tool dependency task. Imported
tools must remain disabled until local approval; no executable marketplace or
hardware-backed attestation is implied.
