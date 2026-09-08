# Experience abstraction and safe transfer

Hardknock V0.17 turns structurally related, context-specific operational
knowledge into explicit candidate abstractions. It then asks the important
question: does the proposed knowledge still change behavior correctly in a
context that was not used to form it?

> Generalize only as far as transfer evidence permits.

This is an empirical abstraction layer, not semantic clustering and not an
LLM-authored universal rule. Similar wording can suggest a candidate, but it
cannot support or validate one. The default provider is deterministic and
model-free; it groups compatible typed artifacts by shared triggers, action and
outcome semantics, causal references, required conditions, and recovery/failure
structure. It deliberately excludes prose from candidate identity.

## Lifecycle

```text
Specific Lessons / Skills / Constraints / AntiPatterns / Recoveries
                              |
                              v
                 structural ExperiencePattern
                              |
                              v
                  candidate AbstractKnowledge
                              |
             source contexts | held-out context
                              v
              paired baseline / transfer experiment
                              |
                  + negative controls when required
                              v
       validate / require evidence / narrow / contradict
                              |
             specializations + explicit exceptions
                              |
                   bounded runtime resolution
```

An `ExperiencePattern` preserves typed member references, shared structure,
scope, evidence, and status. `AbstractKnowledge` preserves the proposed
statement, an executable applicability predicate, its generalization boundary,
source patterns, transfer evidence, maturity, risk, and complete provenance.
The supported kinds remain distinct:

| Kind | Meaning | Promotion sensitivity |
| --- | --- | --- |
| `AbstractLesson` | Reusable operational guidance | Held-out local support |
| `AbstractSkill` | Reusable procedure shape | Held-out behavioral support |
| `AbstractConstraint` | Candidate prohibition or invariant | Held-out support plus negative control; never automatic enforcement |
| `AbstractAntiPattern` | Reusable failure-producing action structure | Held-out support plus negative control |
| `AbstractRecovery` | Reusable recovery mechanism | Recovery success in a held-out failure context |

## Context and boundaries

`ContextVariable` makes generalization dimensions explicit. Built-in dimensions
cover environment, resources, software/dependency versions, action semantics,
idempotency, reversibility, externality, concurrency, tools, agent runtime, and
failure mode. Each variable has a value and a relevance classification:
`required`, `suspected`, `varies`, `irrelevant`, or `unknown`.

`GeneralizationBoundary` has three independent sets:

- `included`: conditions with affirmative evidence;
- `excluded`: known counterexample or exception conditions;
- `unknown`: untested conditions that must remain visible.

A failed transfer does not rewrite or erase the original artifacts. It can
contradict the candidate or produce a narrower revision and a
`KnowledgeException`. Revisions, evidence, and negative-transfer events are
append-only where history matters.

## Transfer evidence

A `TransferHypothesis` names source contexts, a genuinely held-out target, and
the expected behavioral change. A `TransferEvaluationSet` keeps source,
held-out, and negative-control contexts in separate roles. A transfer plan
requires two equivalent-start trials:

```text
baseline_without_abstraction
abstraction_active
```

Both are delegated to the existing Experiment engine with
`ExperimentIntent::ValidateTransfer`. The abstraction layer does not create a
second executor and grants no capability, Effect, or approval authority.

`TransferEvidence` references the baseline and transfer trials, records context
differences, experiment quality, evidence diversity, expected applicability,
whether guidance fired, and one of `supports`, `contradicts`, `narrows_scope`,
`inconclusive`, or `invalid`.

Constraints and AntiPatterns require a negative control before promotion. The
negative control is a context where the broad rule should *not* apply. If a
constraint fires there, promotion returns `narrow_scope`; it never treats the
false block as additional support.

## Promotion and trust

The default promotion policy requires:

- at least two source contexts;
- at least one controlled, local, held-out support result;
- at least two distinct root origins, so descendants of one belief are not
  miscounted as independent support;
- a clean negative control for Constraints and AntiPatterns;
- no observed false-constraint application.

Remote evidence can propose a transfer test but cannot satisfy the local
held-out gate. Signed bundles authenticate their producer; they do not turn a
remote abstraction into local knowledge. Only locally validated abstractions
can be exported. Imported and re-exported abstract objects remain advisory
until the receiving node records its own controlled support.

## Distillation and runtime use

Distillation records which validated specific artifacts are represented by an
abstraction. It does not delete those artifacts or their evidence. If the
abstraction becomes contradicted or stale, members can be reactivated as direct
runtime knowledge.

Runtime resolution is deterministic, model-free, and bounded. Applicable
knowledge is resolved in this precedence order:

```text
specific exception > specific knowledge > specialization > abstraction
```

When a more specific item is selected, redundant represented members are
suppressed from the injected context but remain inspectable. Unknown boundary
conditions are reported rather than silently treated as matches.

An abstract Constraint is guidance only. `assess_guard_candidate` can report
whether evidence is sufficient for external governance review, but Hardknock
does not automatically create an enforcing guard. OpenKedge or another policy
authority must perform an independent authorization and governance step.

Assurance manifests pin every abstraction revision, transfer-evidence record,
specialization, and exception used by a certified Skill, so later abstraction
changes alter the evidence manifest and trigger ordinary review. Generic
AbstractSkill contracts can be linked to stronger specialization contracts
without forcing providers into one state schema. A validated AbstractAntiPattern
may seed an EarlyWarning candidate in a new domain, but the candidate remains
inactive until local predictive positives and negative controls validate it.

## CLI

```bash
hardknock pattern list
hardknock pattern show pattern-<uuid>
hardknock pattern candidates
hardknock pattern explain pattern-<uuid>

hardknock abstract list
hardknock abstract show abstract-<uuid>
hardknock abstract propose
hardknock abstract test-transfer abstract-<uuid> --target-tag object-store
hardknock abstract test-transfer abstract-<uuid> --target-tag idempotent-api --negative-control
hardknock abstract validate abstract-<uuid>
hardknock abstract boundary abstract-<uuid>
hardknock abstract history abstract-<uuid>
hardknock abstract impact abstract-<uuid>
hardknock abstract benchmark

hardknock federate export --abstract-knowledge abstract-<uuid> --dry-run
```

`pattern candidates` and `abstract propose` deterministically persist stable
candidate pattern/abstraction identities so later commands can address them;
they do not promote those candidates. Persisted abstractions are revised by
explicit validation. `abstract test-transfer`
persists the hypothesis and held-out set and returns the typed paired plan; the
caller must supply the concrete starting state, candidates, and evaluator to
the Experiment executor, then record the resulting `TransferEvidence`.

## Benchmark scope

`abstract benchmark` compares three deterministic arms across authoritative
state, quorum availability, and misleading surface similarity fixtures:

1. specific-only retrieval;
2. naive semantic abstraction;
3. Hardknock empirical abstraction with held-out tests and boundaries.

The acceptance claim is intentionally narrow: in these fixtures, empirical
abstraction produces more held-out successes than specific-only retrieval,
fewer negative transfers than naive abstraction, and fewer injected runtime
items than specific-only retrieval. This is not a population estimate, an
agent benchmark, or proof of universal transfer.

See the [V0.17 implementation report](implementation-v017.md) for the delivered
surface and known limitations, and the [roadmap](roadmap.md) for V0.18.
