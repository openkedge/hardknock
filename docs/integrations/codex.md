# Codex App Server

```bash
hardknock integrate codex check
hardknock --repo /path/to/project integrate codex run 'repair the failing test'
hardknock --repo /path/to/project integrate codex run --resume <thread-id> 'continue'
```

The adapter launches `codex app-server --listen stdio://`, sends `initialize`/`initialized`, starts or resumes a thread, supplies Hardknock evidence as a separate text item in the turn input, submits `turn/start`, and consumes structured events. It does not parse terminal prose and never uses `codex mcp-server`.

Fixtures are pinned to **codex-cli 0.149.1**. `check` reads the executable version, generates and verifies required local schema fields, then performs a real initialization handshake without a model call. Unknown versions fail in noninteractive mode unless `--allow-untested` is explicitly supplied; missing required schema fields still fail. This is a tested-version check, not a guarantee that every behavior in a future schema is compatible.

`commandExecution`, `fileChange` and `mcpToolCall` items are normalized. Unknown notifications and all reasoning events are ignored. Diff notifications are observed but their bodies are not persisted; the Bridge captures its bounded local Git diff. A multi-file change stays a batch observation rather than invented individual tool executions.

The adapter preserves configured developer/base instructions by adding evidence to turn input instead of replacing instruction fields. The actual task prompt is not sent to the Bridge or persisted by Hardknock. If Bridge calls fail, advisory mode logs a warning and lets Codex continue with its native permissions. Missing completion recording is reported as `unavailable`, not success. Timeouts/cancellation kill and reap the App Server process group, end the Bridge session when reachable, and record an incomplete outcome.

## Approvals and workspace ownership

Ordinary `item/started` notifications **cannot pause execution**. The capability matrix therefore reports broad pre-action interception as false. `item/commandExecution/requestApproval` and file-change approval requests can contribute Hardknock evidence while paused. The current noninteractive CLI does not collect user approval; it replies `cancel` when no user response is available. It does not change sandbox/approval configuration, accept on the user's behalf, or turn a lesson into denial. Unsupported server requests receive an error, never permission.

The existing `RealityProvider`/Git Dojo remain the experiment boundary. Native runs observe the workspace selected by the user; Hardknock does not duplicate Codex workspace/worktree provisioning or claim that it owns the external directory. A dedicated approval UI/callback and richer native pre-tool hooks are follow-up work.

| Security question | Implemented boundary |
| --- | --- |
| Can observe | App Server thread/turn IDs, structured command/file/MCP items, approval requests, message existence, diff notifications, turn completion |
| Cannot observe | All actions before execution, hidden tool internals, private reasoning, unreported remote side effects |
| Persists | Normalized action metadata/results, native version/model, evaluator evidence and local tracked diff |
| Redacts/omits | No reasoning events, full agent message, full prompt, arbitrary MCP arguments, provider diff body, or inherited environment dump |
| Influences | Experience context on the submitted turn; evidence at approval requests |
| Can block | Explicit policy can decline a matching approval request. Ordinary started notifications cannot block. Missing user approval cancels independently of experience. |

Verified locally: version/schema/handshake passed. A real model turn requested user approval for the fixture action; it was cancelled, and the latest run was recorded as **interrupted**. This verifies safe approval handling and outcome recording, not successful live task execution. The model-free bidirectional fixture and Codex-shaped to Claude-shaped transfer pass. Real successful cross-agent acceptance remains pending.

Sources: [official App Server documentation](https://learn.chatgpt.com/docs/app-server) and schemas generated locally by `codex app-server generate-json-schema`, checked 2026-08-27.
