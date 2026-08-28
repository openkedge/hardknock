# OpenClaw typed plugin

```bash
hardknock integrate openclaw install
hardknock integrate openclaw check
hardknock integrate openclaw uninstall
```

The installer places the manifest, package entry, TypeScript registration and local JavaScript transport under `~/.openclaw/extensions/hardknock`. `--config /path/to/plugin-directory` overrides the destination. **Enable/allow the plugin through OpenClaw's own trust configuration**; the installer does not broaden that trust automatically or claim that copying files means the host loaded them. Configure the plugin's `home` when using a nondefault Hardknock home.

Registration uses typed `api.on` hooks, not legacy `registerHook`: `before_prompt_build`, `before_agent_run`, `before_tool_call`, `after_tool_call`, `agent_end`, `session_end`. No `llm_input`/`llm_output` capture is installed.

The plugin caches per-session context, refreshes at prompt build and uses a 20 ms pre-tool RPC deadline. It distinguishes shell exec from code-mode exec. Native tool IDs bind results; missing IDs are skipped. Learning warnings/replans are logged and queued for the next prompt; the plugin does not rewrite tool arguments or disguise advice as policy. Explicit policy can return `block: true` or native `requireApproval`.

Experience failures are caught inside hooks so OpenClaw's own hook timeout/fail-closed semantics do not turn missing advice into accidental denial. Plugin setting `policyRequired: true` is a separate explicit choice to block on missing policy service. The `autostart` setting controls plugin startup attempts; set false for tests or managed daemon startup. As with any local filesystem I/O, the socket deadline is not a hard operating-system scheduling guarantee.

| Security question | Implemented boundary |
| --- | --- |
| Can observe | Typed prompt/tool/run/session hooks, correlated tool IDs, command/cwd or file paths, result error state, duration |
| Cannot observe | Hidden reasoning, every nested tool effect, arbitrary remote action state, actions without host IDs |
| Persists | Normalized tool observations, run claims separately from evaluator outcomes, checks and tracked diff |
| Redacts/omits | No prompt/messages/LLM output capture, full tool response, opaque arguments or file content; Bridge redacts common secrets |
| Influences | Prompt context, future-prompt learning advice, native policy approvals or denial |
| Can block | Explicit independent policy or `policyRequired` availability enforcement; learning evidence does not block |

Mock typed-host callbacks, failure behavior, privacy, and code-mode discrimination pass. **Live OpenClaw host loading and SDK typecheck are pending** because OpenClaw was not installed. This is a source plugin with tested callback behavior, not a published/host-certified plugin.

Sources: [OpenClaw hooks](https://docs.openclaw.ai/plugins/hooks), [public hook types](https://github.com/openclaw/openclaw/blob/main/src/plugins/hook-types.ts), checked 2026-08-27.
