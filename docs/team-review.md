# Team review gates

A team review binds a claim to the exact proposed action/effect and, when present,
the plan run/revision/step and composition revision/step. Use the Rust
`team::review_action_hash(&RuntimeDecisionContext)` API to derive this identity.
Descriptions and caller-supplied assessments cannot alter it. A review expires
within 24 hours and names its proposer, executor, required reviewer roles, minimum
observable evidence diversity, and maximum supporting-evidence age.

For high-risk team execution, proposer, reviewer and executor must be distinct
registered members. Each member has a distinct authenticated session. This is
role separation; it does not establish statistical independence. Reviewers must
hold a current, applicable assignment permitting challenges. Low-risk operations
without an explicit gate need no review. Once an operation has an explicit gate,
omitting its review ID cannot bypass that gate.

## Contributions and evidence

The local contribution API accepts bounded structured statements, contribution
types and references to existing V0.13 evidence paths. It checks session identity,
assignment authority, observed scope, target action and claim identity. Proposal
and hypothesis statements create no empirical paths. Observation, experiment,
review and execution contributions cite existing evidence without changing its
source. This API is a local adapter boundary, not a remotely authenticated service.

The evidence assessment deduplicates path IDs, reuses V0.13 fusion, dependency
analysis, echo detection and fault domains, and includes known contradictions for
the claim even when a reviewer omitted them. Different roles or repeated citations
cannot manufacture diversity. High-risk gates require at least moderate observable
diversity and reject shared experience/evaluator/root dependencies common to all
paths. Old supporting evidence requires revalidation. No scientific independence
claim follows from these checks.

A `NoIssueFound` finding requires inspectable evidence references. Other findings
remain unresolved until an explicit local user disposition records a reason and
claim-bound evidence. Opening another review for the same operation does not erase
an unresolved finding. Dispositions preserve both records; they do not remove
contradictory evidence or grant external effect authorization.

## Runtime and history

The store discards caller-supplied assessments and checks live authority and review
state when recording and publishing a runtime decision. The review assessment hashes
its immutable declaration, contributions, findings, dispositions and evidence
assessment. A change before publication requires resolution again. Historical
runtime contexts retain the original result. Existing plan validity, capability
checks, security policy and external commit authorization continue to apply.

## Local CLI

Commands emit the existing JSON-oriented response format:

```text
hardknock review target --context executor-context.json
hardknock review create review.json
hardknock review show <review-id>
hardknock review findings <review-id>
hardknock review contribute contribution.json --context context.json --findings findings.json
hardknock review assess <review-id> --context executor-context.json
hardknock review resolve-finding disposition.json
```

`team diversity` and `team common-mode` take the review ID as a positional argument:

```text
hardknock team diversity <review-id>
hardknock team common-mode <review-id>
```

These commands configure and inspect the local store. They are not agent Bridge
self-approval endpoints. Mutation commands require the same local administrative
trust as team/role import. Review resolution is explicit user governance, not an
automatic claim that arbitrary findings have been empirically disproved.

## Current limits

This layer does not run reviewers, schedule experiments, or verify arbitrary
natural-language claims. It consumes canonical evidence already recorded by the
existing evidence subsystem; it does not add stronger authentication to that
subsystem. Full role-aware handoffs, blind knowledge exposure, responsibility maps,
effect actor receipts, recovery coordination and the remaining V0.21 integrations
are tracked in `v0.21-progress.md`.
