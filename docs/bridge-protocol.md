# Hardknock Bridge v1

The public protocol is portable JSON, independent of internal Rust types and SQLite tables. Adapters use the Bridge exclusively. MCP is not a dependency.

## Transport and authentication

The daemon accepts one request and one response per connection, each terminated by a newline. Default endpoint: `$HARDKNOCK_HOME/run/hardknock.sock` (normally `~/.hardknock/run/hardknock.sock`). `hardknock bridge start --tcp 0` selects an available **127.0.0.1** port. IPv6 wildcard and external bind addresses are not supported.

Both transports require a fresh random 64-character runtime token from `run/bridge-token`. The home and runtime directories are private; token, endpoint description, and Unix socket are mode 0600. An exclusive file lock prevents duplicate daemons. Tokens rotate on restart. Tokens are not placed in command arguments, event logs, or error responses. Runtime symlinks and non-socket replacements are rejected. This authenticates the local user, not a particular agent: another process running as that user can read the token and impersonate an adapter. There is no remote or multi-user trust model.

```json
{
  "protocol_version": "hardknock.bridge.v1",
  "request_id": "client-generated-unique-id",
  "token": "read-locally-never-log",
  "payload": {
    "event": "session_started",
    "data": {
      "session_id": "native-session-id",
      "agent": {"name": "claude", "adapter_version": "0.3.0"},
      "cwd": "/absolute/project",
      "task": "short task summary",
      "environment": {"versions": {"pnpm": "observed-version"}}
    }
  }
}
```

Replies echo `protocol_version` and `request_id`, with `ok` and either `payload` or `error: {code, message}`. Unknown protocol versions, event tags, and unknown typed fields are rejected. Invalid unauthenticated requests cannot mutate state. Malformed JSON and overlarge/incomplete frames may close the connection without a response. Clients must treat a disconnect as a failed request, not a continue decision.

## Lifecycle

| Event | Data and reply |
| --- | --- |
| `session_started` | Native session ID, agent name/version/model/adapter version, absolute cwd, optional repository hint/task/environment. Returns `hardknock_session_id`, up to five `relevant_experience` briefs, and optional `context_document`. Repository and OS facts are captured locally, not trusted from hints. |
| `context_requested` | Hardknock session ID and optional task summary. Refreshes context and cached lessons. A new run can establish a fresh clean Git baseline; a mid-run request does not replace the baseline. |
| `action_proposed` | Session ID, stable action ID, `action`, and context booleans `can_intercept`, `no_state_change`, `config_changed`. Returns a decision. |
| `action_completed` | Same session, ID, and normalized action; result, duration in milliseconds. A completion without a proposal or a conflicting duplicate is rejected. |
| `agent_message` | Explicit bounded summary only. Never send a prompt, conversation, or reasoning trace. |
| `run_completed` | Session ID, stable per-turn `run_id`, optional claimed success, duration, and optional `termination` (`completed`, `interrupted`, `timed_out`; default `completed`). Full final messages and arbitrary external metadata are discarded. Returns a queued run record with a preallocated Experience ID. |
| `session_ended` | Ends registration; not evidence that the task succeeded. |
| `lesson_rejected` | Delivered lesson ID, reason enum, optional bounded detail. Multiple distinct session rejections or an environment-change rejection flag validated lessons for review; no automatic retirement. |
| `experiment_requested` | Session, lesson ID, bounded trial/run/duration request. **Reserved and explicitly rejected in this pass**. Use the existing controlled `experiment` CLI. Arbitrary agent-native experimentation is not enabled. |

Administrative payloads: `status`, `sessions`, `inspect` (session ID), `run_status` (session and run IDs), `events` (exclusive `after` cursor), `refresh_cache`, `shutdown`. All require authentication. `inspect` shows the most recent 50 action summaries, not conversations. `events` returns at most 100 telemetry rows; it is not the Experience repository.

## Actions and decisions

Actions use a `type` discriminator:

- `shell`: `command`, absolute `cwd`.
- `file_read`, `file_write`, `file_delete`: `path`, without file contents.
- `tool_call`: `tool`, portable JSON `arguments`.
- `network`: `method`, `target`.
- `custom`: `kind`, portable JSON `payload`.

An action result contains `success`, optional `exit_code`, `error_class`, `output_summary`, and `artifacts: [{uri, description}]`. Artifact URIs are metadata; the Bridge does **not** open adapter-supplied paths. Successful action status with a nonzero exit code is rejected. Action success is never task success.

Decisions use a `decision` discriminator: `continue`; `advise`/`warn` with `message` and evidence references; `replan`/`require_approval` with `reason` and evidence; `block` with `reason` and authority. Authorities are `experience`, `reflex`, `user_policy`, `external_policy`. This implementation only produces blocks from explicit local user policy.

Default mapping: eligible Lesson → advise; Supported Reflex → warn; Active Reflex → replan. Matching is exact whole shell command and matching workspace/scope. Compound shell commands, alternate spellings, commands inside opaque tools, and non-shell reflex matching are not inferred. A `replan` is advice; an adapter must not silently convert it into policy denial.

## Completion and durability

A background writer/evaluator handles completion separately from action matching. Graceful daemon shutdown cancels running checks, reaps their process groups, and flushes remaining telemetry. Checks are configured by canonical workspace path in local `config.toml`; a wire message cannot supply an executable check. Agent claims of success are stored separately from evaluator outcomes. No checks means `inconclusive`. Explicit interrupted/timed-out runs skip successful evaluation; a started observation with no completion remains incomplete. An intercepted proposal that was abandoned after advice is not itself a failed execution.

The Bridge captures a redacted, bounded tracked Git diff relative to the registration baseline; it does not attribute every changed byte to the agent. Untracked file contents and binary data are not included. External workspaces are recorded as `RealityStatus::Observed`, never owned or deleted by the Dojo. A synthetic `bridge-observation` execution links the existing Experience schema to the normalized action artifact; it does not claim that an aggregate shell command ran.

The observed Reality, execution, evaluation, immutable Experience, lesson applications, and successful run record commit in one SQLite transaction. Lifecycle acknowledgments enqueue telemetry; they are not synchronous fsync acknowledgments. Crashes may lose unflushed telemetry. On restart, committed runs are recovered; unfinished runs are reported as interrupted, not retried or declared successful. Partial artifact directories are retained on recording errors. There is no arbitrary command replay on restart.

Stable external session IDs are namespaced by agent; native action/run IDs are idempotency keys. New actions must have new IDs. A duplicate completed run returns its existing record. Reusing an action ID with a changed action/result is an error. Do not use a new ID to retry delivery of the same turn.

## Limits and privacy

Defaults: request/frame 1 MiB, context document at most 32 KiB, five lessons, output summary 8 KiB, 256 registered sessions, 2,048 actions per session, 128 runs per session, 32 simultaneous socket clients, bounded background queue. Configuration is validated. Session/action budgets currently require starting a fresh data home or deliberate maintenance when exhausted; no automatic retention policy is supplied.

No full prompts, outputs, conversations, reasoning, or inherited environment-variable dump are stored by native adapters. Short summaries, selected action results, normalized actions, checks, diffs, and provenance remain. Redaction recognizes common secret assignments, `*_TOKEN`, `*_KEY`, `*_SECRET`, password/authorization fields, bearer tokens and some known key formats. It is **not perfect secret detection**. Inline commands, filenames, arbitrary secrets, and encoded data can still be sensitive. Evaluator output exists temporarily in private scratch storage before sanitized artifacts are written; a host crash can leave scratch data. Git capture has a five-second per-command deadline and a 1 MiB read limit; evaluator scratch output still has no disk quota. Use disposable fixtures and do not run with production secrets.

See [the agent contract](agent-experience-contract.md) and [integration security tables](integrations.md).

Native agents maintain their own logs, account state and conversation retention under their own settings. Hardknock does not control that storage.
