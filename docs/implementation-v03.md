# Hardknock V0.3 implementation pass

Date: 2026-08-27 (local). Version: `0.3.0-dev.1`.

**Status: integration preview, not a completed V0.3 live acceptance milestone.** The common Bridge, native adapters, deterministic transfer, and local checks are implemented. A successful two-agent live demonstration, Hermes/OpenClaw live loading, and the optional skill-validation demo remain outstanding. V0.4 has not begun.

## 1. Files created and changed

Created:

```text
docs/agent-experience-contract.md
docs/bridge-protocol.md
docs/implementation-v03.md
docs/integrations.md
docs/integrations/claude.md
docs/integrations/codex.md
docs/integrations/hermes.md
docs/integrations/openclaw.md
integrations/claude-code/fixtures.json
integrations/codex/fixtures/fake_app_server.py
integrations/codex/fixtures/lifecycle.jsonl
integrations/hermes/__init__.py
integrations/hermes/plugin.yaml
integrations/hermes/test_plugin.py
integrations/openclaw/bridge.mjs
integrations/openclaw/hooks.mjs
integrations/openclaw/hooks.test.mjs
integrations/openclaw/index.ts
integrations/openclaw/openclaw.plugin.json
integrations/openclaw/package.json
migrations/006_bridge.sql
src/bin/hardknock-test-adapter.rs
src/bridge/cache.rs
src/bridge/config.rs
src/bridge/engine.rs
src/bridge/mod.rs
src/bridge/privacy.rs
src/bridge/protocol.rs
src/bridge/recording.rs
src/bridge/transport.rs
src/cli/integrations.rs
src/integrations/claude.rs
src/integrations/codex.rs
src/integrations/install.rs
src/integrations/mod.rs
src/store/bridge.rs
tests/bridge.rs
tests/integrations.rs
```

Changed:

```text
.gitignore
CONTRIBUTING.md
Cargo.lock
Cargo.toml
README.md
docs/agent-integration.md
docs/architecture.md
docs/cli.md
docs/experience-model.md
docs/reflexes.md
docs/roadmap.md
src/application.rs
src/cli.rs
src/core.rs
src/experiment.rs
src/learning_loop.rs
src/lesson.rs
src/lib.rs
src/store.rs
src/store/experiences.rs
src/store/transfer.rs
tests/learning.rs
tests/substrate.rs
```

The existing modular Rust crate is retained. No global agent configuration was installed or changed during this pass; installer tests used temporary destinations. No commits or marketplace publication were made.

## 2. Bridge protocol design

`hardknock.bridge.v1` is a portable JSONL request/response contract with explicit DTOs, tagged actions/events/decisions, correlation IDs and schema validation. One request/reply per connection; default Unix socket, optional loopback TCP. Adapters do not query SQLite or internal repositories.

The daemon owns session registration, context, cached action evaluation, observation ingestion and asynchronous evaluated recording. A bounded worker queue separates the action path from writes/checks. New sessions/context requests, completions and a two-second poll refresh the lesson/reflex cache. The explicit experiment-request event exists but rejects execution in this pass.

The implementation reuses the existing Store, evaluator, immutable Experience model, Git Reality provider, controlled Experiment engine and transfer policy. See [the protocol](bridge-protocol.md).

## 3. Authentication model

Both Unix and TCP require a per-runtime random token. Token/endpoint/socket permissions are 0600, data/runtime directories are private, a file lock prevents duplicate daemons, and tokens rotate on restart. TCP binds only `127.0.0.1`; there is no unauthenticated write endpoint. Runtime symlinks/non-socket replacements, wrong tokens, unknown versions and oversized frames are rejected. Token values never appear in command arguments or telemetry.

This authenticates the local user. A process with the same user's access can impersonate an adapter; it is not a multi-user or tamper-proof trust boundary.

## 4. Normalized lifecycle schema

| Event | Meaning |
| --- | --- |
| SessionStarted | External session/agent identity, cwd, optional explicit task summary and nonsecret version labels; returns compact relevant experience/context |
| ContextRequested | Refresh context; establish a new clean baseline only between runs |
| ActionProposed | Correlated shell/file/tool/network/custom action plus observable precursor flags; returns continue/advise/warn/replan/require_approval/block |
| ActionCompleted | Matching action ID/action, selected result, duration; conflicting duplicates are rejected |
| AgentMessage | Explicit bounded summary; no private reasoning or conversation capture |
| RunCompleted | Stable turn ID, claimed success separately from evaluation, duration and termination; queues immutable evidence |
| SessionEnded | Ends registration without claiming task success |
| LessonRejected | Delivered lesson ID and structured disagreement; supports review flags |
| ExperimentRequested | Reserved bounded request, explicitly rejected; cannot execute arbitrary commands |

Run status, authenticated telemetry/inspection, cache refresh and shutdown are additional administrative messages. Native observations are `Observed` Realities, never owned/deleted worktrees. Interrupted/timed-out runs cannot become successful evaluations; abandoned intercepted proposals are not executions. Exact action-time advice references are retained instead of attributing unrelated retrieved lessons.

## 5. Claude Code status

Implemented managed installer/uninstaller and `integration-event --agent claude`: SessionStart/UserPromptSubmit context, PreToolUse normalization and decisions, PostToolUse/failure observation, Stop evaluation, SessionEnd, and compaction reinjection. Only independent policy can deny a tool or request approval. The adapter never emits permission `allow`.

Stop permits at most one verification continuation and bounds its post-submission polling window to 2.5 seconds. Full messages/transcripts are not read or retained. Hooks and installer preservation are tested with fixtures and a real local Bridge. **A Claude executable was unavailable; live acceptance is not verified.** [Guide](integrations/claude.md).

## 6. Codex App Server status

A dedicated bidirectional stdio client initializes, checks local version/schema, starts/resumes threads, submits tasks and consumes structured command/file/MCP/message/completion events. Fixtures target **codex-cli 0.149.1**. Unknown versions require `--allow-untested`; required schema fields still must exist. No `codex mcp-server` integration was added.

Experience is supplied as a separate turn text item without overriding configured developer/base instructions or native sandbox/approval settings. The actual prompt is not sent to the Bridge. Native approvals remain user approvals; the noninteractive runner cancels when no approval response exists. Timeout/cancellation kills and reaps the App Server process group, closes registration and records incomplete work when the Bridge is reachable. Advisory outages allow Codex to continue but cannot claim successful recording.

The installed executable passed real schema/initialization checks. Live model attempts reached native approval; no approval was granted. The latest observed run was **interrupted**, not successful. An earlier attempt recorded evaluator failure. These test safe cancellation and recording, not successful task execution or two-agent transfer. [Guide](integrations/codex.md).

## 7. Hermes status

Source plugin with native hook registration, context injection, correlated terminal/file observations, run/session completion, bounded local RPC, explicit human approval escalation and independent required-policy availability mode. Install/uninstall preserves unmanaged files. Python mock-host tests cover lifecycle, malformed input, timeout, privacy and policy separation.

**Live host loading is unverified.** Native session/tool IDs and local workspace paths are required; missing visibility is skipped rather than invented. Optional `hardknock_validate_skill` and the rolling-deployment demonstration were not implemented. [Guide](integrations/hermes.md).

## 8. OpenClaw status

Source plugin registers typed `api.on` hooks for prompt, agent run, pre/post tool, agent end and session end. Input gates use the native `outcome` shape; tool gates use native block/approval results. Code-mode exec is not mislabeled as shell. Learning advice queues for the next prompt; it does not rewrite tool arguments or deny tools. Four Node mock-host tests cover the decision variants, failure paths and privacy.

**Live loading and SDK typecheck remain unverified** because no OpenClaw installation was available. Host trust/enablement remains a user decision; copying files is not reported as verified host loading. [Guide](integrations/openclaw.md).

## 9. Capability matrix

These are implemented adapter surfaces, not claims of live-host certification.

| Adapter | Session | Context | Pre-action interception | Post-action | Run end | Structured | Native Reality |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | Yes | Yes | PreToolUse | Yes | Stop | Yes | No |
| Codex | Yes | Yes | **Approval requests only** | Yes | Turn completion | Yes | No |
| Hermes | Yes | Yes | Correlated pre_tool_call | Yes | Session/run hook | Yes | No |
| OpenClaw | Yes | Yes | Typed before_tool_call | Yes | agent_end | Yes | No |

`agent capabilities` reports Codex's broad interception boolean **false**. Item-start notification is observation, not a point at which execution can be paused. `integrate doctor` checks managed files, configuration, reachability and Codex schema/initialization. “Connected” means an unended registration, not a heartbeat. Missing native IDs, hidden tool internals and remote side effects are not fabricated.

## 10. Transfer and regression results

The mandatory deterministic cross-agent test passes:

1. A Codex-shaped structured action executes the fixture baseline and fails the configured evaluator.
2. An explicit fixture hypothesis creates a scoped candidate; this is not represented as live model reflection.
3. Existing ExperimentEngine reconstructs clean paired Realities: baseline failure, alternative success. The source remains immutable.
4. A Claude-shaped adapter in the distinct service/worker fixture receives the lesson and an advisory on the bad action, chooses the alternative, and passes evaluation.
5. Recorded provenance contains Codex discovery and Claude successful transfer; the lesson becomes Validated. Controlled trials retain their actual scripted identity.
6. Structured rejection flags revalidation without immediate retirement. Cross-agent identity alone never adds an independence claim.

Final local quality gates:

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --all` | **90 passed, 0 failed, 2 optional real-agent tests ignored** |
| `git diff --check` | Passed |
| Optional real Codex schema/handshake and model lifecycle tests | 2 passed; model work was cancelled at native approval and recorded interrupted |

The default count includes 8 unit, 14 Bridge, 8 CLI, 9 integration, 15 learning, 17 resilience, 7 substrate and 12 transfer tests. The integration suite also runs 3 Python and 4 Node plugin tests. Targeted coverage verifies authenticated Unix/TCP, malformed/version/oversized frames, stalled-peer deadlines, bounded context JSON, redaction, duplicate IDs, restart, policy separation, evaluator shutdown/reaping, Codex timeout cleanup, and fixture transfer.

The default suite uses no external models or network services; transport tests use local sockets. Nested plugin tests require Python 3.11+ and Node.js 20+. Verification was local on macOS, not a claim that remote CI ran. Optional real Codex tests use the configured local account and do not bypass permissions.

## 11. Pre-action latency

Measured using 200 warm in-process action requests in the debug build:

| Workload | P95 observed |
| --- | --- |
| Action handler while a background evaluator runs | 85 µs |
| 1,000 matching lessons plus 1,000 out-of-scope reflexes, including ranking and enqueue | 8,026 µs (8.026 ms) |

Both pass the 25 ms target. The large workload deliberately exercises a full lesson ranking, not an empty-cache shortcut. These are local handler timings, not native host startup/IPC/LLM latency guarantees. Results vary by hardware and load. Run `cargo test --test bridge -- --nocapture` to reproduce.

## 12. Privacy and redaction

Native prompts, full model/tool outputs, file contents, conversations, transcript files and reasoning events are omitted by default. Explicit summaries, normalized actions/status, selected outputs, checks, tracked diffs and provenance remain. Common secret assignments (including quoted values), key/token/secret/password fields, bearer/authorization text and known key formats are redacted before persistent native artifacts are written. Adapter-supplied artifact paths are never opened.

Limits include 1 MiB frames, 32 KiB context JSON including escaping/briefs, five lessons and 8 KiB output summaries. Git capture has a five-second per-helper deadline and 1 MiB read bound; persisted diff is capped at 32 KiB. Redacted/truncated commands cannot be used as faithful native counterfactual reconstructions.

This is not perfect secret detection. Filenames, inline commands and unknown/encoded secrets may remain sensitive. Evaluator scratch output is private but has no disk quota; a crash can leave it. Existing generic-run logs are not retroactively redacted. Native agents retain their own state under their own settings.

## 13. Graceful degradation

Ordinary native advisory failures log a bounded diagnostic and return no policy denial. Codex preserves native permission behavior even without a Bridge and reports unavailable recording explicitly. Hermes/OpenClaw pre-tool calls have 20 ms RPC budgets; slow learning does not run there. Their separately configured required-policy mode can block missing-policy availability. Claude malformed hook input produces an empty successful advisory response rather than an accidental denial exit code.

Daemon shutdown cancels/reaps checks and flushes telemetry. Run/action IDs prevent duplicate evidence, and committed runs recover on restart. Unfinished recording is marked interrupted; it is not silently replayed. Queue acknowledgments are not fsync guarantees, and recording failures remain inspectable.

## 14. Known limitations

- Successful live Claude+Codex transfer and real Hermes/OpenClaw loading are outstanding. The optional live test currently proves approval cancellation, not successful task completion.
- Codex cannot intercept every tool, and this CLI has no interactive approval callback. Ordinary advisory evidence cannot authorize native execution.
- Native checks/diffs observe a live external workspace. Concurrent edits/turns are not exclusively attributable; serialize activity when collecting meaningful transfer evidence. Dirty/unversioned starts cannot establish observed transfer or controlled reconstruction.
- Session/action/run history has finite budgets and no retention/eviction command. Acknowledged unflushed telemetry may be lost on a crash. Partial artifacts are retained for inspection rather than auto-replayed.
- Shell matching is exact, not semantic. Dynamic no-state-change/config-change precursors default false unless explicitly observable; adapters do not guess them. Cache updates may lag by two seconds.
- Claude versions without a Stop ID use a message digest; identical final messages on different turns can collide. No transcript is read to disambiguate them.
- Plugin host version pinning, broader lifecycle conformance, installer concurrency/crash recovery and automatic version-change revalidation need more work. Current revalidation uses explicit environment-change feedback or repeated session rejection.
- The shared budget caps additional trials/runs, not tokens or money. Optional duration-based shared budgets are explicitly rejected by the runner; per-execution timeouts remain enforced.
- Global `bridge.failure_mode` and `[reflex].default_response` settings are not implemented; unsupported configuration is rejected. Required-policy availability is currently explicit in the Hermes/OpenClaw adapters only.

## 15. Deviations and rationale

The suggested workspace split was not required: the established single crate was retained with modules and thin boundary packages, avoiding an unrelated migration. Native workspaces are observed, not claimed as Dojo-owned sandboxes. Native reconstruction is explicitly distinguished from replay.

The experiment-request DTO is reserved but does not execute. Arbitrary agent-requested trials, hypothesis/recovery tools, skill-validation tools and optional MCP were deferred to keep the native integration pass bounded and honor the instruction not to begin V0.4 before reliable Claude/Codex acceptance. This leaves the requested on-demand experiment lifecycle and Hermes skill demonstration incomplete, rather than represented by a placeholder success.

A full live demonstration was not substituted with fixtures: the deterministic fake-adapter transfer is labeled as such, and missing hosts/native approval are reported directly. Diagnostics use JSON even without `--json`; native approval collection and richer human display remain follow-up work. No deferred hosted, multi-user, cloud, marketplace, virtualization, tournament or GUI features were added.

## 16. Recommended next implementation phase

First finish V0.3 acceptance: run user-authorized disposable Claude and Codex tasks through this Bridge, demonstrate evaluated transfer on distinct repositories, load both plugins in real hosts, and add a native approval callback without changing permission policy. Harden sustained session retention/crash behavior and environment revalidation before release.

Only then start **V0.4 — Agent-Native Experimentation** with explicit requested trials, shared budgets/deadlines, structured hypotheses, skill validation and evidence-returning tools. MCP, if added, should remain an optional facade over the same contract.
