# Scoped reflexes

A Reflex recognizes an evolving risky state before the next fixture operation. It contains a scoped TriggerPattern, exact proposed ActionPattern, response, heuristic confidence, source Lessons, source chaos Trial, evidence, and versioned lifecycle.

```text
Candidate → paired test → Supported → explicit enable → Active
                                                  disable → Disabled
```

The retry fixture proposes `repeated failures >= 3 AND no state change → Replan` after observed retry exhaustion. The drift fixture proposes `configuration differs from the committed plan → Replan`. Context includes repository, fixture-kind tag, markers, OS, and architecture. The matcher requires the exact next operation; a similar command name or unrelated fixture is insufficient. Candidate and disabled rules do not match during ordinary campaigns.

`DeterministicReflexMatcher::evaluate(ActionContext, &[Reflex])` returns matches with the exact rule version, observed precursor, source Lessons/Trial, response, and confidence. The runtime checks between fixture attempts (and before the first apply for drift). An active Replan executes the fixture's explicit alternative/re-read procedure and then evaluates the result. Advise/Warn are representable; generated rules use Replan. Block is representable for future governance but cannot be generated, enabled, or executed here.

```bash
hardknock reflex list
hardknock reflex show reflex-<uuid>
hardknock reflex test reflex-<uuid>
hardknock reflex enable reflex-<uuid>
hardknock reflex disable reflex-<uuid>
hardknock why
```

Testing uses two fresh Realities with the same source snapshot, environment policy, evaluation, and condition IDs. The without arm excludes Reflexes. The with arm locally tests only the selected rule, without enabling it for other runs. A matched replan changing FAIL to PASS/DEGRADED moves Candidate to Supported (0.58 → 0.84). One such pair does not establish transfer, universal safety, or calibrated probability. Source Lessons remain Candidate until their own Lesson evidence policy supports promotion. The test's match is explicitly marked `test_only`.

Enabled rules are snapshotted in subsequent fixture campaign plans. Generic agents are not intercepted. `why` follows the historical match → source Lesson → chaos Trial → campaign → Experience chain. Later disabling the rule does not rewrite why an earlier run replanned.

## False positives

```bash
hardknock reflex test reflex-<retry-uuid> --perturb command-failure:3
```

In this negative case, the first three operations fail transiently and the fourth original operation succeeds. The Reflex would replan before that successful fourth action. The paired test retains both traces and records a false positive, disables the rule, and lowers its heuristic confidence to 0.30. A later positive replay cannot erase this evidence or permit activation; a narrower candidate is needed. Automated narrowing is deferred, with `Narrows` reserved as an evidence relationship.

`FalsePositiveReflexRate = false-positive paired firings / paired tests with a firing`. A firing counts as false positive only when the without-arm trace demonstrates success of the next original action under the same deterministic prefix. This fixture protocol is not a general counterfactual estimator for opaque agents. Empty denominators are unknown (`null`). Contradictions and inconclusive tests are stored alongside support; nothing is hidden to improve the metric.

Retired is a modeled terminal state, but no retirement CLI is provided yet. Response/trigger editing, automatic scope refinement, arbitrary tool semantics, and remote policy engines remain deferred. The Bridge integration below adds a separate explicit local policy boundary.

## V0.3 Bridge action decisions

The Bridge loads eligible lessons and reflexes into `ExperienceHotCache`; action matching does not query SQLite. Exact command/cwd and context gates apply. Supported rules warn, active rules request replanning, and lessons advise. A local policy list may separately block an exact shell command or request native approval. An adapter must not convert learning evidence into governance.

Claude maps advice to hook context; Hermes/OpenClaw retain it for the next supported context injection. Codex can contribute evidence at approval requests but cannot pause ordinary item-start notifications. No arbitrary parameter rewriting, autonomous retries, LLM reflection, or shell semantic analysis occurs in this callback. Config/version changes require Bridge restart; lesson/reflex snapshots refresh at session/context requests, completion, and a two-second daemon poll.

The deterministic benchmark includes 1,000 cached lessons and 1,000 reflexes and measures the full in-process action handler. A dedicated release-profile CI job enforces P95 below 25 ms; ordinary debug test runs retain a broader smoke ceiling because wall-clock timing on shared runners is noisy. Actual measurements and their limits are in the [V0.3 report](implementation-v03.md); they are not an end-to-end native host latency claim.
