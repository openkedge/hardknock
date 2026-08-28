# Hermes plugin

```bash
hardknock integrate hermes install
hardknock integrate hermes check
hardknock integrate hermes uninstall
```

Installs an owned `plugin.yaml` and `__init__.py` under `~/.hermes/plugins/hardknock`. `--config /path/to/plugin-directory` changes the destination. It refuses to overwrite an unmanaged directory; uninstall removes only managed filenames, retaining user additions. Hermes's plugin trust/enable controls still apply. No core patches or Gateway-only hooks are used.

`on_session_start` registers context; `pre_llm_call` refreshes/injects it; `pre_tool_call` evaluates with a 20 ms socket budget; `post_tool_call` records results; `on_session_end` records a run (Hermes emits it per conversation call); `on_session_finalize` ends registration. Native `session_id` and `tool_call_id` are required for reliable pre/post correlation. Missing IDs are skipped, not invented. CLI cwd is used unless an explicit cwd is available; remote/Gateway working directories need host-provided paths.

Hermes pre-tool callbacks cannot inject arbitrary advisory context directly. Learning advice is queued for `pre_llm_call`, never used to hard-block. Explicit policy returns canonical `{"action":"block","message":"..."}`. Policy approval requests return `action: approve`, which **requests human approval** in Hermes rather than granting permission. `HARDKNOCK_POLICY_REQUIRED=1` independently opts into blocking when the Bridge is unavailable. Default experience-only hooks catch errors/timeouts and return no directive.

| Security question | Implemented boundary |
| --- | --- |
| Can observe | Registered CLI/Gateway plugin callbacks, correlated tool metadata, terminal command/cwd, selected result status, run completion |
| Cannot observe | Hidden reasoning, remote effects, internal shell actions, tools lacking IDs, unexposed Gateway workspace state |
| Persists | Normalized actions/status, evaluator evidence/diff, session identity/model, context provenance |
| Redacts/omits | User messages/history ignored; full terminal output, file contents and opaque arguments omitted; Bridge secret redaction applies |
| Influences | LLM-turn context, pre-tool policy escalation/denial, future-turn learning advice |
| Can block | Explicit policy only, or separately enabled required-policy availability enforcement; lessons/reflexes do not block |

Mock host lifecycle, malformed/timeout behavior, privacy and policy tests pass. **Live Hermes plugin loading is unverified** in this environment. The optional `hardknock_validate_skill` tool and rolling-deployment skill-validation demonstration are not included; existing local chaos/skill commands remain available. Skills are never modified by this plugin.

Sources: [Hermes plugin API](https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins), [canonical hook catalog](https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks), checked 2026-08-27.
