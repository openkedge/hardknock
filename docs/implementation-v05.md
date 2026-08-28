# V0.5 implementation report

Version: `0.5.0-dev.1`. Verified on 2026-08-28, locally on macOS arm64. This phase implements explicitly invoked, bounded curricula. It does not install a scheduler or modify production systems. V0.3 live Claude/Codex acceptance remains open.

## 1. Files created and changed

New implementation files:

```text
src/curriculum/mod.rs
src/curriculum/model.rs
src/curriculum/catalog.rs
src/curriculum/policy.rs
src/curriculum/inventory.rs
src/curriculum/planner.rs
src/curriculum/executor.rs
src/store/curriculum.rs
src/cli/curriculum.rs
migrations/008_curriculum.sql
```

New tests and documentation: `tests/curriculum.rs`, `tests/curriculum_bridge.rs`, `docs/curriculum.md`, and this report. Both `fixtures/skill-hardening/` and `fixtures/skill-hardening-transfer/` contain `operation.sh`, `replan.sh`, `test.sh`, `refresh-token.sh`, `read-state.sh`, `generation`, `plan-generation`, `input-generation`, `dependency`, `token`, `fixture-kind`, and `hardknock-fixture.json`.

Updated files:

```text
Cargo.toml
Cargo.lock
src/lib.rs
src/core.rs
src/budget.rs
src/experience.rs
src/workflow.rs
src/store.rs
src/store/resilience.rs
src/cli.rs
src/cli/resilience.rs
src/bridge/config.rs
src/bridge/protocol.rs
src/bridge/engine.rs
src/bridge/experiments.rs
src/experimentation/config.rs
src/resilience/models.rs
src/resilience/fixture.rs
src/resilience/runtime.rs
src/resilience/campaign.rs
src/resilience/testing.rs
tests/learning.rs
tests/substrate.rs
README.md
docs/architecture.md
docs/cli.md
docs/bridge-protocol.md
docs/roadmap.md
```

No dependency was added. Existing execution, evaluation, perturbation, reflection, response testing, and artifact storage are reused.

## 2. Schema migration

Migration **008** adds curricula, goals, evidence gaps, trials, engine links, progress events, task families, derived Skill coverage/usage, append-only package snapshots, and contradiction review records. References use existing Skill, Lesson, Experience and engine records; execution data is not copied into a second store.

Curriculum updates use revision checks. State changes and progress events commit together. Identity/budget, terminal curricula, recorded trial results, engine links, gaps and package history have mutation guards. Engine links are recorded when the underlying engine creates its record, before execution, so partial evidence remains discoverable. Original Skill rows and older Experience JSON remain immutable; Skill reads can enrich their metadata from derived package snapshots. Migration preservation, SQLite integrity and foreign-key checks are covered. Back up an existing database before upgrading; there is no down migration.

## 3. Curriculum model

`Curriculum` contains a typed target, finite profile, explicit goals/trials, maximum rounds, budget, reservations, recorded usage, before/after packages, revision, session provenance and status. Statuses are Planned, Running, Completed, PartiallyCompleted and Cancelled. Completion means selected trials finished, not that all conditions succeeded or every gap was closed.

Goals retain their gap, severity, priority, rationale, safety decision and execution status. Trials retain a semantic fingerprint, intent, round, required isolation, estimated cost, engine reference, observations and `LearningOutcome`. Learning outcomes reference new Experiences and generated or updated artifacts. Skill and explicitly registered TaskFamily targets execute; the broader target enum leaves future Agent/Repository/Lesson planning visible but unsupported.

## 4. Evidence Gap semantics

A gap names a dimension, known and unknown values, and why another observation is useful. Unknown means untested or inconclusive, never failed. A conclusive failure closes uncertainty about that tested condition while opening a response gap. Interrupted trials do not count as coverage. Unsupported catalog entries remain visible and rejected; budget reductions defer complete trials.

Gap rationale and original observations are retained after later evidence arrives. A known failure does not imply a universal prohibition, and a successful recovery does not change the original failed observation.

## 5. Deterministic planning rules

The planner inventories current Skill evidence, proposes missing catalog conditions, revalidates stale or weak base evidence, tests Candidate recoveries, challenges reflexes with negative controls, and compares recorded contexts for contradicted Lessons. Every executable Skill must have a supported single shell procedure and explicit evaluator.

Recent conclusive exact fingerprints suppress novel repeats; identical planned/running work also suppresses duplication. Explicit `--replicate` bypasses novelty suppression while preserving safety and budgets. Fingerprints include Skill/procedure, repository path/commit/tree, source agent, evaluator, condition parameters, measured environment and runtime. Base revalidation deliberately executes the stored shell procedure through the existing generic shell experiment adapter; it is not a new native-agent capability measurement.

One round is the default. A configured second round may only test a newly generated Recovery, with one trial slot reserved and all aggregate limits enforced. No recursive agent-generated fault execution is allowed. The suggestion-provider interface validates bounded named suggestions as rejected or requiring approval; no LLM provider or automatic suggestion execution is installed.

## 6. Prioritization policy

Priority is an explained heuristic: dimension weight × 100 + capped observed execution frequency + unknown-value count. Recovery and contradiction gaps rank first, then credential/configuration gaps, freshness/reflex checks, other conditions and latency. Stable condition/Skill ordering breaks ties. The score is not a probability, expected financial value, or learned information-gain estimate. TaskFamily targets share an aggregate budget rather than receiving independent per-Skill allowances.

## 7. Skill Coverage semantics

Profile Coverage is the number of uniquely observed configured conditions divided by the finite configured catalog size, including the healthy control. Passing, degraded and failed observations count; inconclusive results do not. The profile and denominator are displayed. No coverage over all possible environments, interpolation between delay points, or unseen credential states is claimed.

Coverage observations retain Experience IDs, outcomes, fingerprints, severity and timestamps. Old envelopes and package snapshots remain available. Age, evaluator, repository and environment checks determine which observations count as current. Duplicate trials do not inflate the numerator.

## 8. Skill Maturity policy

Observed has no current successful base evidence. Supported has one. Validated requires at least two current successful base observations. Hardened additionally requires the configured minimum tested dimensions, no unresolved Critical/base failure, current tested recoveries for configured High-severity failure classes, and required reflex negative controls. The default dimension minimum is three. Degraded reflects unresolved Critical/base failure; Retired follows explicit Skill retirement.

Age comparisons use full durations, including a test at exactly 30 days and one second beyond. Old response support cannot qualify a changed repository context as Hardened. A false-positive reflex must be disabled; it cannot remain active and satisfy the policy. Hardened can coexist with named UNKNOWN conditions when the configured minimum is met. It is a statement about this evidence policy, not universal safety.

## 9. Experience Package representation

A serializable package identifies the Skill, all linked operating envelopes, Lessons, Reflexes and Recoveries, plus coverage, maturity, usage/freshness summary, and versioned provenance references. Human output includes procedure, scope, artifact IDs and evidence links; JSON preserves the full index. Original local records retain scope, confidence, agent identity and environment provenance.

Packages are local indexes with append-only snapshots, not standalone portable bundles. No remote import, export trust protocol, automatic Lesson promotion, Skill procedure rewrite, or Reflex activation was added.

## 10. CLI and agent Bridge

Implemented: `curriculum plan/run/list/show/why/report/cancel`, `skill harden/package`, enriched `skill show/list`, and `task-family register/list/show`. Planning creates no Reality. `skill harden` combines plan and run. Reports show selected/deferred goals, rationale, trial results, artifacts, usage, coverage, maturity and remaining gaps. See the [reproducible commands](curriculum.md).

The authenticated Bridge accepts explicit plan requests, separate start requests, progress polling, cancellation and bounded package inspection. Agent requests default off; when enabled they require active sessions, cumulative session budgets, and verified bundled procedures in the requesting repository. The existing bounded experiment worker handles curricula. Session end cancels unstarted plans as well as queued/running curricula. Fake Claude and Codex clients exercise the same protocol. No MCP server existed, so no separate server was introduced.

## 11. Hardening test and CLI results

The four-condition test records delay **PASS**, empty required credential **FAIL**, config drift **FAIL**, and dependency fallback **DEGRADED**. Four curriculum trials create eight Experiences, two Candidate Lessons, one Candidate Reflex, two Candidate Recoveries and four envelopes. Coverage rises from 1/5 to 5/5. Maturity rises from Supported to Validated; three subsequent response trials are required before Hardened. A further plan has no remaining work. Immutable evidence, cleanup and package provenance are asserted.

The standalone `deploy-rolling-update --profile resilience-basic --budget 5` CLI smoke used a dedicated managed local fixture, not a deployment:

| Invocation | Results | New Experiences | Coverage | Maturity after |
| --- | --- | ---: | --- | --- |
| First, budget 5 | Four failures, one degraded fallback; four Lessons, one Reflex, four Recoveries proposed | 10 | 1/7 → 6/7 | Validated |
| Second, budget 5 | Four recovery tests and one negative control pass | 10 | 6/7 | Hardened |
| Third, budget 1 | Remaining delay passes | 2 | 7/7 | Hardened |

First curriculum: `curriculum-e72ee37a-3886-44de-bd82-4081aa1f127e`. Local smoke evidence is under `/tmp/hardknock-v05-smoke.uo52b2yq`; it is temporary, not committed. All 24 Realities, including the two seed runs, were discarded. SQLite integrity and foreign keys passed, the source remained clean, and only its original worktree remained. Re-running the terminal curriculum returned stored evidence without new work.

Final gates **passed**: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all`. The full suite reports **121 passed, 0 failed, 2 ignored**. The ignored tests are preexisting opt-in native Codex checks. The new suites contain 12 curriculum tests and two Bridge tests; default tests use local fixtures/sockets and no external models or network services. The separate benchmark rerun also passed. Local Markdown links and `git diff --check` were checked.

## 12. Held-out resilience benchmark

Training uses an empty credential and generation 2. The separate transfer fixture starts at generation 7 and tests a different invalid synthetic credential and generation 9. Its controller selects a stored, tested Recovery by the observed failure signature, not by the perturbation name or a supplied answer.

| Metric | No experience | Lesson advice only | Full package |
| --- | ---: | ---: | ---: |
| Held-out success | 0/2 | 1/2 | 2/2 |
| Gain over no experience | — | +0.5 | +1.0 |
| Repeated failures | 2/2 | 1/2 | 0/2 |
| Typed recovery success | Undefined: no attempts | Undefined: no attempts | 2/2 |
| Time to recovery | Null | Null | 24 ms and 27 ms in the final separate run |

The test asserts **Y > X**. Lesson-only advice is explicitly attempted by a fixture controller, including Candidate advice; production retrieval still excludes unvalidated Candidates. The full-package group executes actual typed Recovery steps after reproducing the failure and checking the precondition. Timings are local wall-clock observations, not a performance guarantee. Only two discrete local cases are tested; no statistical, live-agent, continuous-boundary or production improvement is claimed.

Reproduce with `cargo test --test curriculum held_out_resilience_benchmark -- --nocapture`. The test prints metrics and immutable Experience IDs; its temporary store is removed when the test ends.

## 13. Revalidation results

Changing the committed environment-version input produces a RevalidateOldExperience goal. The replay runs in the new recorded state, refreshes current evidence, and leaves the old Experience unchanged. A separate test first hardens the Skill, changes the commit, and confirms that old recovery support cannot harden the new context: after new fault observations it remains Validated with two recovery gaps. Measured age boundaries and runtime/environment fingerprints are also checked.

## 14. Contradiction-resolution results

A real stored contradictory Lesson from the existing package-manager fixtures generates comparisons in both recorded contexts, including legacy Trial evidence. Two two-arm experiments consume four Realities and create two review records. The original Lesson version, scope and confidence are not silently rewritten. The result recommends inspecting context boundaries; it does not invent an automatically narrower claim from two examples.

## 15. False-positive Reflex results

The retry fixture generates a candidate precursor. A negative control preserves the same prefix through its trigger but allows the next original action to succeed. The recorded test identifies a false positive, disables the Reflex, lowers its confidence to 0.30, and refuses activation. The config-drift negative control instead passes without firing; that supports the negative check only, not positive influence or activation.

## 16. Recovery-gap behavior

Known failures with Candidate recoveries receive paired tests ahead of low-severity exploration. Both failure reproduction and restoration consume the budget. Supported recovery requires a reproduced failure, matched precheck, executed typed steps and successful final evaluation. A new failure can trigger one bounded adaptive recovery test in round two.

If a High-severity class has no matching Candidate recipe to test, the planner retains a deferred, explicit recipe gap. An unrelated recipe cannot hide it; a regression assertion covers a partial inventory with one recipe missing. The planner does not invent arbitrary executable recovery code. Older supported recipes tied to another context require new evidence and do not close current gaps.

## 17. Known limitations

- Git worktrees share host processes, network, credentials, Git metadata and files outside the checkout. Capability checks and obvious-effect/exact-command guards are not a security sandbox; trusted procedures and evaluators remain required.
- Curriculum dispatch is serial. Budgets cover trial/Reality/agent slots and elapsed time, not money, memory, CPU, artifact bytes or opaque native tool calls. Reservations are charged before dispatch and never recycled after partial setup failure. Recorded usage may be smaller than reservations when no Experience could be captured.
- Cancellation awaits the current engine's process cleanup. Synchronous Git/filesystem/SQLite work can exceed the requested deadline. Hard process death, disk exhaustion and deliberately escaped sessions are not automatically reconciled; Running curricula require inspection and a new plan.
- Rich faults and typed responses use the bundled fixture runtime. General scripts support only the existing bounded top-level conditions. No cloud mutations, real credential expiry, network partitions, Docker/VM snapshots or background scheduler exist.
- Package refresh requires a reachable clean source checkout and scans local evidence. Retained historical package metadata is not a continuously refreshed agent profile. Large-inventory indexing and pruning are future work.
- Major dependency/architecture changes and hidden live-model changes are not inferred. Task families are explicit selectors over registered Skills, not automatic semantic clustering.
- Live Claude/Codex V0.5 acceptance was not run. Two preexisting optional native Codex tests remain ignored. The earlier V0.3 live integration caveats still apply.
- An initial full run hit an intermittent macOS process-group `EPERM` in the preexisting CLI cancellation test. Its isolated rerun and the subsequent full run passed; no signal error was suppressed or converted into a success.

## 18. Deviations and rationale

`TrialExecution` is an enum over existing strategy experiments, chaos campaigns and response tests rather than making every trial a synthetic `ExperimentRequest`. These engines have distinct established provenance/result semantics; reusing them preserves truthful evidence and avoids another runner.

The fixture produces two Recoveries for its two distinct failures, rather than the illustrative single Recovery. Fresh controls consume separate Realities/Experiences, so counts exceed the number of curriculum conditions. Hardened is deliberately delayed until response evidence exists.

`env:missing` means an empty required fixture credential value, not host environment-variable removal. Unknown credential expiry/revocation remains unsupported. The built-in profile is a finite set of local conditions; it does not claim the broader production scenarios in illustrative output.

Curriculum caps are separate from V0.4 strategy caps but use the shared budget type and capacity leases. Parallelism is restricted to one, maximum depth to two rounds. Curriculum quality remains conservative Medium, or Invalid on execution error; no unsupported High quality score is inferred from artifact counts. The optional status-dashboard enhancement and portable package distribution remain deferred.

## 19. Recommended next phase

**V0.6 — Persistent Agent Development:** maintain explicit agent profiles and longitudinal evidence across tasks, with task-family history, freshness/revalidation queues, stable package version references, and measured transfer/retention over time. First complete live integration acceptance, strengthen environment isolation and crash reconciliation, and improve inventory scaling. Keep future scheduling separately opt-in, budgeted and cancellable; do not infer permission for background work from this implementation.
