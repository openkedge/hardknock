# V0.6 implementation report

V0.6 adds persistent development over the existing local evidence engines. It does not train model weights, run a background scheduler, or establish production reliability. All numbers below come from local evaluated fixtures or explicitly labeled synthetic load tests.

## 1. Files and architecture

New implementation: `src/development/{mod,model,policy,profile,benchmark}.rs`, `src/store/development.rs`, `src/cli/development.rs`, and migration `009_development.sql`. Integration changes touch `core`, `store`, `store/curriculum`, `retrieval`, `application`, the CLI modules, Bridge cache/config/engine/protocol, and package version metadata. New regression coverage is in `tests/development.rs`; existing Bridge and migration tests are updated. No dependency was added.

The README, development guide, CLI/architecture/Bridge/retrieval references, and roadmap describe the implemented boundaries. The new abstraction is a projection of evidence, not a second source of outcomes.

## 2. Migration and retention

Migration 009 adds the compact observation view, indexed agent/repository chronology, profile cache, immutable snapshots with evidence links, development episodes, Skill revisions, package revisions, revalidation queue, regression records, and benchmark runs/metrics. Skill revision 1 is backfilled from original Skill evidence. Existing Lesson revisions are reused.

Original Experience/Execution/Lesson/Skill JSON is not rewritten. Snapshot, revision, terminal episode, revalidation and benchmark mutation guards retain history. Membership guards prevent adding arbitrary evidence to recorded snapshots/episodes. SQLite integrity and foreign keys are tested, including migration from a populated V0.5 database (schema 008). Back up before upgrading; no down migration is provided.

## 3. Experience Profiles

Agent, Repository, TaskFamily and SharedLocal subjects are implemented. Agent kind is distinct from agent version, model and measured configuration fingerprint. Default agent profiles span model changes; optional filters restrict them. Repository paths are canonical, task-family scope comes from registered examples, and SharedLocal is only local aggregation. Workspace/organization subjects fail explicitly as unsupported.

Profiles include artifact inventory, metric definitions/counts, per-Skill coverage, freshness, validation efficiency, contributors and policy hashes. Source agent and context are retained when another agent applies a Lesson. Current artifact inventory is independent of the selected metric window.

## 4. Snapshots and comparisons

Snapshots are explicitly created or captured around episodes. They retain computed metrics, evidence IDs and policy/configuration versions. Rebuilding a cache does not reinterpret a stored snapshot. Date comparison selects actual stored snapshots; IDs select exact records.

Growth compares the last two completed episode windows when available. It refuses a trend claim for overlapping evidence, different subjects/policy hashes/window types, nonchronological snapshots, unknown values, or insufficient samples. Raw deltas remain descriptive. There is no interpolated history or fabricated weekly curve.

## 5. Metric definitions

Every rate exposes its numerator, denominator, period, confidence label and definition. Zero samples means `value: null`, not zero performance. Confidence labels are conservative sample-count heuristics; they are not confidence intervals.

| Metric | Profile denominator / interpretation |
| --- | --- |
| Task success | Conclusive evaluated task attempts; retries count, internal experiment/chaos/response arms do not |
| Repeated mistake | Task attempts with observed actions; numerator requires a recorded matched-Lesson avoid action |
| Repeated failure | Encounters of a previously observed concrete signature in the same repository; generic check failure excluded |
| Recovery success | Reproduced, attempted typed Recoveries |
| Recovery latency | Median successful typed Recovery time, with separate sample count; failed recoveries are not assigned a time |
| Experience transfer | Delivered, observed applications in a different source tree; success plus Applied is beneficial |
| Lesson precision | Successful outcomes among observed Applied items; association, not causal precision |
| Reflex false positives | Paired tests where a Reflex fired and the test could classify it |
| Experiment success | Conclusive paired Lesson experiments among associated recorded experiments |
| Curriculum yield | New Lesson/Reflex/Recovery artifacts per executed trial; may exceed one |
| Portability | Beneficial observed applications of versions already Validated when delivered to a changed agent/version/model |

Hardened Skill count is an inventory count. Strategy-comparison experiments are not pooled into paired-Lesson experiment success. No universal intelligence score is computed.

## 6. Skill and Lesson revisions

Skill revision requires successful, evaluated, unperturbed replayable evidence in the existing scope. It appends procedure/context/evidence lineage and does not rewrite the original Skill. A new procedure cannot inherit an older package's Hardened label.

`lesson history` exposes the existing immutable Lesson versions and exact evidence relationships. Changing the tested claim, scope or actions still requires a new hypothesis; a mere metadata edit cannot confer support. Automatic merging, scope narrowing and destructive compaction are not implemented.

## 7. Freshness and reinforcement

Fresh/Aging/Stale/Contradicted/Retired states combine status, last support, context and age. Superseded is reserved for richer artifact evolution; prior revisions remain addressable without being active heads. Age alone does not invalidate a Lesson. Old evidence plus material runtime/repository change lowers its retrieval rank; source repository context survives a later transfer.

New support appends evidence and can refresh the supporting context. Contradictions are not erased by another success. Configuration/model changes are context signals, not independent replication. The freshness heuristic is not a guarantee across all historical environments; per-condition envelopes remain the more precise record.

## 8. Revalidation and maintenance

Health inspects evidence. Maintain records deduplicated revalidation recommendations and reports likely duplicates without merging them. Requests preserve artifact revision, reason, repository and runtime context. They never auto-run.

`revalidation run` executes Lesson requests through the existing paired engine after checking the current revision, commit and runtime. It stores the actual experiment ID and terminal result. Direct queue dispatch for Skills/Reflexes/Recoveries is deferred; existing `skill harden`, `reflex test` and `recovery test` are the explicit paths. Changed-scope proposals do not bypass scope gates.

## 9. Active retrieval and memory pressure

Cold retrieval and the hot cache share scope and freshness scoring. Defaults cap active advice at five Lessons, three Reflexes and three Recovery references. Stale/contradicted cached items do not automatically advise or replan. User policy remains the only blocking authority.

Cold freshness reads only linked source/support observations; it does not load all raw Experience records. Cache construction resolves support once. Pre-tool evaluation reads no database or filesystem, calls no model and starts no experiments. Full archival evidence remains available regardless of activation limits.

## 10. Regressions and efficiency

Disjoint windows with at least five observations per compared metric use a default five percentage-point change threshold. Lower repeated-mistake/failure/false-positive rates count as improvement. A regression produces a review recommendation, not a started curriculum.

Experiences-to-validation counts unique linked Experience IDs present by the first Validated Lesson revision, including referenced controlled trials. It does not count all unrelated store traffic. Unvalidated artifacts remain UNKNOWN. Median latency and Hardened inventory changes are descriptive; they are not presented as statistical trends.

## 11. Timeline, growth and agent context

Timeline includes Experience outcomes, artifact revisions, episodes and benchmark records. Skill/Lesson/agent/time filters and bounded history inspection are available. The timeline scan is capped at the latest 20,000 events; it is not unbounded archival pagination.

The optional Bridge development bundle exposes bounded IDs, known unknowns, stale/contradicted items and recommendations. It is off by default, redacted and subject to the existing serialized response cap. An over-budget optional bundle is omitted. Projection work is outside the session mutex. No new session scheduler or autonomous experiment trigger is introduced.

## 12. Longitudinal benchmark design

`benchmark longitudinal` executes five episodes with six evaluated tasks per arm per episode: three workspace tasks and three credential faults. Episodes cover initial failures, learning/recovery, a related repository, model replacement, and an updated environment that makes the old rule wrong. Fixture version is `longitudinal-fixtures-v1`.

All tasks use actual subprocess execution, evaluation, immutable Experience persistence and worktree cleanup. The pnpm operations are deterministic shell simulations, not network package-manager calls. Credentials are local fixture values. Hardknock learning uses one paired Lesson experiment and two bounded curriculum rounds of four and three trials, with all internal arms recorded. A later revalidation uses the same paired engine.

## 13. Arms and reproducibility

Stateless receives no retrieval or saved preference. Reflection memory saves deterministic text from the actual first failure, then retains that preference without controlled validation or freshness handling. Credential failure handling in both baselines retries the unchanged operation. Hardknock uses supported retrieval, a hardened package and tested recovery, then explicitly revalidates contradicted advice.

Baseline stores are isolated under the benchmark home; the main store contains the Hardknock arm. Every baseline Experience ID resolves in its recorded arm home. Results retain application/fixture versions, source trees, configuration, agent/model identities, starting empty evidence state, task IDs, per-episode metrics and learning curves. No randomness or external model is involved. Existing populated/configured homes are rejected to prevent prior-evidence leakage.

## 14. Measured results

| Arm | Success | Repeated mistakes | Repeated failures | Recovery |
| --- | --- | --- | --- | --- |
| Stateless | 3/30 (10.0%) | 25/30 | 25/30 | 0/15 |
| Reflection memory | 9/30 (30.0%) | 18/30 | 18/30 | 0/15 |
| Hardknock | 23/30 (76.7%) | 4/30 | 4/30 | 12/15 |

Per-episode successful tasks: Stateless **0, 0, 0, 0, 3**; Reflection **0, 3, 3, 3, 0**; Hardknock **0, 6, 6, 6, 5**. All are out of six.

The benchmark's repeated-mistake metric audits failed reuse of a previously failed strategy in the same task family/environment version. Unlike normal profile metrics, this harness audit is defined even when the baseline has no Lessons. Benchmark recovery includes unchanged retries after an observed fault; profile recovery counts only typed Recovery attempts. These definitions are recorded, not silently pooled.

Hardknock's successful recovery latency was **25 ms median across 12 cases**; baseline successful latency is UNKNOWN because neither baseline recovered. No “faster than baseline” claim is made without baseline successes. The run also measured beneficial transfer 6/7, Lesson precision 9/10, conclusive paired experiments 2/2, one Hardened Skill out of one, and seven linked Experiences to the first validated Lesson. Reflex false-positive rate remained UNKNOWN (no classified firings), not zero.

The [machine-readable result summary](benchmarks/v06-longitudinal-summary.json) records benchmark `benchmark-c7df2b11-034c-40ad-b633-a4cb90e7e571`, task IDs, source trees, configuration, per-episode rates and the learning curve. Machine-specific home paths are omitted from that summary. The standalone result and canonical stores were retained in the dedicated local benchmark home.

## 15. Stale-rule challenge

The environment update changes the evaluator so the formerly discouraged default is correct. Hardknock initially applies the old advice and fails; explicit paired revalidation records `Contradicted`, retaining the old successful evidence. Its remaining two tasks succeed without that advice. Reflection memory continues the old preference and fails all three. Stateless already chooses the newly correct default and succeeds 3/3.

Thus Hardknock beats reflection memory on this challenge, but does not beat stateless on every subset. A separate age/context regression test verifies old unsupported evidence is downranked and queued without deleting it.

## 16. Model and agent portability

Episode four replaces `fixture-agent-a-v1` / `deterministic-a` with `fixture-agent-b-v1` / `deterministic-b`. The new identity successfully applies the old Validated Lesson on 3/3 observed opportunities. Original source agent, controlled-test contributors and new application IDs remain inspectable through the provenance API. Identity change does not increase distinct-context counts by itself.

These are deterministic adapter identities, not live Claude/Codex measurements. Successful two-agent live acceptance remains pending. The final all-time portability rate also includes the later failed application after environment change; it must not be confused with the isolated migration episode's 3/3.

## 17. Performance and quality gates

On macOS arm64, the synthetic load regression used **10,004 Experiences, 1,001 Lessons and 100 Skills**. It duplicated complete local fixture records with explicit synthetic labels; those rows are never used as longitudinal outcomes. Observed debug-build times: profile rebuild **1,049 ms**, cold retrieval **43 ms**, 200-event timeline **141 ms**, 1,000 cached Lesson decisions **7 ms**, snapshot history read **11 ms**. Broad limits are 10 seconds for profile/retrieval and 5 seconds for the other operations.

The separate existing full Bridge handler test with **1,000 Lessons and 1,000 Reflexes**, 200 proposed actions, observed P95 **14,679 µs**, below its 25,000 µs gate. This includes synthetic matching/ranking work, not only an empty-cache fast path.

Local gates passed: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all`: **134 passed, 2 ignored**. The ignored tests require an optional native Codex runtime. No remote CI or live vendor-agent acceptance is claimed.

## 18. Package versioning and export

Numbered package revisions pin Skill revision, included artifact versions, finite coverage, maturity and a BLAKE3 evidence hash. Generation timestamp alone is excluded from the hash. History/diff shows version and coverage/maturity changes. Existing V0.5 package snapshots remain available in the original table; numbered revisions start with post-upgrade generation.

Profile exports omit raw outputs/procedures. Package exports are private, non-overwriting local reference manifests marked untrusted when shared. They are not complete portable execution bundles. No importer, remote sharing service or automatic trust promotion was added. Legacy package snapshot and numbered revision writes are separate transactions; interrupted generation is retryable.

## 19. Limitations and deviations

The system supports local deterministic development measurement, not longitudinal production evidence over weeks. Training compute is not equalized across arms. Task families are explicit; distinct Git trees are a proxy for context diversity, not proof of independence. Confidence and trend thresholds are heuristics.

Workspace/organization profiles, imported artifact lifecycle, automatic merge/compaction, general goal optimization, richer supersession, native-model acceptance, background scheduling, and crash resumption remain deferred. Profiles rebuild on demand; there is no incremental aggregation daemon. Revalidation is explicit and does not automatically synthesize a narrower rule after contradiction. Full-history retrospective Skill/package as-of reconstruction is not claimed; use the recorded immutable snapshots.

Git worktrees still share host filesystem, credentials, Git objects and network. Only trusted commands should run. There is no cloud service, neural training, universal intelligence score, federation, or new permission bypass.

## 20. V0.7 direction

Experience Federation and Team Learning should begin with explicit exchange manifests, signatures, origin/context preservation, candidate-only import, local revalidation, revocation and conflict handling. Authentication and ownership must precede shared execution. A shared package must not convert another agent's confidence into local authority. Preserve current unknowns, artifact history and local policy boundaries while finishing live integration acceptance and stronger isolation.
