# Hardknock V0.15 implementation report

This pass implements deterministic Predictive Experience on top of V0.14 causal
evidence. The measured benchmark is [predictive-v015.json](benchmarks/predictive-v015.json)
and operating instructions are in [predictive-experience.md](predictive-experience.md).

## 1–25. Implementation

| Item | Result |
| --- | --- |
| 1. Files | Added `predictive::{model,engine,policy,benchmark}`, predictive store/CLI code, migration 018, 10 fixture families, integration tests, docs, and benchmark output. Updated runtime, Bridge, causal-facing policy, Reflex metadata, assurance, curriculum, core IDs, README, and architecture. |
| 2. Schema | Migration 018 adds indexed trajectories/events, indicators, signatures/revisions, forecasts/feedback/misses, interventions/counterfactuals, quality snapshots, and compact predictive events. Raw events, revisions, feedback, and counterfactual evidence are append-only. |
| 3. ExecutionTrajectory | A session/task-family-scoped ordered record with start/end, event references, outcome, bounded operational context, and structural fingerprint. |
| 4. Normalization | `TrajectoryObservation` accepts at most 64 bounded operational feature names/values. Full prompts, conversation, hidden reasoning, environment dumps, and secret-shaped arbitrary names are outside the model. |
| 5. Fingerprint | Hashes ordered event kinds plus latest salient normalized features. Timestamps are excluded; millisecond features use coarse declared buckets. |
| 6. RiskIndicator | Separate candidate/supported/validated/contradicted/retired precursor model. Deterministic discovery extracts failure-only event/feature differences from at least two failures and two successful controls; the result stays Candidate. Candidates and remote indicators cannot activate runtime. |
| 7. EarlyWarningSignature | Ordered simple conditions, failure class, qualitative horizon, exact context/runtime scope, local/advisory origin, evidence, causal basis, and append-only revisions. It remains distinct from a post-failure signature. |
| 8. ForecastHorizon | Supports next action, action count, duration, milestone, and unknown. No precise time claim is synthesized. |
| 9. Forecast engine | Deterministic ordered matcher over a 20-event/five-minute default window. No LLM, network, anomaly model, embeddings, or global score. |
| 10. Historical matching | Preserves event order, task/scope compatibility, and structural-prefix count. No misleading universal similarity percentage. Historical scans are a slow-path option. |
| 11. Causal integration | A signature names typed V0.14 hypotheses; the store supplies only locally supported canonical hypotheses. Causal plus matching history yields mixed strong evidence. |
| 12. Forecast evidence | Explicitly labeled correlational, causal, or mixed. Strength is insufficient/weak/moderate/strong, never an invented probability. |
| 13. Preventive intervention | Structured action, target failure, signature, cost, reversibility, externality, effect-authority requirement, scope, origin, and earned status. |
| 14. Counterfactual validation | Requires a `Controlled` existing Experiment, two distinct candidate arms, and candidate fingerprints equal to its starting-state proof. Control fail/intervention pass yields `AvoidedFailure`; two distinct Experiment IDs validate, so replaying one experiment cannot inflate support. |
| 15. Runtime | `RuntimeDecisionContext` carries active forecasts/interventions without changing legacy hashes when empty. Hard policy and capabilities run first. Validated prevention can warn/replan; external authority remains approval-gated. Persisted decisions reload canonical status and quarantine degrading predictors. |
| 16. Reflex | Adds `ReflexTiming::{Reactive,Preventive}` and an optional typed early-warning reference without creating a second Reflex subsystem. |
| 17. Effects | Normalized `EffectPrepared` observations support expiry forecasting; tested `ReprepareEffect` remains a preventive action, not Recovery. |
| 18. Feedback | Active forecasts resolve once as failure occurred, avoided, false alarm, expired, or inconclusive. A missed failure is stored separately. |
| 19. Metrics | Precision, recall, false-positive/negative rates, avoided-failure rate, and unnecessary-intervention rate use explicit denominators and remain absent below the minimum sample. Predictive summaries are included in Experience Profiles/snapshots and their changes appear in growth reports. |
| 20. Warning lead | Uses action distance for deterministic tasks. A zero-lead warning is retained but is not presented as useful early warning. |
| 21. Health/staleness | Contradiction, false-alarm degradation, and explicit runtime-version drift recommend revalidation. History is preserved while automatic persisted use is quarantined. |
| 22. Curriculum | Adds discover, validate, calibrate, and preventive-validation goal kinds. Misses create discovery recommendations; observability gaps do not pretend more trials can expose missing signals. No goal auto-runs. |
| 23. Federation | Remote signatures retain advisory origin and Candidate status. Localization creates a distinct local Candidate with provenance; local positive/negative reproduction is still required. |
| 24. Assurance | Adds optional `PredictiveFailureCoverage` with profile-defined forecastability, precision, false-positive, prevention, and severity requirements. Ordinary Skill certification is unchanged. |
| 25. CLI | Implements the required trajectory list/show/compare/replay and forecast list/show/explain/replay/quality commands, plus predict, discover, impact, forecastability, curriculum, register/validate, localization, and trusted benchmark commands. Replay copies only the pre-failure normalized prefix and never executes raw actions. |

## 26–36. Measured results

All values below are finite deterministic fixture observations, not population claims.

| Item | Result |
| --- | --- |
| 26. Retry exhaustion | Historical `timeout → retry → stale state` triggers a Strong mixed forecast with a two-action horizon. |
| 27. False-positive refinement | Broad retry warning fires on 3/3 healthy controls (60% fixture-set false-positive rate when combined with failures). Adding the stale-state condition fires on 0/3. |
| 28. Forecast miss | A new uncovered precursor records `NotYetPredictable` and a discovery curriculum recommendation. |
| 29. Observability | A hidden remote-state failure records `InsufficientObservability`; no warning is generated. |
| 30. Causal predictor | Latency-only warning has fixture precision 0.4. The stale-state mechanism-backed warning has precision 1.0 and false-positive rate 0.0 within the configured retry profile. |
| 31. Prepared effect | An approaching-expiry warning forecasts the next action. Two real Experiment pairs show control FAIL and reprepare PASS; the action becomes Validated. |
| 32. Staleness | A v1-scoped signature in v2 context reports Stale and recommends revalidation. Repeated false alarms report Degrading without rewriting validation history. |
| 33. Federation | Remote warning remains advisory and cannot validate in place; localization creates a new local Candidate. |
| 34. Authority | A valid high-severity external forecast and recommendation without commit authority returns RequireApproval. Prediction grants no authority. |
| 35. No overreaction | Weak/low-severity conditions with a high-cost action return Observe/Warn rather than intervention. |
| 36. Runtime performance | Standalone debug observation: 0.582083 ms for indexed deterministic evaluation versus the 40 ms target. The test also exercises 100 repeated local evaluations. This is not a production P95 or SLA. |

The three benchmark arms report: Reactive initial failures 2 and avoided-failure rate
0; Naive Warning precision 0.4, false-positive/unnecessary-intervention rate 0.6,
and avoided-failure rate 0.4; Hardknock Predictive precision/recall 1.0 within the
retry profile, false-positive rate 0, two-action median lead, avoided-failure rate 1.0,
and unnecessary-intervention rate 0. All arms eventually succeed because Reactive can
recover after failure; only the predictive arm avoids the known failures without warning
spam.

## Quality gates

`cargo fmt --check` and
`cargo clippy --all-targets --all-features -- -D warnings` pass. `cargo test --all`
passes 256 tests with two explicitly optional local-Codex integration tests ignored.
The existing process-control tests were run outside the managed shell sandbox because
they intentionally inspect and terminate child processes. Predictive defaults are
deterministic, network-free, and external-model-free.

## 37. Known limitations

- Simple ordered conditions are implemented, not LTL/CTL or a complex event processor.
- Forecast scope transfer is exact and conservative. It can miss useful warnings rather
  than silently generalize them.
- Bridge maintains compact live trajectories and uses the no-history fast path. Validated
  signatures and preventive actions use a process-local hot cache refreshed from indexed
  SQLite; there is no distributed cross-process cache-coherence protocol.
- Calibration is deterministic aggregate accounting, not confidence intervals or a
  probabilistic risk model. Sparse operating-envelope points are never interpolated.
- Capability escalation and unknown-commit fixture families exercise the generic typed
  event model; specialized automated intervention validation remains explicit fixture work.
- No production external mutation is executed. Git worktrees are cooperative isolation,
  and trusted fixture execution requires `--trusted-local`.

## 38. Deviations and rationale

The requested traits receive the event slice through `ForecastContext` because an
`ExecutionTrajectory` deliberately stores references, not duplicated raw events.
Fast-path evaluation uses evidence already attached to a validated signature and skips
historical scans; explain/replay can run structural history matching on the slow path.
The initial sufficient calibration count is two resolved samples. Forecast impact reports
runtime influence conservatively; it does not infer causality from a decision timestamp.

Deferred exactly as requested: generic anomaly ML, neural sequences, telemetry streaming,
reinforcement-learned policy, unattended external mutation, probabilistic causal forecasts,
global services/marketplaces, full observability platform, and GUI.

## 39. Recommended V0.16 direction

Add long-horizon `PlanTrajectory`, milestones, commitments, delayed effects, temporal
dependencies, checkpoints, plan invariants, goal drift, and delayed failure signatures.
Preserve the same discipline: observable commitments, held-out episodes, explicit delayed
ground truth, governance precedence, and abstention when early local actions cannot yet be
linked to long-horizon outcomes.
