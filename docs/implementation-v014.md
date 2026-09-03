# V0.14 end-of-pass report

Version: `0.14.0-dev.1`. Scope: explicit, local, controlled causal hypotheses.
No general causal discovery, hidden reasoning extraction, or production intervention.

## 1. Files created and changed

Created:

- `src/causal/{mod,model,planner,engine,benchmark}.rs`
- `src/store/causal.rs`, `src/cli/causal.rs`, `migrations/017_causal.sql`
- `tests/causal.rs` (16 integration/regression tests)
- `fixtures/causal/{stale-state,confounded-latency,interaction,wrong-agent-explanation,recovery-mechanism,scope-split,federated-causal,causal-diversity,invalidation-propagation,untestable}/{scenario.sh,fixture.json}`
- `docs/causal-experience.md`, this report, `docs/benchmarks/causal-v014.json`

Changed:

- `Cargo.toml`, `Cargo.lock`, `README.md`, `docs/architecture.md`
- `src/{core,lib,cli,store,reflection,retrieval}.rs`
- `src/experimentation/{model,orchestrator}.rs`
- `src/runtime/{model,context,policy,scenario}.rs`, `src/store/runtime.rs`
- `src/resilience/{models,campaign}.rs`, `src/curriculum/model.rs`
- `src/assurance/{model,evaluator}.rs`, `src/store/assurance.rs`
- `src/bridge/{cache,protocol}.rs`
- `src/epistemic/{model,policy}.rs`, `src/store/epistemic.rs`
- `tests/learning.rs`, `tests/substrate.rs` (current schema assertions)

The v0.13 integration needed prerequisite repairs: missing type imports and an
Experience revision field, misplaced assurance-summary fields, Clippy warnings,
and a planner tie-break that ignored an equally dominant injected Lesson dependency.
These repairs preserve the existing intended behavior; the existing regression test
now passes. A temporary Rust toolchain was used without changing shell configuration.

## 2–12. Domain, experiment and model semantics

| Item | Implementation |
| --- | --- |
| 2. Migration | Schema 17 adds variables, hypotheses/status revisions, conditions, investigations, interventions, pairs, causal evidence, model revisions/edges/gaps, artifact dependencies, revalidations, refinements, remote claims, observations and events. Evidence/definitions/history are protected by append-only or immutability triggers; existing Experiments and Experiences are referenced. |
| 3. Variables | Typed IDs; action/environment/configuration/state/perturbation/tool/agent/outcome/intermediate kinds; Boolean, categorical, integer and finite float domains; separate observable/intervenable flags. Custom domains remain descriptive. |
| 4. Hypotheses | Explicit cause, outcome, claim, ContextSelector, conditions, categorical predictions, origin, evidence and timestamps. Candidate status is enforced at ingestion. Structured equivalent claims deduplicate; no “Proven” status exists. |
| 5. Interventions | One declared input literal changes; all other bound inputs are held constant. Safe-value and actual Reality capability checks precede execution. Commands/evaluator are identical between candidates. |
| 6. Counterfactual pairs | Reference baseline and intervention candidate/Experience IDs and the engine's starting proof. Never create a parallel execution engine. Replay appends new evidence and checks the old fingerprint. |
| 7. Confounders | Explicit known confounders must be controlled and represented among changed/held variables. Unknown confounders are not claimed absent. |
| 8. Quality | Non-equivalent starts or inconsistent evaluators → Invalid. Multiple changed variables or uncontrolled known confounders → Confounded. Partial engine control cannot become Controlled. Setup/execution failure is not a causal counterexample. |
| 9. Planner | Deterministic, capability- and budget-constrained single-variable search over explicitly available values. Three interventions by default; one-variable maximum; total reserved duration is allocated across pairs. |
| 10. Discrimination | Maximizes the number of differently predicted hypothesis pairs; stable state-variable/name/value tie-breaking. Unknown predictions earn no separation credit. The first stale-state intervention separates H3 from H1/H2. |
| 11. Support policy | Default one controlled informative pair → Supported. Three distinct experiment replications plus Moderate evidence diversity → StronglySupported. A controlled contradiction is retained, not outvoted. Necessary/sufficient/risk/mediation claims remain inconclusive without the required matrix/estimator. Thresholds are configurable via the Rust policy. |
| 12. Models | Append-only scoped revisions include variables, edges, conditions, exact tested input points, evidence and known gaps. Shared hypotheses cause affected model views to be revised after new evidence. |

## 13–23. Downstream integration

| Item | Behavior |
| --- | --- |
| 13. Operating envelopes | `EnvelopeObservation` carries a map of joint input values and trial references. `OperatingEnvelope` has a backward-compatible causal-observation field; `causal envelope` exposes the investigation's measured points. No interpolation or extrapolation. |
| 14. Lesson refinement | `causal refine` persists an investigation-linked revision candidate; it never changes/promotes an existing Lesson. |
| 15. Reflex refinement | The candidate includes a more precise conditional trigger recommendation. Activation still requires the existing Reflex validation lifecycle. Benchmark rules are evaluated, not activated. |
| 16. Recovery selection | An explicitly linked supported intervention prioritizes a matching fresh Recovery; otherwise runtime recommends REPLAN with the tested intervention. Arbitrary Recovery code is not automatically semantically verified by a dependency link. |
| 17. Runtime | Matches failure, scope, repository version and exact observed input values. Unknown high-risk mechanisms recommend safe experiments or abstention. Hard policy/capability/isolation retain precedence. Decisions record their causal basis and downstream links. Failure lookup is indexed. |
| 18. Curriculum | Adds `ResolveCausalContradiction` and `DiscriminateHypotheses`; `causal curriculum` provides explicit goals. They do not automatically launch work or spend an implicit budget. |
| 19. Diversity | Uses V0.13 dependency assessment with evaluator hashes and recorded environment/fixture fingerprints. Three identical trials stay Supported; distinct evaluator and fixture revisions can yield StronglySupported. |
| 20. Federation | Remote reported support/root origin is preserved as advisory metadata; local status starts Candidate with no imported evidence credit. Local contradiction does not rewrite remote history. Signed-package automatic causal embedding is deferred. |
| 21. Assurance | Optional `CausalFailureCoverage` counts locally supported mechanisms explicitly linked to the selected Skill at the configured severity. Missing coverage is inconclusive. Basic certification is unchanged. Causally dependent runtime certification guidance can require review. |
| 22. Invalidation | Appends review/revalidation events, preserves artifact history, and quarantines dependent automatic guidance. Retrieval/runtime and cache reload honor quarantine; live persisted runtime decisions recheck it. |
| 23. CLI | `causal list/show/explain/investigate/demo/plan/test/compare/impact/replay/refine/link/envelope/curriculum/benchmark`, `causal model list/show/history/graph/diff`, and `provenance causal-ID`. Execution requires explicit trusted-local acknowledgment. |

## 24–33. Measured tests and comparative results

All causal integration tests are deterministic, network-free and external-model-free.
The recorded standalone run is [causal-v014.json](benchmarks/causal-v014.json).

| Item | Result |
| --- | --- |
| 24. Stale-state diagnosis | Three real paired experiments: refresh → PASS; removing latency → FAIL; more retries → FAIL. State explanation Supported; latency/retry explanations Contradicted. The first intervention discriminates stale state from both alternatives. |
| 25. Confounding | Uncontrolled tool-version confounder gives Confounded evidence and no support. The quality policy also rejects a two-variable change as isolated evidence. |
| 26. Interaction | Latency with retry pressure can fail; without retry pressure, changing latency alone leaves PASS. An unqualified sufficiency claim stays Inconclusive. A condition-qualified hypothesis can gain support. |
| 27. Wrong explanation | An agent-proposed memory-pressure explanation is contradicted when changing it fails to prevent the failure. No Lesson is created/promoted. |
| 28. Mechanism-guided Recovery | Held-out latency=2000/retries=5: increasing retries fails; refreshing state succeeds. Runtime selection tests prefer the explicitly linked Recovery over the generic one. |
| 29. Reflex precision | Three actually evaluated healthy input points: latency-based rule gives 2 false positives (66.7%); stale-state rule gives 0 (0%). These are finite fixture results, not population estimates. |
| 30. Scope contradiction | Refresh works with the dependency available and fails when unavailable. Status becomes Contradicted; both pairs persist; differing input values are exposed for explicit scope refinement. Untested context cannot inherit support. |
| 31. Federated claim | Remote StronglySupported becomes local Candidate; differing local implementation produces local Contradicted while the remote reported status remains preserved. |
| 32. Invalidation | Real stored Lesson, Reflex and Recovery dependencies all receive review/revalidation and are quarantined after contradiction; original artifact statuses/history are unchanged. |
| 33. Comparative benchmark | Correlation learner: task/recovery success 0, repeated failure 1, challenged spurious claim rate 1. Strategy counterfactual and causal policies both achieve success 1 and repeated failure 0; only causal policy retains the explicit tested mechanism. No manufactured success advantage over the strategy policy. |

The benchmark compares explicit deterministic decision policies using real paired
Experiment outcomes, not autonomous LLM populations. Rates have tiny, reported
fixture denominators. Paired engine durations in the standalone run were 431 ms
(generic retry) and 436 ms (refresh); these are not production time-to-recovery.
The lookup test measured 100 hypothesis/model/impact batches in roughly 14–21 ms
on the local debug build. This is a small-store smoke measurement, not a scaling SLA.

Additional tests cover domain validation, unsafe path binding, actual provider
requirements, zero/cumulative budgets, symlink setup failure, append-only storage,
database integrity, evidence separation, model diffs, CLI execution/replay and
hard-policy precedence. Empty new fields are omitted from legacy hashed runtime
contexts, assurance summaries and envelopes to preserve their serialization.

## Quality gates

All required gates passed on the final implementation: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all`,
including all 16 causal integration tests. Optional live Codex/model tests remain
ignored by the project's default test configuration. `git diff --check` also passed.

## 34. Known limitations

- Explicit trusted local file-bound adapters only; Git worktrees are cooperative
  repository isolation, not a security sandbox. No production mutations are authorized.
- No exhaustive necessity/sufficiency matrix evaluator, mediation/risk estimator,
  unknown-confounder discovery, causal graph search, or statistical identification.
- Runtime matching is deliberately exact and version-sensitive. It can miss useful
  transfer; it must not invent it. Candidate conditions and artifact links remain explicit.
- Model revisions happen after atomic evidence/status recording. An interrupted
  model projection can be rebuilt with `Store::revise_causal_model`; current runtime
  support reads canonical hypotheses/evidence, not a stale model projection.
- Existing raw Bridge cache content updates on refresh; persisted runtime decisions
  recheck quarantine. There is no push broadcast to every connected agent's context.

## 35. Deviations and rationale

Single-variable pairs implement the first release; pairwise interactions are expressed
as conditions across separate pairs, not simultaneous multi-variable intervention
search. This keeps isolation semantics testable. Scope contradictions are marked
disputed with refinement candidates rather than silently narrowing the original claim.

Lesson/Reflex/Recovery refinement is stored as a reviewable candidate, not automatic
code rewriting or activation. Curriculum integration exposes goals rather than
implicitly executing them. Observations are extracted only from declared structured
environment facts; arbitrary prose and every perturbation adapter are not auto-parsed.

Optional agent MCP proposal tools, automatic signed causal federation packages,
general interaction search and GUI remain deferred. No model calls or agent voting
were needed for the fixture suite. The implementation remains one Rust package and
reuses the existing Experiment Engine and artifact validation boundaries.

## 36. Recommended V0.15 direction

Predictive Experience: record trajectories and calibrated early-warning indicators
around these explicitly tested mechanisms. Evaluate forecasts and least-disruptive
preventive interventions on held-out trajectories. Keep false-alarm rates, horizon,
scope and abstention explicit; do not turn this into an ungrounded anomaly detector.
