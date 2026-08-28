# V0.2 implementation report

Local verification: 2026-08-27, macOS arm64. Version: `0.2.0-dev.1`. No external model, network service, infrastructure fault injector, or new agent integration was used for acceptance tests.

## 1. Files created and changed

| Area | Files |
| --- | --- |
| Perturbations | New `src/perturbation.rs`; extended `src/core.rs`, `src/process.rs`, `src/agent.rs` |
| Resilience domain/runtime | New `src/resilience/{mod,models,fixture,campaign,reflex,runtime,testing}.rs`; shared `src/workflow.rs` and evaluator integration |
| Persistence and provenance | New `src/store/resilience.rs`, migration 005; updated `store.rs`, `store/experiences.rs`, `store/transfer.rs`, `experience.rs`, `application.rs`, `lesson.rs`, `explanation.rs` |
| CLI | New `src/cli/resilience.rs`; extended `src/cli.rs`, module exports, Cargo version/lockfile |
| Fixtures | New `fixtures/retry-resilience/`, `fixtures/stale-credential/`, `fixtures/config-drift/`; each includes marker/kind, operation/check/replan/read-state/refresh scripts, generation/plan/token inputs |
| Tests | New `tests/resilience.rs`; migration expectations updated in `tests/learning.rs` and `tests/substrate.rs` |
| Documentation | Updated README, architecture, experience model, experiments, retrieval, agent integration, CLI, roadmap; new chaos, operating-envelope, reflex, recovery guides and this report |

Lottie/`mascot.png`, Apache-2.0 `LICENSE`, and `NOTICE` are preserved. No new dependency was added.

## 2. Schema migration

`005_resilience.sql` adds explicit perturbations, Skills, campaigns, planned conditions, trials, Experience/condition links, trial/Lesson links, envelopes/versions/observations, Reflexes/versions/Lessons/evidence/matches, Recoveries/versions/steps/evidence/attempts, and paired resilience tests. Foreign keys connect every derived object to immutable evidence.

The migration rebuilds the two constrained relation/evidence tables to add `chaos_variant_of`, `recovery_of`, and `narrows`, preserving existing rows and immutable triggers. Historical Execution/Experience/Lesson JSON is not rewritten. Optional Experience resilience fields default absent. V1, V3, and populated V4 migration tests pass; newer schema versions are still rejected. There is no down migration: back up the data home before upgrading.

## 3. Perturbation types

EnvironmentVariable, FileMutation, CommandFailure, and CommandDelay are implemented. Handles carry child environment/command effects and reverse file mutations in reverse application order. No process-global environment or source checkout is mutated. Path traversal, `.git` paths, symlinks, hard-linked targets, reserved environment keys, and oversized inputs are rejected. Partial application errors unwind earlier mutations.

Normal termination/cancellation cleans worktrees after evidence capture. Capture/storage errors preserve a Reality for inspection; host termination requires orphan cleanup. This is not a security sandbox or universal rollback guarantee.

## 4. Campaign semantics

A persisted plan precedes a fresh unperturbed control. Successful execution plus all required checks must pass before perturbations begin. Unhealthy controls abort with no perturbed execution or envelope. Every executed variant gets a fresh Reality and atomically linked Experience/Trial, with a `ChaosVariantOf` control relation.

Trial budgets are hard: default 10 perturbed trials, at most 100; control is one additional run. Dispatch duration and per-action/evaluator timeouts are bounded and documented. Plans retain commit/tree, conditions, goal, commands/checks, environment facts, agent, fixture/runtime/binary versions, and active Reflex snapshots. Inspection reconstructs partial trial lists from committed rows.

## 5. Operating-envelope representation

Each completed/partial campaign with variants emits an immutable version-1 envelope. All classifications refer to exact tested points. `AllUntestedConditions` remains explicit, and inconclusive points remain unknown. There is no interval interpolation, extrapolation, continuous coverage percentage, or inferred exact failure boundary. Different campaigns produce different envelopes rather than rewriting historical conclusions.

## 6. Reflex lifecycle and matching

Failures propose Candidate rules at heuristic confidence 0.58. The deterministic matcher requires repository/markers/tags/OS/architecture scope, exact proposed operation, and configured repeated-failure/no-state-change or stale-config precursor. It runs at fixture lifecycle boundaries only.

Paired failure without the rule and evaluated success after a matched replan support the rule at 0.84. Activation remains explicit (`Supported → Active`), with explicit disable. Test-only matches never activate a rule globally. Advise/Warn/Replan/Block are modeled; automatic generation uses Replan, and Block is rejected for generation, activation, and execution. Source Lessons remain Candidate under their separate evidence policy.

## 7. Recovery lifecycle

Chaos failures propose bounded typed procedures at 0.42. A test records a failure-only replay, then reproduces and checks the failure in the response Reality before applying any steps there. Steps include shell commands, child environment changes, and replan/retry. Successful reproduction, restoration, and final evaluation yield Supported at 0.81. Failed restoration after reproduction yields Contradicted at 0.25. Missing reproduction, interruption, or timeout is inconclusive. Repetition alone does not produce Validated status.

Each RecoveryAttempt records signature, reproduction, attempted/succeeded, procedure execution time, step count, immutable action artifacts, and `RecoveryOf` lineage. Revisions and paired conclusions commit atomically against the latest stored revision.

## 8. CLI commands added

`chaos run/list/show/report`, `envelope list/show`, `reflex list/show/test/enable/disable`, `recovery list/show/test`, and `skill register/list/show` are functional. Campaigns accept Task, Command, or Skill targets, four built-in profiles, explicit conditions, and delay sweeps. `why` explains historical Reflex matches and their source evidence; `status` includes resilience resource counts.

JSON keeps one stdout result and sends campaign progress events to stderr. Reports expose task outcomes, retries, failure detection, paired false positives, recovery attempts/successes, and tested-point counts. Unmeasured/empty rates remain `null`. See [CLI semantics](chaos.md#json-and-exit-codes).

## 9. Deterministic chaos test results

**67 tests pass:** 8 unit, 8 CLI, 15 learning, 17 resilience, 7 substrate, and 12 transfer. `cargo fmt --check`, strict `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all` pass using the cached toolchain/dependencies offline. Linux CI is configured but was not run remotely in this pass.

| Retry condition | Observed result |
| --- | --- |
| Control | PASS |
| delay=0ms | PASS |
| delay=100ms | PASS |
| delay=500ms | PASS |
| delay=1000ms | DEGRADED |
| delay=2000ms | FAIL, `retry_exhaustion` |

The failure has six failed operation attempts, five retries, immutable output/diff evidence, a scoped Candidate Lesson, Reflex, Recovery, and linked envelope. Tests also cover unhealthy controls, all four perturbations, budgets, manual Skill targeting, cancellation, immutable storage, migration, clean source trees, and worktree cleanup.

A separate CLI acceptance run exercises the documented bundle sweep, test/activation/inspection commands, all three recoveries, scope exclusion, a false positive, Skill registration/targeting, reports, and JSON progress. It retains 26 Experiences, 14 chaos Trials across 5 campaigns, and 6 paired tests in a temporary data home. SQLite integrity/foreign-key checks passed; all bundled source trees stayed clean and all trial worktrees were removed. This is additional local verification, not a general agent benchmark.

## 10. Reflex validation results

For retry exhaustion, the without arm fails after six attempts. The tested Reflex recognizes three failures without state change, replans to the explicit alternative, and the evaluator succeeds. The rule becomes Supported, not Active. After explicit activation, subsequent matching fixture runs record observed non-test matches. `why` retains the exact historical version even after disabling the rule.

The successful response can retain a DEGRADED resilience classification because retries/logical duration still exceed nominal control. This preserves the cost of replanning rather than disguising it as an unaffected execution. Task evaluation is PASS. Configuration drift additionally demonstrates a pre-apply stale-plan match and successful re-read/replan. Concurrent tests preserve both paired evidence sets and revisions.

## 11. Recovery validation results

The credential profile changes only a simulated token state. Both failure-only replay and the response Reality first exhibit six authentication failures. The response's precheck fails before token refresh, explicit environment repair, state re-read, and retry. The final evaluator passes and Recovery becomes Supported. A deliberately unavailable refresh operation produces Contradicted evidence after one failed recovery step; it never claims success. Configuration drift and the retry alternative also recover under their explicitly tested conditions.

## 12. False-positive and scope results

Three transient command failures followed by a successful fourth original operation produce a measured false positive. The paired trace verifies the exact next action and its recorded state fingerprint. Evidence remains visible, the overbroad rule becomes Disabled at 0.30, and a later positive replay cannot erase the negative result or re-enable it.

A matching failure count in another fixture/repository does not fire the rule. Wrong proposed actions, observed state changes, and inactive candidates also fail the matcher gates. A real second fixture campaign with the earlier rule Active confirms scope exclusion, not merely a mock score check.

## 13. Known limitations

Git worktrees share the host, credentials reachable by other paths, network, and repository configuration. Only trusted local commands are in scope. Fixture logical time is simulated; general Command duration is scheduler-sensitive. The environment fingerprint is not a complete toolchain/host snapshot. Explicit parameters/logs may contain secrets; no general redaction, quota, artifact verification-on-read, or garbage collection is added.

No automatic continuous boundary search, campaign aggregation, Skill synthesis/revision, context narrowing, calibrated confidence, recovery replication validation, generic pre-tool interception, arbitrary recovery registration, or automatic crash resumption exists. User TOML profiles and full chaos replay remain deferred. Restart/cleanup and filesystem/SQLite atomicity limits remain documented.

## 14. Design deviations and rationale

The exact `--fixture` demo creates a versioned bundled source under the data home so it works without modifying the user's checkout. Control is optional while a campaign is incomplete, so interrupted setup is representable. Sparse `ConditionRange` values are tested-point references, not guessed intervals. The trial model links Execution/Evaluation IDs instead of duplicating full evidence.

`--max-duration` bounds starting further work, not every synchronous Git operation or the final evaluator; `--trials` remains a hard execution count. JSON events use stderr to preserve the established stdout contract. Restoration is observed inside one response Experience after induced failure, alongside a separate failure-only Experience; there is no illustrative clean-state substitute. A single observed Skill or recovery is Supported, never fabricated as Validated. `Narrows` is represented without implementing unsafe automatic scope edits.

## 15. Recommended next phase

Define stable, versioned **Experience Query, Experiment Request, Reflex Evaluation, and Evidence Reporting** contracts. Then add explicitly permissioned MCP/API surfaces, selected real-agent adapters and lifecycle hooks, portable evidence exchange, and cross-agent validation. Preserve the observed/self-reported distinction and validate scope on every reuse. Strengthen isolation and artifact/crash handling before broadening perturbations. No such integrations were started in V0.2.
