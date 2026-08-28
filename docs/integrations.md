# Agent integrations (V0.3 preview)

V0.4 adds the shared [experiment request/progress/result helper](agent-experiments.md#during-an-integrated-session) to native context. Fake Claude/Codex sessions exercise that contract through authenticated Unix JSONL; this does not change the live-host acceptance status below or imply live model experimentation has been verified.

**Models change. Experience survives.**

The common local Bridge is implemented. Four native adapters have deterministic fixture coverage; successful live demonstrations with two different agents are **not yet complete**. The installed Codex 0.149.1 passed schema detection/initialization, and a live task exercised native approval cancellation and recorded a non-success outcome. Claude, Hermes and OpenClaw were unavailable for live host testing in this pass. See [the implementation report](implementation-v03.md).

```text
Claude Code ─┐
Codex ───────┤
Hermes ──────┼── Agent Adapters ──→ Hardknock Bridge
OpenClaw ────┘                          │
                         ┌─────────────┼─────────────┐
                         ↓             ↓             ↓
                     Experience      Reflex         Dojo
                         └─────────────┼─────────────┘
                                       ↓
                                    Evidence
```

## Quick start

```bash
cargo build --locked
hardknock bridge start
hardknock bridge status
hardknock integrate claude install
hardknock integrate codex check
hardknock agent capabilities
hardknock integrate doctor
hardknock events tail --follow
hardknock bridge sessions
hardknock bridge inspect hk-s-<id>
hardknock bridge stop
```

For foreground diagnostics use `hardknock bridge start --foreground`. Use `--home`/`HARDKNOCK_HOME` for a dedicated data directory outside the workspace. Starting an adapter session can autostart the daemon; pre-tool hooks never spawn it. The CLI currently returns structured JSON for integration diagnostics even without `--json`. `doctor` verifies configuration, managed files, Bridge reachability, and Codex version/schema/initialization when installed; native plugin enablement is reported as unverified.

## Capability matrix

| Adapter | Context | Pre-action | Post-action | Run end | Structured events | Native Reality provider |
| --- | --- | --- | --- | --- | --- | --- |
| [Claude](integrations/claude.md) | SessionStart / prompt hook | PreToolUse | PostToolUse / failure hook | Stop | Yes | No |
| [Codex](integrations/codex.md) | Turn text context | **Approval requests only** | Item completion | Turn completion | Yes | No |
| [Hermes](integrations/hermes.md) | pre_llm_call | pre_tool_call | post_tool_call | on_session_end | Yes | No |
| [OpenClaw](integrations/openclaw.md) | before_prompt_build | before_tool_call | after_tool_call | agent_end | Yes | No |

Codex's broad `pre_action_interception` capability is reported **false**; a notification that execution has started is not an interception surface. Hermes/OpenClaw can synchronously match a proposal, but ordinary learning advice is retained for the next supported context injection rather than converted to a denial. Every adapter can observe only what its host exposes. Internal subprocess actions, remote side effects, and tools without correlated IDs are not automatically visible.

## Configuration

`~/.hardknock/config.toml`:

```toml
[bridge]
autostart = true
timeout_ms = 200
max_context_lessons = 5
max_context_bytes = 32768
evaluator_timeout_secs = 30
max_verification_retries = 1

[bridge.evaluators]
"/absolute/canonical/project" = ["./test.sh"]

[integrations.claude]
enabled = true
max_context_lessons = 5

[integrations.codex]
enabled = true
mode = "app-server"
```

Evaluators are user-configured executable shell commands, run with the local user's authority in the observed workspace. They can change files; review them like build scripts. Restart the Bridge after configuration changes. Lessons/reflexes refresh at session/context requests, completion and a two-second daemon poll. No reflection or model inference runs on the action path.

Optional governance is configured independently:

```toml
[bridge.policy]
blocked_shell_commands = ["an exact forbidden command"]
approval_shell_commands = ["an exact approval-gated command"]
```

This is an exact-command integration gate, not a shell parser or sandbox. It cannot enforce policy in opaque commands or in Codex events that cannot be paused. Native host sandboxing and permissions remain essential. Missing Bridge advice normally fails open. Hermes/OpenClaw offer separately configured availability enforcement; see their guides.

## Compatibility and conformance

Default tests need Rust, Git, a C compiler, Python 3.11+ and Node.js. They call no external models and use no network other than loopback sockets. They cover the shared lifecycle, malformed messages, duplicate IDs, authentication, redaction, timeout/degradation, installer preservation, native fixtures, controlled transfer, and cache latency.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test --test bridge -- --nocapture

# Optional installed Codex; second command can use the configured account/model.
cargo test --test integrations real_codex_app_server_handshake -- --ignored --nocapture
cargo test --test integrations real_codex_model_lifecycle_smoke -- --ignored --nocapture
```

`hardknock-test-adapter` reads lifecycle JSONL from stdin, supplies local authentication and prints Bridge replies. It is a conformance tool, not a reasoning agent. `hardknock bridge call` sends one payload. Neither should receive raw tokens on the command line.

Existing `run --agent test-agent`, `run --script`, and `run --agent-command` remain available without plugins. `run --experience-budget N` bounds additional controlled trials plus retries; the initial task execution is outside this additional budget. Native Codex uses `integrate codex run`, not `codex mcp-server`. No MCP compatibility layer is included yet.
