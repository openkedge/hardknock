# Experience model

> Experiences are immutable evidence. Lessons are revisable interpretations of evidence.

```text
Reality ≠ Experience
ExecutionRecord ≠ evaluated task success
Experience ≠ Lesson
Reflection ≠ causal proof
```

## Typed records

IDs are resource prefixes followed by canonical UUIDs. Different ID types cannot be interchanged through parsing or JSON. The active prefixes are `r-`, `exec-`, `eval-`, `exp-`, `hypothesis-`, `lesson-`, `experiment-`, `trial-`, and `application-`. Reflex and Recovery IDs reserve future concepts without implementing them.

| Record | Fields and meaning |
| --- | --- |
| `StateRef` | Canonical source path, full commit, tree hash |
| `ExecutionRecord` | Agent process status, goal, identity, argv/environment mode, timing, exit/signal, stdout/stderr, pre-evaluation diff |
| `EvaluationSpec` | Ordered, required shell check scripts |
| `Evaluation` | ID, spec, status, success, individual checks and their process actions, summary |
| `Experience` | ID/time, goal, context, state, Reality/Execution IDs, agent, observed actions, perturbations, outcome, evaluation, failure signatures, evidence, tags, optional explicit replay script |
| `CandidateHypothesis` | ID/source Experience/time, claim, rationale, scope, avoid/prefer actions, reflection-provider identity |
| `Lesson` | ID/version, source and hypothesis IDs, status, claim/scope/actions, rationale, confidence, evidence, discovery identities, creation/update times |
| `Experiment` | Source Experience/Lesson/Hypothesis, starting state, replay plan, trial results, status, conclusion, optional runtime failure |

Actions are observed process invocations, not instrumented commands inside an opaque agent. Fixture logs such as `ACTION shell npm install` describe a simulation. The recorded process may invoke `./agent-script.sh run`; its explicit replay script is the observed strategy (`baseline` or `alternative`). Prediction, surprise, automatic recovery observations, and general perturbation engines are deferred. The only implemented perturbation is explicit command replacement.

## Application and lineage

Experience includes `lesson_applications`, `relations`, `repeated_mistakes`, `observed_actions`, and `application_report_errors`. All default empty when reading historical JSON. Lesson adds optional `retired_at`, `retired_reason`, and `validation`; these belong to new revisions, not edits to old observations.

| Record | Meaning |
| --- | --- |
| `LessonApplication` | Lesson ID/version, Experience ID, relevance/matches, delivered flag, influence, verification, resulting action, reason, proof artifacts |
| `ExperienceRelation` | `retry_of`, `counterfactual_of`, or observed `transfer_from`, directed from new to prior Experience |
| `RepeatedMistakeObservation` | Relevant supported/validated Lesson, observed avoid action, score and artifact |
| `LessonValidationDecision` | Policy version, result, distinct successful context count and reason |

Retrieval alone is not application. Fixture traces can establish observed influence; opaque agent reports are self-reported and cannot validate. Context files and valid usage reports have hashed snapshots outside the disposable Reality. Application, lineage, mistake, and validation rows are immutable, linked by foreign keys and saved atomically with the Experience and any Lesson evidence revision.

## Evaluation is distinct from execution

Process statuses are `succeeded`, `failed`, `interrupted`, and `timed_out`. Evaluation statuses are `completed`, `not_configured`, `interrupted`, and `timed_out`. Every configured check must pass for `success=true`. Checks use `/bin/sh -c`, preserve output/timing/exit/signal, and may modify the worktree. Missing commands normally produce a recorded shell exit 127.

Experience outcomes are `success`, `failure`, `inconclusive`, `interrupted`, and `timed_out`. Outcome follows evaluation rather than the agent's exit code. Agent interruption/timeout skips evaluation; later checks are marked `not_run`. With no configured checks, task success is unknown; the CLI's process-based exit fallback does not change that recorded outcome.

## Context and signatures

Context records source path/name/commit, optional branch (currently unset for detached executions), OS, architecture, Reality working directory, selected environment facts, and a fingerprint. It detects root markers:

`package.json`, `pnpm-workspace.yaml`, `Cargo.toml`, `go.mod`, `pyproject.toml`, `requirements.txt`, `pom.xml`, `build.gradle`, and `hardknock-fixture.json`.

Marker tags are also recorded. Generic inherited environments deliberately omit arbitrary values that could contain credentials. Their fingerprint is not a reproducibility guarantee and cannot authorize replay. Controlled scripts record a normalized fixed environment; see [experiments](experiments.md).

Failure signatures have a name, source (`evaluator`, `agent_output`, `rule`, or `manual`), confidence, and artifact references. The current extractor records required check failures and searches the first 64 KiB of each output for the literal fixture tokens `package_manager_conflict` and `duplicate_lockfile`. These are observations from deterministic rules, not learned semantic classifiers or established causes. Pattern confidence is a heuristic.

## Evidence and storage

`ArtifactRef` contains `path`, `blake3`, `bytes`, and `kind`. Existing names are preserved for compatibility. Kinds include stdout, stderr, diff, evaluation output, metadata, and other. Output and patches live on disk; SQLite stores bounded structured records and references, not log blobs.

The agent diff precedes checks; the final Experience diff includes check effects. `execution.json` is a hashed metadata artifact. `metadata.json` mirrors the Experience and is excluded from its own artifact list to avoid a self-referential hash. References are content digests, not tamper-proof storage or automatic verification on read. Local owners can edit files/database contents; compare hashes before relying on exported evidence.

`ExperienceStore` exposes insert/get/list with an outcome query and typed summaries. It has no update/delete operation. SQL triggers protect Experience/Evaluation/artifact history from ordinary updates/deletes. Discarding a Reality does not delete its Experience.

`LessonStore` supports insert/get/list and optimistic metadata revisions. The next version is required, and creation time, identity, and historical evidence cannot change. Evidence changes go through experiment finalization or the atomic Experience/application transaction. Explicit retirement creates another revision. Applications reference the exact Lesson version used; `why` also shows its current version. A different tested claim, scope, or pair of commands requires a new hypothesis, avoiding accidental reuse of old evidence. Rationale revisions and all prior versions remain available through the store API.

## Scope and actions

`ContextSelector` can constrain repository path, required markers, tags, OS, and architecture. Manual proposals use the source repository, observed markers, OS, and architecture. Version-2 fixture A uses a shared selector requiring the pnpm fixture-family tag, workspace/fixture markers, OS, and architecture. Existing Lessons retain their original scope. Neither path creates a global “never use npm” rule.

`ActionPattern` supports shell commands, file operations, and custom actions. Only shell patterns are executable by the experiment engine. Matching is exact equality after trimming **outer whitespace only**: `npm install` does not match `npm  install`, `npm install --force`, or a substring of a larger script. There is no regex, prefix, quoting, or semantic matcher.

`EvidenceRef` is either an Experience or a Trial linked to its Experiment. Relationships are `origin`, `supports`, `contradicts`, or `inconclusive`. The last value explicitly retains neutral comparisons without presenting them as support. Lesson → Hypothesis → source Experience and Lesson → Experiment → Trial → Evaluation/artifacts are represented by typed fields and SQLite foreign keys.

## Lifecycle and confidence

| Experiment | Lesson effect | Heuristic confidence |
| --- | --- | --- |
| New hypothesis | Candidate | 0.42 |
| Baseline fails, alternative passes | Candidate → CounterfactuallySupported | 0.78 |
| Baseline passes, alternative fails | Candidate, supported, or Validated → Contradicted | 0.20 |
| First observed successful application in a distinct tree | Supported → Validated | 0.90 |
| Second distinct successful application context | Validated | 0.94 |
| Explicit retirement | Retired; time/reason persisted | Unchanged |
| Both pass / both fail | Status unchanged; neutral evidence retained | Unchanged |
| Interrupted/runtime failure | No Lesson revision or support claim | Unchanged |

A timed-out trial gives an inconclusive comparison. Duplicate experiment evidence is rejected. Supporting pairs alone cannot validate a Lesson; repeated support preserves confidence already earned. A later supporting pair never erases a contradiction. Failed applications alone add inconclusive evidence, not a causal contradiction. Retired Lessons are excluded from default listing and retrieval.

Validation requires controlled support plus a relevant, observed, successful application in a different Git tree and no controlled contradiction. Tree/fingerprint keys deduplicate applications. Renamed tasks, identical clones, same-state retries, and opaque self-reports cannot inflate confidence. Policy decisions and applying-agent identity remain inspectable. A different tree is only a heuristic for context independence; see [retrieval and validation](retrieval.md).

**Validated means Hardknock observed supporting evidence in both a controlled counterfactual and at least one distinct application context. It does not imply universal correctness.**

**V0.1 confidence values are heuristic indicators of accumulated evidence, not calibrated probabilities.** Counterfactual support is not universal causal proof.
