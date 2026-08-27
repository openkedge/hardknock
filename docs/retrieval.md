# Lesson retrieval and transfer

Experience is immutable observation; a Lesson is an interpretation. Retrieval judges relevance, application records influence, and validation combines evidence. None of these grants execution permissions.

```text
Past Experience → supported Lesson → context match → advice
                                                     ↓
                                      observed application → new Experience
                                                                 ↓
                                              distinct support → validation
```

## Deterministic policy

`QueryContext` contains repository/environment context, detected markers, task text, proposed `ActionPattern`s, and tags. Task text is recorded but is not a scoring signal. No embeddings, LLM ranking, keyword guessing, or network calls are involved.

`ContextSelector` is a hard gate: every required repository path, marker, tag, OS, and architecture must match. An unrelated context scores zero and is excluded even with a zero minimum threshold. Eligible states are `CounterfactuallySupported` and `Validated`. Candidates can be inspected with `--include-candidates`, but are never injected. Contradicted and retired Lessons are excluded.

After the scope gate, `DeterministicRelevance` adds each signal once:

| Signal | Weight | Rule |
| --- | --- | --- |
| Required markers | 0.40 | The selector requires at least one marker and all are present |
| Repository | 0.20 | An explicitly required repository path matches |
| Proposed action | 0.30 | One proposed action matches the Lesson's entire avoid script |
| Required tags | 0.10 | The selector requires tags and all match |

Scores are rounded to two decimal places and clamped to [0, 1]. Results sort by descending score, then Lesson ID for stable ties. Matches carry signal values and weights; exclusions carry reasons. The current local store scans Lessons; no scale or latency claim is made.

| Default score | Meaning |
| --- | --- |
| Below 0.50 | Excluded |
| 0.50–0.69 | Informational; not delivered to the agent |
| 0.70–0.84 | Recommendation |
| 0.85–1.00 | Strong relevance recommendation |

`--min-relevance`, `--recommend-threshold`, and `--strong-threshold` configure these boundaries on `run` and `lesson search`. Values must be finite, bounded, and ordered. Confidence and relevance are separate heuristics: high relevance does not establish that a claim is correct. At most 20 eligible recommendations are delivered per attempt.

```bash
hardknock --repo /path/to/B lesson search --action 'npm install'
hardknock --json --repo /path/to/B lesson search --action 'npm install'
hardknock lesson search --include-candidates --action './fix.sh'
```

Search uses a clean committed repository, captures controlled context, and does not create a Reality or execute an agent. Without proposed actions, the fixture family scores 0.50 and remains informational.

## Scope of the fixture lesson

Manual hypotheses retain repository-specific scope. Existing stored Lessons are not broadened during migration. Version-2 fixture A can propose a shared selector requiring:

- `pnpm-workspace.yaml` and `hardknock-fixture.json`.
- `fixture-family:pnpm-workspace-v2`, derived only for the three named pnpm fixtures.
- The source OS and architecture.

This permits A → B transfer and testing in D, but excludes ordinary npm fixture C and arbitrary repositories. It is deliberately a fixture-family claim, not a general rule about npm or pnpm. All package-manager behavior is simulated locally.

Action matching normally compares the complete script after trimming outer whitespace. One documented fixture-only alias maps a proposed `npm install` to `./agent-script.sh baseline`. There is no general semantic shell matcher. The cross-repository fixture score is 0.80: markers + action + tags.

## Advice, control, and application

The test adapter enables advice by default. Other adapters opt in with `--with-experience` or `--retry-with-experience`. Before launching the agent, Hardknock writes `.hardknock/context.md` and `context.json`, saves hashed copies outside the Reality, and reports delivered advice on stderr. Existing `.hardknock` files/directories/symlinks cause an intervention; no repository content is overwritten. See [the contract](agent-integration.md).

`--no-experience` disables advice delivery, fixture reflection, and retries. For measurement only, Hardknock still computes an audit match against stored Lessons. Audit matches cannot influence the agent: they have `delivered=false`, `influence=ignored`, and an audit-only reason. This deliberate distinction allows the control to record repeated mistakes. The fixture runs its explicit baseline directly, without reading context files.

Each `LessonApplication` records the exact Lesson revision, relevance/matches, delivery, influence, verification, resulting action, reason, and proof artifacts. Merely retrieving a Lesson is not applying it:

- `Retrieved`: advice was delivered; use is unconfirmed.
- `Consulted`: the fixture reported reading the Lesson.
- `Applied / Observed`: a trusted fixture trace reports the delivered ID and the preferred strategy.
- `Applied / SelfReported`: an opaque agent reports using the Lesson; this cannot promote it.
- `Ignored`: advice was not delivered or the fixture explicitly declined it; inspect `delivered` and `reason`.
- `Contradicted`: an agent may report disagreement; this alone is not a controlled contradiction.

`RepeatedMistakeObservation` means an observed avoid action executed under a matching, currently supported/validated Lesson at or above the retrieval minimum. It does not claim the agent consciously ignored advice. Opaque internal actions are not counted without an observer.

## Retry and validation

Retry is opt-in, default budget 1, maximum 10. Every attempt uses the original commit, task, adapter, and checks in a fresh Reality; the Lesson context can change. Retry requires a failed evaluated task and an applicable supported recommendation. Success, cancellation, absence of advice, or budget exhaustion stops it. Timeouts and inconclusive observations are not automatically retried. The two counterfactual trials are separate from this retry budget.

New Experiences link `retry_of`, `counterfactual_of`, and observed `transfer_from` to prior Experiences. Neither the original failure nor the Experiment is edited. A successful retry of the same snapshot adds evidence but is not distinct validation.

`DistinctApplicationValidation` (policy `distinct-application-v1`) requires:

1. An eligible supported Lesson with a completed supporting controlled comparison.
2. No completed contradicting controlled comparison.
3. At least one relevant, observed, successful application whose Git tree differs from the origin.

Qualifying contexts are deduplicated by tree hash plus environment fingerprint. A changed task label, commit metadata, or identical clone cannot create another distinct context. A different tree is only a V0.1 proxy for independence; an irrelevant file change can satisfy it. The fixed validation relevance floor is 0.70, regardless of lower delivery thresholds.

The first distinct success produces `Validated` at confidence 0.90; a second distinct context produces 0.94. Same-tree replays and agent self-reports do not raise confidence. Validation evidence includes applying-agent identity; the policy does not depend on model brand. Currently only the trusted fixture observer supplies independently observed application evidence.

**Validated means Hardknock observed supporting evidence in both a controlled counterfactual and at least one distinct application context. It does not imply universal correctness.** Confidence is an evidence heuristic, not a calibrated probability.

A relevant failed application alone is inconclusive about causality. `lesson test` can establish a controlled contradiction: baseline success plus alternative failure moves even a Validated Lesson to `Contradicted` at 0.20, preserving earlier evidence. New support does not silently erase that contradiction. `lesson retire --reason ...` is an explicit, idempotent lifecycle operation; there is no automatic retirement.

## Measurement and provenance

The automated B comparison records success 0/1 → 1/1 and repeated mistakes 1/1 → 0/1. Conceptually, Experience Transfer Rate is the fraction of applicable Lessons that improve outcome on related unseen tasks. This designed fixture pair supplies one comparison, not a population estimate. A benchmark CLI, randomization, larger task sets, token/cost accounting, and independent semantic context checks remain deferred.

`experience show`, `lesson show`, `experiment show`, and `why --experience ...` expose application → Lesson revision → controlled Experiment → source Experience. `why` also shows the current Lesson, so later contradiction or retirement is visible without changing the historical application. `status` reports stored counts; it does not compute a benchmark.
