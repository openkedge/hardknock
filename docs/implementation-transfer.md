# Retrieval, retry, validation, and transfer — phase report

Verified locally on 2026-08-27, macOS arm64, Rust/Cargo 1.98.0. This extends the [Milestones 3–6 implementation](implementation-phase-3-6.md).

## 1. Files created and changed

| Area | Files |
| --- | --- |
| New modules | `src/retrieval.rs`, `application.rs`, `validation.rs`, `learning_loop.rs`, `explanation.rs`, `store/transfer.rs` |
| Existing code | CLI, core types, Experience, Lesson, Experiment, reflection, workflow, store and module exports |
| Schema | `migrations/004_transfer.sql` |
| Fixtures | Version-2 A; new transfer B, ordinary npm C, and contradiction D |
| Tests | New `tests/transfer.rs`; expanded learning, substrate and fixture support; validation unit tests |
| Docs | README, architecture, CLI, Experience model, experiments, roadmap; new retrieval guide, agent contract and this report |

The mascot, Apache-2.0 LICENSE, SPDX identifier and NOTICE are retained. No dependency or remote service was added. Changes are local commits; nothing was pushed.

## 2. Schema migration

Migration 004 adds immutable `lesson_applications`, `application_artifacts`, `experience_relations`, `repeated_mistakes`, and `lesson_validations`. Foreign keys link exact Lesson revisions, Experiences and artifacts. Existing `lesson_evidence` remains the normalized evidence table.

An immediate transaction saves Experience, application/lineage/mistake observations and any Lesson evidence/validation revision together. Concurrent runs load the current revision under that transaction. The v3 migration regression verifies old Experience JSON remains byte-for-byte unchanged; new collections default empty. There is no backfill, scope broadening or destructive down migration.

## 3. Retrieval algorithm and rationale

Required repository/marker/tag/OS/architecture constraints are hard gates. Eligible supported/validated Lessons receive markers +0.40, required repository +0.20, exact avoid-action match +0.30 and required tags +0.10. Scores are bounded, rounded and sorted by score then ID, with matching signals and exclusion reasons.

Thresholds default to 0.50 informational, 0.70 recommend and 0.85 strong relevance; they are configurable and validated. At most 20 eligible recommendations are delivered. No task keyword, embedding or LLM matching is used. Manual/historical Lessons remain repository-scoped. Fixture-family A/B/D score 0.80 for the baseline; C fails scope. See [retrieval](retrieval.md).

## 4. Validation semantics

Policy `distinct-application-v1` requires controlled support, no controlled contradiction, and a relevant successful **observed** application in a different repository tree. Tree/fingerprint keys deduplicate contexts. Same-tree retries, renamed tasks, identical clones, retrieval alone and opaque self-reports cannot raise confidence.

One distinct success yields `Validated` at 0.90; two yield 0.94. Controlled contradiction moves even a Validated Lesson to `Contradicted` at 0.20, preserving prior versions. Retirement is explicit and idempotent.

**Validated means supporting evidence exists in both a controlled counterfactual and at least one distinct application context. It does not imply universal correctness.** Confidence and relevance are heuristics, not calibrated probabilities.

## 5. CLI commands added

- `lesson search`, proposed actions, relevance thresholds, and candidate debugging.
- `run --with-experience`, `--no-experience`, `--retry-with-experience`, `--max-retries`.
- `lesson test`, `lesson retire`, and `lesson list --include-retired`.
- `why [--experience ID]` and `status`.

All support JSON. Advice is reported on stderr before execution; stdout remains one final JSON result. See [CLI reference](cli.md).

## 6. Representative CLI output

Excerpt from the executed documentation demo; UUIDs shortened here:

```text
Relevant experience (before execution)
  lesson-df14… · relevance 0.80 · confidence 0.78
Evaluation: Success · 1/1 required checks passed
Experience: exp-776f…
Relevant experience: lesson-df14… · Applied · relevance 0.80 · delivered true
Lesson: Validated · confidence 0.90
```

`why` exposed the successful B application `exp-776f1f26-2af9-481d-8d20-6e198ac2f15d`, Lesson `lesson-df14a36c-9405-499b-86b4-fc6f3347f577` revision 3 at use, supporting Experiment `experiment-10b86484-d829-4692-ba5e-c42c1aab65e9`, and original failure `exp-e1e00a3a-0695-40e6-a94f-212561fae2df`.

After A and both B runs, status reported 6 Experiences, 1 Experiment, 1 Lesson, 3 application records and 1 repeated mistake. One application record is the undelivered control audit. The documented setup/run/search/why/status commands executed successfully.

## 7. Deterministic retry results

A records original failure, failed baseline, successful alternative and successful retry: four immutable Experiences. The retry preserves the original state/task, records observed application and `retry_of`/`transfer_from`; trials record `counterfactual_of`. The Lesson stays supported at 0.78 because this retry is not a distinct tree.

An agent deliberately ignoring advice exhausts a two-retry budget without validation. Cancellation during retry records interruption and lineage, stops further attempts, and removes the worktree. Existing process-group cancellation and capture-failure retention checks still pass.

## 8. Distinct transfer results

| B condition | Success | Repeated mistake | Application |
| --- | --- | --- | --- |
| Experience disabled | 0/1 | 1/1 | Ignored, not delivered; audit-only |
| Experience enabled | 1/1 | 0/1 | Applied, observed |

B differs from A in package layout, versions, checks, output, repository tree and task. Its advised success validates at 0.90. Original A Experience and Experiment JSON remain unchanged. Repeating B, renaming the task or cloning identical contents stays at 0.90; a second distinct tree reaches 0.94. Concurrent applications preserve both evidence revisions without double-counting a tree.

This is one designed transfer comparison, not an estimated general Experience Transfer Rate.

## 9. Irrelevant-context results

C is an ordinary npm repository without the pnpm workspace marker or family tag. Search returns no match even with proposed action `npm install`. No Lesson is injected; the correct npm strategy passes.

## 10. Contradiction results

D has matching fixture-family context but requires legacy npm-compatible output. `lesson test` records baseline success and alternative failure, moves Validated to Contradicted at 0.20, and clears the current validation claim. Earlier support remains intact. The Lesson is not automatically retired. Explicit retirement records time/reason, excludes default listing/retrieval, and preserves all evidence.

## 11. Known limitations and real-agent result

- Scope and behavior are deliberately narrow, trusted fixture protocols. They do not establish general package-manager causality or secure action instrumentation.
- Different Git trees are a proxy for distinct context; an irrelevant file change can satisfy it. Environment fingerprints cover selected inputs, not external services or every tool.
- Opaque agents cannot be replayed automatically. Their usage reports remain self-reported and cannot validate a Lesson. Generic identity records the executable, not a guessed model/version.
- Git worktrees are not security sandboxes. Host/network/credentials remain reachable; logs may contain secrets. Redaction, quotas, artifact pruning, crash resumption and broad environment manifests are deferred.
- Only local macOS verification is reported. Linux/macOS CI is configured but was not run remotely here.

The requested real-agent smoke test succeeded with Codex CLI 0.149.1, configured model `gpt-5.6-sol`, using the generic adapter and `workspace-write` sandbox. Codex read the injected context, ran `./agent-script.sh alternative`, passed `./test.sh`, and wrote the actual Lesson ID/action to `usage.json`. Hardknock recorded `Applied / SelfReported`; confidence stayed at 0.90. No model override, credential copying, sandbox bypass or rules bypass was used. See [the agent contract and invocation](agent-integration.md).

## 12. Deviations and verification

The existing adapter interface is preserved; context injection wraps execution. `--no-experience` disables advice and learning but retains an undelivered audit match to measure repeated mistakes. Distinctness is stricter than a different task name or clone path. Broad cross-repository scope is limited to explicit version-2 fixture tags, and external-agent reports cannot qualify as observed validation.

Optional benchmark CLI, surprise metric and Candidate Reflex generation were deferred. The automated comparison supplies benchmark data; `status` supplies counts. No embeddings, hosted services, MCP/plugin server, chaos engine, command interception or policy enforcement was added.

Local checks:

- `cargo test --offline --all`: **50 passed** — 8 unit, 8 CLI, 15 learning, 7 substrate and 12 transfer tests.
- `cargo fmt --check`: passed.
- `cargo clippy --offline --all-targets --all-features -- -D warnings`: passed.
- Local Markdown links, code fences, mascot path and `git diff --check`: passed.
- The documentation demo ran successfully; all four source repositories stayed clean, with one source worktree each. SQLite integrity and foreign-key checks passed.
- Tests verify source/Experiment immutability, artifact hashes, v1/v3 migration, candidate exclusion, context collisions, concurrency, failed advice and cancellation.

## 13. Exact recommended next phase

**Active resilience building in trusted local fixtures:** validated explicit Skills → bounded local perturbations → failure-boundary discovery → evidence-backed operating envelopes → disabled advisory Reflex candidates → bounded Recovery trials.

Start with deterministic file/environment changes and matched controls. Record budgets, seeds, tested conditions and restoration checks; measure recovery and total experiment cost. Keep authorization separate from advice. Do not start with external API faults or automatic enforcement. The detailed sequence is in [the roadmap](roadmap.md#exact-next-phase-plan). This phase has not been started.
