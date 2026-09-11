# Hierarchical operational knowledge (V0.18 Pass 1)

General knowledge supplies defaults. More specific knowledge can change those defaults only when its scope is applicable, its activation is Active, its maturity meets the policy, its freshness is Fresh, and its provenance contains evidence. Contradicted knowledge is excluded. The engine is deterministic Rust; it uses no model, embedding service, or graph database.

**An exception is evidence-backed operational knowledge, not permission.**

**A Hardknock exception cannot override an external OpenKedge policy.**

This pass exposes a library and CLI. It does not change RuntimeController, V0.12 retrieval, Curriculum, Experience Economics, decision replay, or production Guard policy.

## Domain and compatibility

`hardknock::hierarchy` owns `KnowledgeHierarchy`, nodes, edges, scopes, validation, and effective resolution. A hierarchy has an ID, name, revision, timestamps, multiple roots, a `BTreeMap` of nodes, and explicit edges. Independent operational principles need no artificial common root. Shared children are permitted: the precedence graph is a DAG, with a forest-shaped presentation where appropriate.

Nodes reference existing artifacts through V0.17 `KnowledgeArtifactRef { kind, id, revision }`. Its existing Lesson, Skill, Constraint, AntiPattern, Recovery, CausalMechanism, and FailureTrajectory variants remain; `AbstractKnowledge(AbstractKnowledgeKind)` adds abstract references with enough type information for compatibility checks. No Lesson, Skill, Constraint, AntiPattern, Recovery, or AbstractKnowledge artifact is replaced.

The engine reuses `KnowledgeMaturity`, `KnowledgeProvenance`, and `EvidenceRef`. `KnowledgeActivationState` is an alias for `ExperienceActivationState`; `FreshnessStatus` aliases `AbstractionFreshnessStatus`, extended with Unknown. Maturity uses explicit support-stage ranks; Contradicted, Overgeneralized, Stale, and Retired never rank above Validated merely because of enum declaration order.

Adapters convert existing `ApplicabilityPredicate` and `ContextSelector` into structured scopes. Unsupported set, absence, and generalization-boundary clauses become Custom predicates, preserving uncertainty. `from_abstract` requires callers to supply assessed freshness and activation. Existing `KnowledgeSpecialization` and `KnowledgeException` relations can be projected with `from_specialization` and `from_exception`, including evidence, scope, and revision checks. An old exception has no independent lifecycle or directive artifact, so its adapter requires an explicit child node. It does not manufacture validated knowledge.

All new IDs use the existing prefix/canonical-UUID macro in `core.rs`.

## Edge orientation and operational semantics

Every precedence edge points from parent to child. In particular, **Supersedes points from old knowledge to its replacement**, and **DependsOn points from prerequisite to dependent**.

| Relation | Meaning when both nodes are eligible |
| --- | --- |
| Specializes | Child expresses the principle within a contained scope. Child is Primary; parent remains SupportingContext. |
| Refines | Child supplies additional detail. Both remain applied; child is Refinement. |
| Excepts | Child explicitly changes the parent directive. Child is Exception; parent is suppressed. |
| Supersedes | Explicit scoped replacement. Old parent is suppressed; replacement applies. Creation time has no precedence. |
| DependsOn | Prerequisite health affects the dependent. It does not establish inheritance precedence. |

An artifact reference is the smallest suppressible unit. To suppress only part of an artifact, model that directive as its own node. The fixture explicitly connects the replay exception to each conflicting reconciliation directive; no prose analysis or implicit ancestor-wide suppression occurs.

Relationships that change precedence need evidence. Exception and specialization scope containment must be proven. An unknown relationship cannot suppress a parent. A stale, contradicted, missing, unknown, or inactive health prerequisite prevents dependent authority; dependency cycles fail closed with a validation warning and unknown dependent health.

## Scopes, contexts, and applicability

`KnowledgeScope.predicates` is a conjunction. Built-ins are Equals, NotEquals, In, Exists, Bool, inclusive IntegerRange, and semver VersionRange. Ordinary values are typed String, Integer, Boolean, or Version. A missing key is Unknown for **every** predicate, including Exists and NotEquals. It is never a match.

Custom predicates have no registered evaluator in this pass and always return Unknown. Malformed version requirements, malformed version values, and inverted integer ranges cannot establish applicability. Ordinary type mismatches reject the predicate.

`KnowledgeContext.values` is a stable ordered map, independent of RuntimeDecisionContext. Library callers can construct it from experiment or runtime data without linking the resolver to those systems. CLI contexts accept primitive JSON objects:

```json
{
  "provider": "provider-x",
  "api_version": "2",
  "action_type": "http_mutation",
  "idempotency": "exact",
  "token_valid": true
}
```

Typed contexts also accept `{"values":{"dependency_version":{"type":"version","value":"2.1.0"}}}`. Semver values must include major, minor, and patch; arbitrary API identifiers such as `"2"` use ordinary Equals or In predicates.

Canonical keys cover task_family, environment, provider, resource_type, api_version, dependency_version, tool_version, action_type, effect_type, idempotency, reversibility, consistency_model, credential_state, and runtime. The resolver contains no application-specific values.

| Status | Conjunction result |
| --- | --- |
| Applicable | Every required predicate true; an empty scope is universal. |
| Inapplicable | At least one false, even if others are unknown. |
| PartiallyKnown | At least one true and at least one unknown; none false. |
| Unknown | No predicate establishes applicability and at least one is unknown. |

The result retains matched, failed, and unknown predicates in canonical order. A more specific Unknown child cannot override an Applicable parent.

## Specificity and scope comparison

`ScopeSpecificity` orders distinct exact dimensions, then bounded dimensions, then existence dimensions. Duplicate predicates do not increase specificity. The resolver does not use this tuple as an arbitrary winner selection rule.

Scope comparison normalizes conjunctions into per-key domains: finite value sets, excluded values, integer intervals, existence constraints, and version requirements. Repeated constraints intersect. Containment across every dimension establishes Equal, Narrower, or Broader; an empty intersection establishes Disjoint. Proven intersections without containment are Overlapping. Unsupported combinations return Unknown.

Stable-release semver comparator conjunctions are reduced to half-open intervals, supporting exact, wildcard, inequality, caret, and tilde requirements. Prerelease requirement relationships remain Unknown unless identical constraints prove containment. Applicability still uses the semver crate's full matching semantics. Arbitrary Custom predicates are never treated as equal merely because their JSON is identical.

## Validation

`validate_hierarchy` returns `valid`, ordered `errors`, and ordered `warnings`, each with kind, message, and node IDs. Resolution rejects reports with errors.

Validation checks self-edges, missing endpoints/roots, duplicate edge identities/relations, node-map identity mismatches, malformed empty artifact references/revisions, exact root membership, precedence cycles (including mixed relation cycles), supersession cycles, scope containment, and artifact compatibility. Iterative topological traversal avoids recursion limits. Dependency edges do not alter roots or normal inheritance cycles.

Specializations must be Equal or Narrower; obvious broader, overlapping-but-uncontained, and disjoint scopes are errors. Exceptions use the same conservative containment requirement, including rejection of disjoint scope. Unknown comparisons warn and cannot establish precedence. Concrete kinds must match; abstract kinds map to their concrete family. DependsOn can cross kinds, including Recovery depending on CausalMechanism.

Validation checks structural references. Evidence IDs and artifact IDs refer to caller-owned catalogs; this pass does not independently revalidate external evidence or hydrate every artifact from the runtime store. Node health must be assessed by the caller. Persisting a hierarchy is not evidence promotion.

## Resolution and trace

The resolver performs validation, applicability evaluation, lifecycle eligibility, dependency-health filtering, scoped supersession, exceptions, specialization, refinement, conflict reporting, and output materialization. Indexed incoming/outgoing edges and stable topological order avoid scanning all edges for every node.

Only Active, sufficiently mature, Fresh, non-contradicted, evidence-backed nodes enter authoritative precedence. Candidate and stale nodes can appear in a separate `advisory` collection when policy permits. Quarantined and Disabled nodes never appear as active guidance. Unknown freshness or evidence state is reported in `unknown`.

Default policy requires Validated maturity and permits stale/candidate advisory visibility. `stale_exception_can_override` defaults to false; requesting true is rejected in Pass 1 because the mandatory freshness boundary always applies.

Multiple eligible sibling specializations, exceptions, or replacements conservatively produce a conflict. Conflicting alternatives remain SupportingContext; the default parent remains authoritative. No creation-order or ID-order tie breaker selects guidance. Independent sibling refinements compose. Explicit supersession is processed before specialization and can remove a competing older rule. A chain of supersessions records the live final replacement as suppressor. Sibling conflict detection occurs before mutations within its phase to avoid multi-parent traversal-order overrides.

`EffectiveKnowledge` contains applied, advisory, suppressed, unknown, conflicts, and trace. Applied entries include artifact, node, role, applicability, and ordered ancestor lineage. Suppressed entries include reason and suppressor. Conflicts include participants, kind, and reason. The minimal model conservatively detects explicit structural competitors; it does not infer contradictory prose or cross-root semantic conflicts. The additional conflict kinds reserve vocabulary for later richer explicit guidance models.

Each `KnowledgeResolutionStep` has a contiguous sequence starting at 1, node ID, action, and reason. Actions include candidate discovery, scope match/rejection/unknown, lifecycle rejection, applied/refinement/exception, suppression, supersession, and conflict. IDs determine stable presentation order only. Timestamps and random values are never generated during resolution.

### Case 1: Validated specialization

An eligible HTTP specialization of a general reconciliation rule becomes Primary. The general rule remains SupportingContext, and its ID appears in lineage. A more detailed but unknown specialization leaves the general rule Primary.

### Case 2: Validated exception

Provider X API v2, exact idempotency, and `token_valid=true` satisfy the fixture exception. The active, fresh, validated, evidence-backed exception applies, explicitly suppressing the linked reconciliation directives.

### Case 3: Stale exception

The same scope with stale exception freshness cannot relax the general rule. The exception appears as advisory by default. Expired-token context makes replay inapplicable; missing token state makes it PartiallyKnown. Both preserve reconciliation guidance.

### Case 4: Unresolved sibling conflict

Two equally eligible specializations or exceptions disagree and have no explicit winner. Resolution reports CompetingSpecializations or CompetingExceptions, preserves the parent, and exposes both alternatives as SupportingContext. Two independent Refines children instead both compose.

## CLI and fixture

All commands support global `--json`; JSON responses use the existing response envelope with `event: "knowledge"`, a `result` object, and `schema_version: 1`. Scopes tag predicates with `predicate`; values use `type` and `value`.

```bash
hardknock knowledge hierarchy show --hierarchy fixtures/hierarchy/idempotency/hierarchy.json
hardknock knowledge hierarchy validate --hierarchy fixtures/hierarchy/idempotency/hierarchy.json
hardknock knowledge resolve --hierarchy fixtures/hierarchy/idempotency/hierarchy.json \
  --context fixtures/hierarchy/idempotency/valid-token.json
hardknock knowledge explain --hierarchy fixtures/hierarchy/idempotency/hierarchy.json \
  --context fixtures/hierarchy/idempotency/unknown-token.json --json
```

For persistence:

```bash
hardknock knowledge hierarchy import fixtures/hierarchy/idempotency/hierarchy.json
hardknock knowledge hierarchy show
hardknock knowledge hierarchy validate
hardknock knowledge resolve --context fixtures/hierarchy/idempotency/expired-token.json
```

`--id <hierarchy-ID>` selects a stored hierarchy; otherwise the CLI lists/resolves stored hierarchies separately. It does not merge unrelated forests into a global resolution or invent an empty-store fixture. `--hierarchy <file>` reads without importing. Validation returns its report, including invalid status, as inspectable output.

The idempotency fixture has five directive artifacts (general, HTTP, provider, exact replay, expired token), fixed IDs/timestamps, artifact statements in `artifacts.json`, and contexts for valid, expired, unknown, wrong-provider, and API v3. Separate hierarchy variants cover stale exception, competing exceptions, and v2/v3 supersession. V2 deliberately has a compatibility scope covering versions 2 and 3 so the explicit v3 replacement can suppress an applicable old rule.

## Persistence and boundaries

Migration 022 adds one `knowledge_hierarchies` table. Each hierarchy is an atomic JSON aggregate with indexed ID and revision, following the existing SQLite/JSON storage approach. Updates must advance the stored revision by one. This avoids maintaining a second node/edge representation of the same aggregate. No effective resolution or trace is persisted.

V0.17 specialization/exception tables remain their source of truth; projection adapters do not dual-write them. Explicit V0.18 aggregates are caller-managed inputs. Automatic projection refresh, artifact health hydration, immutable historical snapshots, and resolution-history persistence belong to Pass 2. A caller importing a projection must refresh it deliberately; no live synchronization is claimed.

## Verification and scaling

Run focused tests with:

```bash
cargo test --test knowledge_hierarchy
cargo test --test knowledge_hierarchy hierarchy_scaling -- --ignored --nocapture
```

The scaling test generates 100, 1,000, and 10,000 nodes with four-way branching and refinement edges. It reports scope filtering, complete resolution including trace generation, and trace JSON serialization separately. Output lineage size is proportional to the number of ancestor references; very deep or dense DAGs can inherently produce large output. This is a foundational scaling smoke test, not the deferred large benchmark suite.

Next: **V0.18 Pass 2 — Runtime Resolution, Historical Knowledge Snapshots, Conflict Curricula, and OpenKedge Guard Revision Candidates**. Wire this engine to RuntimeController, hardknock why, ExperienceContextBundle, replay, snapshots, conflict-driven experiments, freshness propagation, and GuardRevisionCandidate review without changing these precedence or governance boundaries.

## Runtime integration

Pass 2 connects the domain resolver to [runtime context and decisions](knowledge-resolution.md), [immutable snapshots and replay](knowledge-snapshots.md), [conflict learning](knowledge-conflicts.md), and [Guard review exports](guard-revision-candidates.md). Health projection distinguishes inherited and independent evidence. General defaults remain available when an exception is stale, unknown or contradicted.
