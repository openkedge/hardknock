# Claude Code hooks

```bash
hardknock integrate claude install
hardknock integrate claude check
hardknock integrate claude uninstall
```

The installer atomically edits `~/.claude/settings.json`, managing only its exact hook command and preserving other settings/hooks. `--config /path/settings.json` supports an alternate settings file. An installation manifest under the Hardknock home records ownership; it does not edit repository instruction files. Restart Claude after installation.

Mapping: `SessionStart` registers/injects; `UserPromptSubmit` refreshes/injects context without sending the prompt; `PreToolUse` proposes; `PostToolUse` and `PostToolUseFailure` complete; `Stop` queues evaluation; `SessionEnd` ends registration. A `SessionStart` with source `compact` reinjects context. The handler also understands `PostCompact`, but the installer uses the broadly supported SessionStart surface.

Bash becomes a shell action; Read/Write/Edit retain paths, never file contents. Other tools retain their name with arguments omitted. Stable `tool_use_id` binds results to proposals. Only structured result fields are used, not terminal prose or transcript files.

Advice/warnings/replans use `hookSpecificOutput.additionalContext`. Explicit policy blocks use `permissionDecision: deny`; approval-required uses `ask`. The adapter never emits `allow`. When a configured evaluator fails within the bounded Stop wait, one continuation is requested. `stop_hook_active` prevents loops. Slow evaluations finish asynchronously without holding Claude's Stop indefinitely. Some versions lack a Stop ID; the adapter hashes the final message for idempotency and does not retain that message. Identical final messages in different turns are a known limitation of this fallback.

| Security question | Implemented boundary |
| --- | --- |
| Can observe | Registered hook metadata, tool IDs, Bash command/cwd, file paths, selected error/status fields, Stop and session end |
| Cannot observe | Private reasoning, unregistered hooks, commands hidden inside tools, remote side effects, full subprocess action trees |
| Persists | Normalized actions/results, evaluator evidence, tracked diff, provenance |
| Redacts/omits | Transcript paths are never read; full final messages, Write/Edit content, opaque arguments omitted; Bridge redacts common secrets |
| Influences | Startup/prompt context, pre-tool advice, user approval escalation, one verification continuation |
| Can block | Only explicit user policy denies tools; an evaluator failure can request bounded Stop continuation. Experience alone does not deny tools. Availability failures return an empty advisory response. |

Fixture payloads and full hook-to-Bridge tests pass. **Live Claude host acceptance is pending**; no Claude executable was available in this pass.

Source contract: [Claude hooks reference](https://code.claude.com/docs/en/hooks), checked 2026-08-27.
