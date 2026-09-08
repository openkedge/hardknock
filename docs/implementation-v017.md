# Hardknock V0.17 implementation report

V0.17 is implemented locally as an empirical, model-free abstraction and
cross-context transfer layer. This report is the completion inventory; it does
not claim universal generalization or production safety.

1. **Files created/changed.** Added `src/abstraction/{model,engine,benchmark}.rs`,
   `src/store/abstraction.rs`, `src/cli/abstraction.rs`, migration 021, twelve
   abstraction fixture families, integration tests, this report, and the
   abstraction guide. Updated core IDs, runtime/development/economics/curriculum,
   federation, CLI dispatch, store migration wiring, README, architecture, CLI
   reference, package version, and roadmap.
2. **Schema migrations.** Migration 021 stores patterns and members, versioned
   abstract knowledge, transfer hypotheses/evidence/evaluation sets, boundaries,
   specializations, exceptions, distillations, representation state, negative
   transfers, analogy mappings, and abstraction events. Revision/evidence tables
   that must retain history reject update/delete.
3. **ExperiencePattern model.** Typed patterns retain members, shared
   `PatternStructure`, scope, status, evidence, and timestamps.
4. **Structural pattern discovery.** The deterministic provider hashes typed
   trigger/action/outcome/required-condition/causal structure while treating
   resource, provider, tool, runtime, and version dimensions as possible
   variables. It never uses prose similarity as support.
5. **AbstractKnowledge model.** Versioned objects retain kind, statement,
   applicability, generalization boundary, pattern/evidence links,
   specializations, exceptions, maturity, risk, and provenance.
6. **AbstractLesson semantics.** Cross-context operational guidance requiring
   local held-out behavioral support.
7. **AbstractSkill semantics.** Reusable procedure structure; source Skills and
   transfer behavior remain independently inspectable.
8. **AbstractConstraint semantics.** Candidate prohibition/invariant with high
   generalization risk, mandatory negative control, and no automatic guard.
9. **AbstractAntiPattern semantics.** Reusable failure-producing action shape
   with the same false-positive control requirement as Constraints.
10. **AbstractRecovery semantics.** Reusable recovery mechanism whose transfer
    must succeed in a distinct failure context.
11. **ContextVariable representation.** Explicit dimension, value, and
    relevance types cover environment, semantics, idempotency, concurrency,
    version, resource, runtime, tool, and failure variables.
12. **TransferHypothesis model.** Links one abstraction revision to source
    contexts, one target context, an expected behavior, status, and evidence.
13. **Held-out context semantics.** Source contexts cannot be recorded as
    held-out evidence. Evaluation sets persist the separation.
14. **Negative-control semantics.** Controls model contexts where knowledge
    should not fire; false constraint application narrows rather than supports.
15. **TransferEvidence lifecycle.** Append-only paired-trial records preserve
    role, expected applicability, observed triggering, outcome, context delta,
    experiment quality, diversity, boundary clause, locality, and time.
16. **GeneralizationBoundary model.** Included, excluded, and unknown clauses
    stay separate and link to the evidence that established them.
17. **Scope narrowing.** Counterexamples create a new overgeneralized revision,
    exclusions, and typed exceptions without mutating source evidence.
18. **Specialization model.** Parent/child revision links retain additional
    applicability scope and evidence.
19. **Exception model.** Exceptions retain precise context, predicate, reason,
    evidence, and parent revision; runtime gives them highest precedence.
20. **Abstraction promotion policy.** Promotion requires diverse root origins
    and controlled local held-out support; risky kinds also require a clean
    negative control. Outcomes are promote, require evidence, narrow, or reject.
21. **Knowledge-distillation model.** Distillation manifests link input
    artifacts, output abstractions, preserved exceptions, and evidence manifest.
22. **Provenance preservation.** Abstract revisions retain all source artifact,
    source-context, evidence, pattern, and root-origin references. Assurance
    manifests pin abstraction revisions, transfer evidence, applicable
    specializations, and exceptions used by a Skill.
23. **Representation/reactivation.** Specific artifacts may become
    `represented_by_abstract` but are never deleted; contradiction can reactivate
    direct representation.
24. **Runtime resolution policy.** Deterministic bounded resolution uses facts,
    markers, tags, and scope, with exception/specific/specialization/abstract
    precedence and explicit unknown conditions. Persisted uses emit an
    `abstract_knowledge_applied` event for impact and Bridge inspection.
25. **Causal-model integration.** Pattern structure carries typed
    `CausalHypothesisId` references and the discovery signature preserves causal
    basis.
26. **Epistemic-diversity integration.** Transfer quality carries the existing
    `DiversityClass`; root-origin counting prevents correlated descendants from
    satisfying the promotion gate.
27. **Curriculum integration.** Added challenge-abstraction,
    find-generalization-boundary, validate-transfer, and reduce-false-constraint
    goal kinds.
28. **Experience Economics integration.** Added validate/challenge/transfer/
    fragmentation opportunities, high-reuse prioritization, and typed routing to
    Curriculum or Experiment engines.
29. **Federation behavior.** Signed bundles can carry validated local
    abstractions. Import is advisory; remote origin cannot be exported as local,
    and remote evidence alone cannot promote.
30. **GuardCandidate/OpenKedge boundary.** Guard assessment is advisory.
    Automatic enforcement is always false and requires independent external
    governance and authorization.
31. **Positive-transfer benchmark.** The authoritative-state held-out fixture
    succeeds under empirical abstraction where specific-only retrieval has no
    matching item.
32. **Negative-control benchmark.** Exact-idempotency and read-only controls keep
    the broad mutation rule from firing.
33. **Constraint overgeneralization results.** A false constraint returns
    `narrow_scope`, adds an exclusion/exception, and cannot auto-enforce.
34. **AntiPattern transfer results.** Typed AntiPattern structure transfers only
    across matching mechanism/conditions and uses the risky-kind control gate.
35. **Recovery transfer results.** Matching recovery-mechanism fixtures support
    held-out transfer while retaining their original recovery records.
36. **Negative-transfer results.** False constraint, harmful Skill, failed
    Recovery, misleading Lesson, unnecessary replan, and incorrect abstention
    are explicit durable outcome types and feed future economics gaps.
37. **Causal-basis test.** Differently worded artifacts with one shared causal
    mechanism group; same-wording artifacts with different mechanisms do not.
38. **Misleading-similarity test.** The naive benchmark arm transfers on lexical
    similarity and incurs negative transfer; the empirical arm rejects it.
39. **Staleness behavior.** Member health yields fresh, partially stale, stale,
    or contradicted abstraction status without deleting revisions. Forecasts
    transferred from a validated AbstractAntiPattern remain inactive candidates
    until the predictive subsystem validates the target context.
40. **Retrieval-compression results.** The fixture benchmark injects fewer
    runtime items under empirical abstraction than under specific-only lookup.
41. **Held-out behavioral results.** Transfer plans require an equivalent-start
    two-candidate budget and use `ValidateTransfer`; source contexts are excluded.
42. **Comparative benchmark.** The deterministic command compares specific-only,
    naive semantic, and empirical abstraction across three fixture families; its
    reported scientific claim is scoped to those fixtures.
43. **Performance results.** Runtime resolution makes zero model/network calls;
    a regression test performs 1,000 empty bounded resolutions in under two
    seconds on the test host. This threshold is a guardrail, not a latency SLO.
44. **Known limitations.** Candidate discovery currently consumes persisted
    Lessons, Skills, and Recoveries; canonical Constraint/AntiPattern/causal/
    trajectory adapters can also use the public model but are not all backed by
    first-class stores. Transfer CLI emits the typed Experiment handoff rather
    than inventing repository state or evaluators. Benchmark scenarios are
    deterministic fixtures, and no live provider or population study is claimed.
45. **Deviations and rationale.** The optional LLM abstraction provider is
    omitted to keep default behavior reproducible and prevent semantic prose from
    becoming evidence. Trial references are typed but deliberately not copied
    into abstraction tables. OpenKedge integration stops at an assessment because
    enforcement authority belongs outside empirical learning.
46. **Recommended V0.18 direction.** Build hierarchical operational knowledge
    with explicit inheritance, specialization, exception, contradiction, scope
    precedence, and staleness propagation so stronger local evidence always wins
    over a broad ancestor.

See [experience abstraction](experience-abstraction.md) for operation and safety
semantics and the [project roadmap](roadmap.md) for the next phase.
