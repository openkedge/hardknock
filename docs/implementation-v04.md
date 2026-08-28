# V0.4 implementation report

Version: `0.4.0-dev.1`. Local verification: macOS arm64. V0.4 proceeds under the updated request; the incomplete V0.3 live acceptance work remains explicitly open.

## 1. Files created and changed

New implementation files:

```text
src/budget.rs
src/experimentation/mod.rs
src/experimentation/model.rs
src/experimentation/config.rs
src/experimentation/comparison.rs
src/experimentation/orchestrator.rs
src/store/experiments.rs
src/bridge/experiments.rs
src/cli/experimentation.rs
migrations/007_agent_experiments.sql
```

New verification/documentation files:

```text
tests/agent_experiments.rs
tests/experiment_bridge.rs
fixtures/strategy-choice/agent-script.sh
fixtures/strategy-choice/test.sh
fixtures/strategy-choice/api-version
fixtures/strategy-choice/consumer-version
fixtures/strategy-choice/hardknock-fixture.json
fixtures/confounded-comparison/agent-script.sh
fixtures/confounded-comparison/test.sh
fixtures/confounded-comparison/api-version
fixtures/confounded-comparison/consumer-version
fixtures/confounded-comparison/hardknock-fixture.json
docs/agent-experiments.md
docs/experience-budget.md
docs/experiment-quality.md
docs/implementation-v04.md
```

Updated files:

```text
Cargo.toml
Cargo.lock
src/lib.rs
src/core.rs
src/experience.rs
src/dojo.rs
src/workflow.rs
src/learning_loop.rs
src/store.rs
src/cli.rs
src/bridge/cache.rs
src/bridge/config.rs
src/bridge/engine.rs
src/bridge/mod.rs
src/bridge/protocol.rs
src/bridge/recording.rs
src/bridge/transport.rs
integrations/codex/fixtures/fake_app_server.py
tests/learning.rs
tests/substrate.rs
README.md
docs/architecture.md
docs/bridge-protocol.md
docs/agent-experience-contract.md
docs/experiments.md
docs/cli.md
docs/integrations.md
docs/roadmap.md
```

## 2. Database migration

Migration **007** adds `experiment_requests`, `experiment_candidates`, `experiment_variables`, `experiment_relations`, and `experiment_progress`, with explicit foreign keys, query indexes, immutable request/budget/terminal-result guards, append-only progress/lineage, and a unique candidate-Experience index. Terminal status/result and terminal progress commit atomically.

The old `experiments`/`trials` tables require a prior Lesson/Hypothesis and exactly two trial positions, so they cannot represent arbitrary requested candidates without changing established semantics. New records reuse the existing Realities, executions, evaluations, Experiences and artifacts. Existing JSON is not rewritten; new provenance fields default absent. Existing migration preservation and foreign-key tests pass at schema version 7. Back up the database before upgrading; no down migration is provided.

## 3. Request schema

`ExperimentRequest` has typed request/session provenance, question, optional hypothesis, explicit candidate executions, starting state, evaluator, budget, requester, creation time, criteria, origin, intent and capabilities. Candidates support Shell command arrays and AgentTask prompts with optional configured agent identity. The portable Bridge DTO deliberately omits caller-selected starting workspace/requester fields: it derives those from the registered session. See the [schema table and examples](agent-experiments.md#structured-request).

## 4. Experience Budget semantics

Core defaults are three Realities, three agent runs and five minutes. Local configuration clamps request ceilings; the effective budget is retained separately. The strict policy rejects the whole comparison rather than silently reducing alternatives. Provider capacity is leased before creating worktrees. Explicit shell entries and all evaluator commands can be capped; opaque native tool-call caps are rejected as unenforceable. Session allocations are cumulative for agent requests, including cancelled/failed reservations. No financial/token accounting or hard CPU/memory/disk quota is claimed.

## 5. Equivalent-state guarantees

Every candidate starts from the same recorded commit/tree and tracked fixture inputs. A composite fingerprint covers controlled environment facts, executor/template hashes, reported identities and evaluator specification. All prepared worktrees are verified at one barrier, then each worker checks its own start and executor before execution. Each immutable Experience records that same fingerprint. Tests reject changed source inputs and a candidate worktree modified after forking, with no candidate execution in either rejected comparison.

The proof excludes ignored dependencies, live process state, inherited native configuration, remote models and host service state. The result says so. Test oracles and commands must be trusted; Git worktrees cannot prevent candidates from modifying tests or host state.

## 6. Parallel execution design

All required Realities are prepared before execution. A bounded JoinSet launches candidate workers, each owning its own SQLite connection and current-thread Tokio runtime, using the shared `workflow::run_prepared_trial`. Configured parallelism defaults to three, or two for agent requests. Tests verify overlapping execution at limit two and nonoverlap at limit one. The Bridge uses a separate bounded experiment queue, keeping candidate execution out of the action handler and lifecycle recording worker.

## 7. Comparison policy

`EvaluatorSuccessFirst` validates outcomes, fingerprints and evaluator specifications. It ranks evaluator success, then fewer failed checks, and uses text diff size/duration only when explicitly enabled. Missing/binary diff metrics are not used. Passing ties, all-failed required-success comparisons, one-candidate replays and interrupted/unconfigured evidence produce no winner. Successful execution is also required for recommendation. Reasons and qualitative evidence weighting are returned; `confidence` is null.

## 8. Experiment quality

Derived variables are strategy, agent/version, model, executor configuration and environment. Multiple changed variables mean Confounded. Nonfixture agents or inherited environments are at most PartiallyControlled. Failed equivalence/evidence collection is Invalid. Known local fixtures with strategy as their only changed variable are Controlled within the measured scope. Confounded/invalid evidence never generates an automatic causal Lesson. A controlled failing/passing pair may propose one scoped Candidate Lesson; it is not promoted or converted into automatic executable advice.

## 9. Bridge additions

`experiment_requested` accepts deliberate structured alternatives. `experiment_accepted`, `experiment_progress`, `experiment_completed`, `experiment_rejected`, and `experiment_cancelled` communicate the lifecycle. Progress uses an exclusive persisted cursor over the existing one-request-per-connection JSONL transport. Results include evaluator/check outcomes, diff metrics, proof, quality, recommendation/reasons, provenance IDs and usage, without raw transcripts. Request IDs are idempotent and conflicting reuse is rejected. Claude and Codex receive the same helper contract through their existing context adapters. No MCP server was added.

## 10. `hardknock try` UX

The exact definition-of-done command works in the initialized `strategy-choice` fixture. Human output shows PASS/FAIL, quality, proof, candidates, diff metrics, recommendation, budget usage and Experience/Lesson counts. JSON is a single final structured result; human progress goes to stderr. Explicit `NAME=STRATEGY` avoids guessing how two positional strings should be executed. `--session` issues the same operation during an integrated session. `experiment show/list`, `why --experiment`, `reality tree`, and integrity-checked, nonoverwriting patch export are implemented.

## 11. Replay, lineage and cancellation

Replay/all or replay/candidate creates fresh immutable records; extension preserves originals and appends alternatives under the same budget checks. Parent-child relations are persisted, and changed measured runtime fingerprints are disclosed. Cancellation is available through CLI, API, Bridge, process signals and session end. It joins launched work, kills ordinary process groups, discards remaining Realities and retains interrupted Experiences. Terminal status and progress are committed together. `continue_after_session_end=false` is the default; user CLI requests are independent.

Live process/session snapshotting, automatic crash recovery, reattaching to abandoned native processes and automatic adoption are not implemented. SIGKILL/power loss may leave nonterminal rows/orphans; inspect, stop abandoned processes, clean up and replay as new evidence.

## 12. Controlled fixture result

**PASS:** same fake agent, same start, same evaluator. Direct: execution succeeded but required check failed; staged: execution and required check passed. Quality Controlled; staged recommended; two Experiences; one Candidate Lesson; source unchanged; both worktrees discarded.

A separate human CLI smoke produced `experiment-8cf789d5-2497-43af-9779-82f6e582e95d`, with two agent runs and two Realities. The observed total duration was 620 ms on this machine; this is not a performance guarantee. Git status was clean afterward and the source was the only remaining registered worktree.

## 13. Confounded fixture result

**PASS:** fake-agent-A/strategy-A fails and fake-agent-B/strategy-B passes. B is recommended for this trial, quality is Confounded, both changed variables are exposed, and no Candidate Lesson is generated.

## 14. Budget results

**PASS:** five candidates with a two-Reality budget reject before creating any Reality. Agent-run caps, evaluator-inclusive shell caps, unenforceable native command caps, provider-capacity rejection, session cumulative spending, and deadline cancellation are covered. Duration cancellation waits for evidence capture/cleanup; synchronous Git operations are not a hard wall-clock sandbox.

## 15. Equivalent-state rejection and other deterministic gates

**PASS:** expected fingerprint drift after changing tracked input and post-fork candidate worktree drift both refuse execution. Other tests cover literal task argv, passing ties, explicit diff tie breaking, bounded concurrency, killed child processes, retained interrupted Experiences, session end, disabled agent requests, cross-session rejection, duplicate IDs, replay/fork lineage, patch applicability/overwrite refusal, immutable terminal storage and foreign-key consistency.

Final default suite: **107 passed, 2 ignored** (17 new experiment tests). The ignored tests are preexisting opt-in real Codex tests. `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all` are the required gates. Tests use no external models or external network; Bridge tests use local sockets. The Codex fake-server timeout fixture was updated to locate the task across text inputs now that experiment context can precede it.

## 16. Optional live Claude/Codex results

**Not run for V0.4.** Fake Claude and Codex sessions both complete authenticated request → progress → result, and native adapter fixture tests remain passing. This is not a live model demonstration. The V0.3 report's live acceptance limitations still apply. No credentials, approval overrides, or global agent configuration were installed/changed for this phase.

## 17. Known limitations

- Git worktrees share host files, processes, credentials, Git metadata and network. Capability declarations and obvious-effect/exact-command guards are not a security sandbox.
- Equivalent-state proof is scoped; native configuration, dependency/service changes, test-oracle mutation and nondeterminism remain outside full control.
- Cancellation cannot stop processes that deliberately escape their group. Git bookkeeping and teardown can exceed the requested deadline; artifact disk quotas are absent.
- Native candidate execution uses trusted argv templates; the Codex default invokes `codex exec`, preserving native settings. The requesting session uses the Bridge; candidate internal tool counts and native transcripts are not imported into a second orchestration engine.
- Operational experiment prompts/commands/diffs are persisted privately; avoid secrets. Bridge summaries omit raw content but generic artifacts are not comprehensively redacted.
- Candidate Lessons refer to specific candidates and need a separate validation recipe before becoming general command advice. No automatic strong lesson, commit, adoption, or confidence inflation occurs.
- Crash reconciliation, Docker/MCP, interactive expensive-request approval, generalized Script execution and automatic curriculum initiation are deferred.

## 18. Deviations and rationale

The suggested Arc/dynamic-trait orchestrator is implemented with the existing concrete provider, borrowed configuration/store and per-worker connections, avoiding a rewrite of the non-Sync SQLite and async-trait architecture. `run` returns a persisted experiment status wrapper containing `ExperimentResult`, so rejection and partial failure remain inspectable. New candidate tables are necessary because old trials are paired and Lesson-bound. Shared execution/evaluation remains in `workflow`, not duplicated.

Strict rejection was chosen over reduced candidate sets. Both completion and failure are returned as structured status, and a completed comparison exits zero even without a winner. Replay freezes the repository commit but remeasures the runtime; environment changes become new evidence. Native context/helper delivery implements the portable contract without adding a server that was not already present. Safe patch export is implemented; automatic commit/adopt and ambiguous two-positional-alternative syntax are deferred. Agent-requested chaos/recovery is rejected with direction to the existing engines rather than represented by fake strategy trials.

## 19. Recommended next phase

**V0.5 — Skill Hardening and Autonomous Curriculum:** use this common budgeted trial surface to identify unexperienced conditions, design controlled tests, stress candidate skills, discover weak spots and propose deliberate experiential practice. First keep curriculum generation explicit and budgeted, finish live integration acceptance, strengthen isolation/quotas and crash handling, and require independent validation before expanding a skill's operating envelope or autonomy.
