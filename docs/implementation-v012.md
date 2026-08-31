# Hardknock V0.12 implementation report

V0.12 adds a deterministic adaptive runtime that uses accumulated Experience while an agent works. It chooses among six typed outcomes, preserves every decision and policy version, accepts observed feedback, and turns repeated runtime uncertainty into curriculum recommendations.

This report describes the code and deterministic fixture evidence in this repository as of 2026-08-31. It is not a production reliability estimate.

## 1. Files created and changed

The principal additions are `src/runtime/{model,policy,context,scenario,benchmark}.rs`, `src/store/runtime.rs`, `src/cli/runtime.rs`, `migrations/015_runtime.sql`, `tests/runtime.rs`, twelve files under `fixtures/runtime-scenarios/`, and the runtime documentation. Bridge, protocol, configuration, direct-run, development-profile, transfer-status, core-ID, architecture, threat-model, CLI, README, and roadmap files were extended.

## 2. Schema migration

Migration 015 adds immutable policy versions and decisions, ordered reason/evidence projections, append-only feedback, abstention projections, and control events. Foreign keys preserve decision provenance without copying Experience rows. Triggers reject updates and deletion.

## 3. `RuntimeDecision` model

`RuntimeDecision` is a Rust enum with `Act`, `Experiment`, `Replan`, `Recover`, `RequireApproval`, and `Abstain` variants. Each carries a dedicated payload. `RuntimeDecisionId`, `HardknockSessionId`, and `RuntimePolicyVersionId` are typed core IDs.

## 4. `KnowledgeState` semantics

The deterministic classifier returns `KnownSupported`, `KnownContradicted`, `KnownStale`, `Unknown`, or `OutOfScope`. Support requires compatible, current, non-contradicted local evidence. Remote advisory evidence does not become local support. A current local Lesson that marks the proposed action as `avoid` is a separate failure-precursor signal and produces `REPLAN`.

## 5. Risk model

Risk remains dimensional: severity, reversibility, externality, assurance requirement, structured Effect risk, and rationale. No universal scalar safety score was added. The Effect risk adapter derives dimensions from structured kind, operation, and target.

## 6. Uncertainty model

Runtime uncertainty has an explicit level, typed reasons, and candidate strategies. Sources include missing Experience, contradiction, failed prediction/staleness, multiple strategies, reported uncertainty, out-of-envelope context, and known gaps. The controller does not require or infer hidden chain of thought.

## 7. Deterministic decision policy

`DeterministicRuntimeController` evaluates an inspectable matrix plus ordered precedence rules. The synchronous policy calls no model. It handles low-risk unknown action with warning, bounded experiments, Reflex/Lesson replanning, Recovery selection, authority-gated approval, and evidence-based abstention.

## 8. Runtime policy profiles

`developer`, `balanced`, and `conservative` are implemented. Balanced is the default profile; advise is the default autonomy mode so ordinary runs retain their existing behavior. Configuration contents generate a stable hashed policy version. Compare and simulate expose profile differences without action execution.

## 9. Assurance integration

The full context synthesizer selects exact current Skill certifications, checks revision, status, revocation, expiry, profile applicability, risk requirements, and evidence gaps. High-risk `ACT` requires current applicable assurance. Certification remains evidence interpretation, not authority.

## 10. Operating Envelope integration

Applicable Skill envelopes contribute `KnownSafe`, `KnownDegraded`, `KnownFailure`, or `Unknown`. Known failure replans; degraded medium/high risk replans; unknown consequential work favors a safe experiment. Untested space remains unknown.

## 11. Reflex integration

The Bridge performs Reflex matching before returning action guidance. Active Reflexes yield `REPLAN`; supported Reflexes remain warnings. Matching respects scope, action, repeated-failure count, no-state-change/config-change signals, status, and freshness.

## 12. Recovery integration

On failed `ActionCompleted`, the Bridge uses the bounded error class as a failure signature, finds supported/validated scoped Recoveries in the hot cache, and asks the same controller for follow-up guidance. Fresh matches yield `RECOVER`; stale matches favor an experiment when safe.

## 13. Experiment integration

`ExperimentDecision` carries a reason, question, candidates, Experience budget, Reality requirements, and automatic eligibility. `off`, `suggest`, and `automatic` modes exist. Automatic eligibility never bypasses Reality, Effect-safety, isolation, duration, or budget limits and does not create arbitrary fanout.

## 14. Approval semantics

`REQUIRE_APPROVAL` means evidence may support preparation but requested commit or user authority is absent. The payload includes the requested authority, evidence summary, dimensional risk, and alternatives. It is not a generic prompt and does not imply that approval grants a missing capability.

## 15. Abstention semantics

`ABSTAIN` records a typed reason, missing assurance, unresolved blockers, and possible resolution steps. Unsupported irreversible Effects, insufficient isolation, critical unknowns, contradictions, unavailable safe experiments, and exhausted budgets remain explicit rather than becoming guessed yes/no answers.

## 16. Policy precedence

Hard policy, capabilities, isolation, and Effect adapter support precede runtime evidence. Recovery then Reflex/failure-precursor handling precede envelope and matrix evaluation. Experience cannot override security policy or grant commit authority.

## 17. Agent adapter changes

Claude, Codex, and other adapters continue through the shared Bridge protocol; no adapter-specific duplicate controller was added. The Bridge adds `RuntimeDecisionRequested`, `RuntimeDecisionMade`, and `RuntimeDecisionFeedback` messages. Its hot path returns established Bridge responses while storing the richer runtime decision.

## 18. Runtime decision persistence

Every CLI/direct-run decision is synchronously persisted. Bridge decisions are evaluated synchronously and queued to the Bridge's ordered SQLite writer. The writer recomputes policy evaluation, validates context hash/session/decision equality, and then atomically stores the record, reasons, evidence, abstention, policy, and events.

## 19. Decision feedback model

Feedback outcomes are successful, failed, avoided failure, unnecessary intervention, and inconclusive. Feedback can carry Experience references and agent disagreement. It appends to history and does not mutate the decision.

## 20. Runtime-to-curriculum integration

`runtime gaps` groups recurring unknown, stale, contradicted, experiment, and abstention contexts. It returns actual `CurriculumRecommendation` objects scoped to the repository with `auto_run: false`. No background curriculum daemon was added.

## 21. CLI commands

Implemented commands are `runtime status`, `audit`, `gaps`, `policy`, and `benchmark`; `decision list`, `show`, `replay`, `simulate`, `compare`, and `feedback`; `why --decision`; and `run --runtime-mode`. Human and JSON output are supported.

## 22. Synchronous-path performance

The checked-in deterministic benchmark runs 1,000 samples for a cached context, cached assurance summary, cached envelope summary, and Reflex-match context. The observed local debug-build P95 values were 0.001292 ms, 0.001000 ms, 0.001250 ms, and 0.002542 ms respectively; all paths made zero synchronous LLM calls. These measure policy over already materialized hot-cache summaries, not cold SQLite synthesis, container startup, or model latency. The existing full Bridge handler benchmark also remained below its 25 ms P95 gate after decision persistence moved to the ordered writer.

## 23. Adaptive-learning benchmark

The mandatory benchmark uses 60 deterministic scenario evaluations per arm. Agent-only task success was 0.25, static rules 0.4167, and adaptive runtime 1.0. The growing-experience sequence changes the same context from `UNKNOWN → EXPERIMENT` to locally supported `ACT`.

## 24. Negative-learning benchmark

Before a learned boundary, the fixture proceeds. After a validated Reflex identifies the failure precursor, the controller returns `REPLAN`. The adaptive arm's avoided-failure rate is 0.90, with a deliberately included false Reflex producing a 0.10 unnecessary-intervention rate.

## 25. Stale-evidence result

Applicable but stale evidence classifies `KnownStale` and produces an experiment for the consequential testable fixture. It is not silently treated as current support.

## 26. Recovery result

A matching fresh scoped Recovery produces `RECOVER`. Benchmark Recovery success is 1.0 and time to recovery is 20 ms versus the fixture's slower uncontrolled path.

## 27. Abstention and approval tests

Unsupported irreversible human-visible Effects with no adapter produce `ABSTAIN(UnsupportedEffect)`. Strong current support for a prepared consequential Effect without commit authority produces `REQUIRE_APPROVAL`; the two paths are asserted separately.

## 28. False-positive intervention result

`UnnecessaryIntervention` feedback on a Reflex-driven replan disables the Reflex in revision 2, lowers confidence from 0.90 to 0.30, and appends a `FalsePositive` resilience test. Original evidence and decisions remain immutable.

## 29. Certification-scope test

A valid certification outside the current scope does not authorize `ACT`; the testable fixture remains an experiment. Exact Skill revision, profile, expiry, revocation, environment, and action applicability checks are explicit.

## 30. Federated-evidence test

Authentic federated advisory evidence remains `Unknown` locally and produces an experiment in the fixture. It can advise or suggest investigation under configured policy but cannot independently authorize action, block, recover, or replan.

## 31. Policy precedence tests

Hard policy and missing capabilities force abstention even when the scenario otherwise has known support. Tests also distinguish security blocking from runtime recommendation, Effect authority from assurance, and balanced high-risk experimentation from conservative abstention.

## 32. Known limitations

- The benchmark is a designed deterministic scenario library, not a production-agent trial or statistical reliability estimate.
- Hot-cache assurance/envelope latency measures policy over cached summaries; cold store synthesis is outside that result.
- The live Bridge hot cache currently synthesizes Lessons, Reflexes, and Recoveries. Full Skill/certification/envelope lookup is used by CLI, direct run, replay, and explicit scenario paths.
- Remote evidence is represented and policy-gated but is not automatically refreshed on the synchronous Bridge path.
- Automatic experiment decisions expose bounded eligibility; they do not autonomously fan out arbitrary live agent work.
- Bridge decision persistence is ordered and asynchronous. A host crash after guidance but before writer commit can lose the record; `flush` surfaces queued writer errors during orderly shutdown.
- Existing V0.9–V0.11 live Docker/Podman, WASI, PostgreSQL, and external-agent acceptance boundaries still apply.
- No MCP facade exists in this repository, so no placeholder MCP API was added.

## 33. Deviations and rationale

The prompt suggested `tests/runtime/`; this repository's established integration-test layout uses `tests/runtime.rs`, so the suite follows that convention. Default autonomy is `advise`, one of the permitted early defaults, so ordinary external behavior does not change without configuration. The Bridge preserves its pre-V0.12 active-Reflex interception, while Lesson-driven replans remain advisory and their stored runtime classification is explicit. Performance lookup labels refer to cached decision summaries and are documented as such rather than being presented as cold database timings.

## 34. Recommended V0.13 direction

V0.13 should add multi-agent Experience coordination and epistemic diversity: independent proposals, disagreement provenance, evidence-aware comparison, and bounded escalation when one reasoning path is insufficient. Agreement must not become authority, and shared evidence must retain origin, scope, contradictions, and local reproduction requirements.

## Reproduction

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all

hardknock runtime benchmark
hardknock decision compare \
  --scenario fixtures/runtime-scenarios/unknown-high-risk.json
```

Machine-readable results are in [the V0.12 benchmark summary](benchmarks/v012-runtime-summary.json).
