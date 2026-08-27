# Experience model

> Experiences are immutable evidence. Lessons are revisable interpretations of evidence.

```text
Reality ≠ Experience
ExecutionRecord ≠ evaluated task success
Experience ≠ Lesson
Reflection ≠ causal proof
```

## Typed records

IDs are resource prefixes followed by canonical UUIDs. Different ID types cannot be interchanged through parsing or JSON. The active prefixes are `r-`, `exec-`, `eval-`, `exp-`, `hypothesis-`, `lesson-`, `experiment-`, and `trial-`. Reflex and Recovery IDs reserve future concepts without implementing them.

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

Actions are observed process invocations, not instrumented commands inside an opaque agent. Fixture logs such as `ACTION shell npm install` describe the simulation; the actual recorded action is `./agent-script.sh baseline`. Prediction, surprise, automatic recovery observations, and general perturbation engines are deferred. The only implemented perturbation is explicit command replacement.

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

`LessonStore` supports insert/get/list and optimistic metadata revisions. The next version is required, and creation time, identity, and historical evidence cannot change. New evidence and confidence/status changes go through `Lesson::apply_experiment` and the experiment finalization transaction. A different tested claim, scope, or pair of commands requires a new hypothesis, avoiding accidental reuse of old evidence. Rationale revisions and all prior versions remain available through the store API.

## Scope and actions

`ContextSelector` can constrain repository path, required markers, tags, OS, and architecture. Current proposals use the source repository, its observed markers, OS, and architecture. They never produce a global “never use npm” rule.

`ActionPattern` supports shell commands, file operations, and custom actions. Only shell patterns are executable by the experiment engine. Matching is exact equality after trimming **outer whitespace only**: `npm install` does not match `npm  install`, `npm install --force`, or a substring of a larger script. There is no regex, prefix, quoting, or semantic matcher.

`EvidenceRef` is either an origin Experience or a Trial linked to its Experiment. Relationships are `origin`, `supports`, `contradicts`, or `inconclusive`. The last value explicitly retains neutral comparisons without presenting them as support. Lesson → Hypothesis → source Experience and Lesson → Experiment → Trial → Evaluation/artifacts are represented by typed fields and SQLite foreign keys.

## Lifecycle and confidence

| Experiment | Lesson effect | Heuristic confidence |
| --- | --- | --- |
| New hypothesis | Candidate | 0.42 |
| Baseline fails, alternative passes | Candidate → CounterfactuallySupported | 0.78 |
| Baseline passes, alternative fails | Candidate or supported → Contradicted | 0.20 |
| Both pass / both fail | Status unchanged; neutral evidence retained | Unchanged |
| Interrupted/runtime failure | No Lesson revision or support claim | Unchanged |

A timed-out trial gives an inconclusive comparison, not positive evidence. Duplicate experiment evidence is rejected. Further supporting pairs stay at 0.78 and do not promote to `Validated`. A later supporting pair does not erase a contradiction. `Validated` and `Retired` are representable domain states, but no CLI assigns them and V0.1 does not perform those transitions.

**V0.1 confidence values are heuristic indicators of accumulated evidence, not calibrated probabilities.** Counterfactual support is not universal causal proof.
