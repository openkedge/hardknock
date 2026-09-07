# V0.16 implementation report

This report covers the local V0.16 experience-economics implementation. Results
are scoped to deterministic repository fixtures on macOS; no network or external
model was used.

1. **Files created/changed.** Added `economics::{model,engine,benchmark}`, its store
   and CLI adapters, migration 020, 14 economics fixtures, economics integration
   tests, this report, and the experience-economics guide. Updated budget, core IDs,
   profile snapshots, architecture, CLI reference, roadmap, README, and affected
   budget literals.
2. **Schema migrations.** `020_experience_economics.sql` creates opportunity,
   portfolio/revision, selection/deferral, ledger, result, cost, saturation, debt,
   and event tables. Revisions and results have immutable update/delete triggers.
3. **ExperienceOpportunity model.** A stable typed ID joins kind, target, reasons,
   value vector, estimated cost, risk, dependencies, lifecycle, saturation,
   marginal value, explanation fields, timestamp, and generator version.
4. **Opportunity-generation rules.** Deterministic projections cover runtime gaps,
   forecast misses and warning health, unresolved causal hypotheses, Lessons,
   Recoveries, Reflexes, epistemic Claims, Skills/envelopes/assurance, broad tool
   grants, and locally relevant federated evidence.
5. **ExperienceValueVector semantics.** Risk reduction, learning value, reuse,
   decision relevance, evidence gap, and novelty remain separate categorical bands.
6. **Risk-reduction model.** Severity, occurrence exposure, mitigation gap, and a
   textual rationale explain the prospective reduction; no probability is invented.
7. **Reuse/decision-relevance model.** Records task-family breadth, affected runtime
   decisions, present use, and likely decision-change band.
8. **ExperimentCost model.** Separately estimates trials, agent runs, duration,
   compute, human attention, staging resources, and effect risk.
9. **Actual-cost tracking.** Results append actual trials, agent runs, duration,
   compute units when known, and approvals. The ledger records estimate overruns.
10. **EvidenceSaturation policy.** Contradiction wins first; saturation otherwise
    requires context, counterfactual, diversity, and replication evidence.
11. **MarginalEvidenceValue policy.** First evidence/counterfactual/context,
    contradiction, replication, repeated replication, and saturation are explicit.
12. **ExperienceBudget changes.** Added human-approval and effect-risk caps while
    preserving old Reality, command, duration, curriculum, and parallel fields.
13. **Budget reservation ledger.** Reserve, release, and actual consumption are
    dimension-wise and saturating; oversubscription is rejected before selection.
14. **ExperiencePortfolio design.** Stores all candidates, selected and deferred
    work, original budget, live ledger, policy/objective/version references, and
    revision reason.
15. **Deterministic allocation policy.** Eligibility gates precede documented
    lexicographic ranking; stable IDs resolve final ties.
16. **Priority-class semantics.** Critical unmitigated risk precedes contradiction,
    recurring failures, stale active evidence, abstention, assurance, causal and
    diversity gaps, capability minimization, and mature replication.
17. **Dominance handling.** Within a priority class, a strictly Pareto-dominated
    candidate is deferred with the dominating opportunity ID.
18. **Opportunity dependencies.** Unmet typed opportunity IDs cause an explained
    deferral. Completed results populate the dependency set on replan.
19. **Adaptive replanning.** Completion releases other reservations, charges actual
    cost, adds newly discovered candidates, and writes a compare-and-swap revision.
20. **Early-stop semantics.** Objective satisfaction, saturation, contradiction,
    unsafe execution, and budget exhaustion are distinct stop reasons; unused
    reservation returns to the ledger.
21. **Runtime integration.** Repeated abstentions/unknowns and their occurrence count
    become decision-relevant acquisition gaps.
22. **Causal integration.** Unsupported or contradicted hypotheses generate
    discriminating or contradiction-resolution work. A discriminating intervention
    receives priority over another same-risk replay.
23. **Epistemic Diversity integration.** Claim reports with missing controlled,
    evaluator, source-type, retrieval, or metadata diversity generate bounded
    `IncreaseEvidenceDiversity` opportunities.
24. **Forecast integration.** Misses, candidate warnings, staleness, degradation,
    and contradiction generate different opportunity kinds and costs.
25. **Assurance integration.** Supported/validated Skills without local certificates
    generate named assurance-gap opportunities; exposure and evidence affect rank.
26. **Capability integration.** Network-capable tools generate one-trial narrowing
    candidates with broad reuse. Passing such a trial does not itself revoke access.
27. **Federation integration.** Only context-matched or reproduction-recommended
    objects enter the backlog; local reproduction stays advisory and cheaper than
    rediscovery in the deterministic comparison.
28. **Experience Debt model.** Reports unresolved Medium-or-higher, meaningfully
    exposed, incompletely mitigated gaps and their age. It is not code debt.
29. **CLI commands.** `explore plan/run/status/show/why/report/history/replay/benchmark`
    and `experience debt` are functional with structured JSON.
30. **Saturation test results.** A counterfactual gap ranks above 12 equivalent
    passing replications; saturated-only work consumes zero of five trials.
31. **Critical-budget test results.** One four-trial Critical gap reserves the full
    four-trial budget ahead of four one-trial Low candidates.
32. **Contradiction/blast-radius results.** One contradiction resets mature evidence
    and gives Critical marginal value; the 100-use target breaks the tie.
33. **Reuse/cost tests.** Broad reuse wins at equal risk/cost; lower cost wins only
    after higher-order dimensions tie. Direct dominance is asserted.
34. **Adaptive replanning results.** After A completes and reveals Critical D, the
    second revision selects D and defers B/C within the exact remaining budget.
35. **Early-stop results.** A four-trial reservation resolving after one consumes
    one and releases three; the stop reason is `StopSatisfied`.
36. **No-spend result.** A fresh saturated Low/Rare opportunity leaves all five
    available trials unused.
37. **Federated reproduction result.** A one-trial local reproduction wins over a
    same-value three-trial rediscovery without changing trust semantics.
38. **Causal discrimination result.** The intervention that separates H3 wins over
    repetitive confirmation in the fixture.
39. **Forecast-priority result.** Missing-warning validation wins over another
    replication of an already mature Recovery under equal severity/exposure.
40. **Capability-minimization result.** A one-trial narrowing test for a very
    frequently used tool wins over an equally uncertain rare gap.
41. **Fixed-budget comparative benchmark.** With 20 trials and five agent runs,
    round robin/static/adaptive close 0/2/2 Critical gaps and spend 4/5/0 trials on
    saturated evidence. Material-learning rates are 0.30/0.75/1.00.
42. **Held-out runtime results.** Deterministic task success is 0.62/0.84/0.87;
    repeated-failure rates are 0.28/0.06/0.02; adaptive avoided-failure is 0.38.
43. **Actual vs estimated cost findings.** Lower actual cost is released into the
    next revision. A three-trial actual against a one-trial estimate records three
    consumed and zero remaining rather than concealing the overrun.
44. **Known limitations.** Values and costs are hand-authored deterministic
    heuristics; instrumentation can be incomplete; no allocation policy is learned;
    benchmark outcomes are synthetic; live provider isolation acceptance remains.
45. **Deviations and rationale.** Existing budget duration remains milliseconds for
    compatibility. Executable plans contain typed target/intent descriptors rather
    than cloning engine request models; concrete target engines must supply fixture
    inputs. `explore run` stops at that explicit handoff boundary and never invents
    commands or effects. Dominance is limited to a priority class to avoid allowing
    cheap low-risk work to suppress Critical work.
46. **Recommended V0.17 direction.** Experience Abstraction, Distillation, and
    Cross-Context Transfer: reuse validated findings across contexts without erasing
    applicability, evidence provenance, contradiction, freshness, or trust.

## Verification

The V0.16-focused integration target contains 13 passing tests. The complete
`cargo test --all-targets` run passed 270 tests; two optional installed-Codex/model
tests were ignored by design. `cargo fmt --all -- --check` and
`cargo clippy --all-targets -- -D warnings` both passed. The deterministic benchmark
is network-free and reports actual fixture denominators.
