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
| `experiment_requested` | Explicit structured alternatives, checks, request ID, budget and capabilities. Returns `experiment_accepted` or `experiment_rejected` with an experiment ID and effective budget. Execution runs asynchronously outside the action/learning queues. |
| `experiment_progress` | Session ID, experiment ID and exclusive `after` cursor. Returns progress rows, partial candidate summaries, status and a compact final result when available. |
| `experiment_cancelled` | Session and experiment ID: requests cancellation. The acknowledgment is not cleanup completion; poll progress for terminal `experiment_cancelled`. |

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

A background writer/evaluator handles completion separately from action matching. Graceful daemon shutdown cancels running checks and experiment candidates, reaps ordinary process groups, and flushes remaining telemetry. Ordinary `run_completed` checks are configured by canonical workspace path in local `config.toml`; that lifecycle message cannot override them. V0.4's separately permitted, explicit experiment requests supply their own checks, which run only in disposable Realities. Agent claims of success are stored separately from evaluator outcomes. No checks means `inconclusive`. Explicit interrupted/timed-out runs skip successful evaluation; a started observation with no completion remains incomplete. An intercepted proposal that was abandoned after advice is not itself a failed execution.

The Bridge captures a redacted, bounded tracked Git diff relative to the registration baseline; it does not attribute every changed byte to the agent. Untracked file contents and binary data are not included. External workspaces are recorded as `RealityStatus::Observed`, never owned or deleted by the Dojo. A synthetic `bridge-observation` execution links the existing Experience schema to the normalized action artifact; it does not claim that an aggregate shell command ran.

The observed Reality, execution, evaluation, immutable Experience, lesson applications, and successful run record commit in one SQLite transaction. Lifecycle acknowledgments enqueue telemetry; they are not synchronous fsync acknowledgments. Crashes may lose unflushed telemetry. On restart, committed runs are recovered; unfinished runs are reported as interrupted, not retried or declared successful. Partial artifact directories are retained on recording errors. There is no arbitrary command replay on restart.

Stable external session IDs are namespaced by agent; native action/run IDs are idempotency keys. New actions must have new IDs. A duplicate completed run returns its existing record. Reusing an action ID with a changed action/result is an error. Do not use a new ID to retry delivery of the same turn.

## Limits and privacy

Defaults: request/frame 1 MiB, context document at most 32 KiB, five lessons, output summary 8 KiB, 256 registered sessions, 2,048 actions per session, 128 runs per session, 32 simultaneous socket clients, bounded background queue. Configuration is validated. Session/action budgets currently require starting a fresh data home or deliberate maintenance when exhausted; no automatic retention policy is supplied.

No full prompts, outputs, conversations, reasoning, or inherited environment-variable dump are stored by native adapters. Short summaries, selected action results, normalized actions, checks, diffs, and provenance remain. Redaction recognizes common secret assignments, `*_TOKEN`, `*_KEY`, `*_SECRET`, password/authorization fields, bearer tokens and some known key formats. It is **not perfect secret detection**. Inline commands, filenames, arbitrary secrets, and encoded data can still be sensitive. Evaluator output exists temporarily in private scratch storage before sanitized artifacts are written; a host crash can leave scratch data. Git capture has a five-second per-command deadline and a 1 MiB read limit; evaluator scratch output still has no disk quota. Use disposable fixtures and do not run with production secrets.

See [the agent contract](agent-experience-contract.md) and [integration security tables](integrations.md).

Native agents maintain their own logs, account state and conversation retention under their own settings. Hardknock does not control that storage.

## V0.4 experiment contract

The protocol identifier remains `hardknock.bridge.v1`; the **previously reserved and nonexecuting** lesson-ID request is replaced with this structured contract. Old reserved payloads are rejected as malformed, not interpreted as permission to run arbitrary trials. Other lifecycle events remain compatible. No MCP transport has been added.

Example payload, inside the normal authenticated envelope:

```json
{
  "event": "experiment_requested",
  "data": {
    "hardknock_session_id": "hk-s-from-session-start",
    "request_id": "request-00000000-0000-4000-8000-000000000001",
    "question": "Which upgrade preserves compatibility?",
    "candidates": [
      {
        "id": "candidate-00000000-0000-4000-8000-000000000002",
        "name": "direct",
        "execution": {"kind": "shell", "commands": ["./agent-script.sh direct-upgrade"]}
      },
      {
        "id": "candidate-00000000-0000-4000-8000-000000000003",
        "name": "staged",
        "execution": {"kind": "shell", "commands": ["./agent-script.sh staged-upgrade"]}
      }
    ],
    "evaluator": {"checks": ["./test.sh"]},
    "budget": {"max_realities": 2, "max_agent_runs": 2, "max_duration_ms": 60000},
    "criteria": {"require_success": true},
    "intent": "compare_strategies",
    "capabilities": {"allow_network": false, "allow_external_mutations": false}
  }
}
```

`AgentTask` uses `{"kind":"agent_task","prompt":"...","agent":{"kind":"test-agent","executable":"ignored-on-wire","version":null,"model":null}}`. The executable is resolved from trusted local configuration or a built-in, never selected by an arbitrary wire path. Omitting `agent` selects the requesting session's agent. The Bridge supplies the origin, requester identity and recorded-commit fallback; clients do not select another workspace or claim a live process snapshot.

`request_id` is stable across delivery retries. Identical requests return the original experiment; conflicting reuse fails. Accepted requests are persisted before queue admission, so the ID is inspectable. New candidate executions are not replayed on daemon restart. The action handler stays independent of the bounded experiment worker. Per-session reservations prevent repeated requests from bypassing Reality/agent-run ceilings. See [budget limits](experience-budget.md).

Progress query:

```json
{"event":"experiment_progress","data":{"hardknock_session_id":"hk-s-from-session-start","experiment_id":"experiment-00000000-0000-4000-8000-000000000004","after":0}}
```

The reply payload contains `event`, `experiment_id`, `status`, `progress: [[sequence, progress], ...]`, `completed_candidates`, `result`, and optional `reason`. Progress phases are `preparing`, `executing`, `evaluating`, `comparing`, `learning`, `completed`, and `cancelled`. Each row has a timestamp and optional candidate ID. Advance `after` to the largest sequence consumed; at most 128 rows are returned. Polling is the streaming mechanism on this one-request-per-connection transport.

Terminal events are `experiment_completed`, `experiment_cancelled`, or `experiment_rejected` (including infrastructure failure, distinguished by `status: failed`). Results contain candidate execution/evaluation outcomes, check statuses, diff statistics, starting fingerprints, quality, comparison reasons, recommendation, generated Experience/Lesson IDs, and usage. They omit raw output, raw commands, and candidate task prompts. Full trusted local artifacts remain available through CLI inspection.

The same bounded context document delivered to Claude and Codex now describes `hardknock try --session`. It is a suggestion to request deliberate experience, not an instruction to initiate automatically. Session end cancels pending/running agent requests unless configured otherwise. Network isolation is advisory; external mutation declarations are rejected. Obvious external-effect commands and exact locally blocked/approval-required commands are rejected, but this is not a shell security parser.

**Experiment privacy differs from opaque lifecycle observation:** candidate prompts, commands, evaluator specifications and resulting diffs are explicit operational inputs persisted for replay. Do not submit secrets or entire conversations. The ordinary adapter rule against collecting hidden reasoning/full transcripts remains unchanged.

## V0.5 curriculum lifecycle

Agent requests require `[curriculum] agent_requests=true`; the default is false. Curricula use the same authenticated transport and shared bounded experiment queue. No MCP endpoint is implemented.

```json
{"event":"curriculum_requested","data":{"hardknock_session_id":"<session>","request_id":"curriculum-00000000-0000-4000-8000-000000000005","target":{"skill":"process-task-successfully"},"profile":"resilience-basic","budget":{"max_trials":2}}}
```

`target` accepts exactly one `skill` or `task_family` name/ID. The reply is `curriculum_planned`, including ID, selected-trial count, budget, bounded gap decisions and `requires_start: true`. Matching request IDs are idempotent; conflicting reuse and foreign sessions are rejected. Agent requests are limited to verified bundled hardening procedures/evaluators in the requesting repository. All planned, cancelled and completed curriculum reservations count toward `max_agent_session_trials`; they do not bypass this limit by submitting another request ID.

```json
{"event":"curriculum_started","data":{"hardknock_session_id":"<session>","curriculum_id":"curriculum-00000000-0000-4000-8000-000000000005"}}
{"event":"curriculum_progress","data":{"hardknock_session_id":"<session>","curriculum_id":"curriculum-00000000-0000-4000-8000-000000000005","after":0}}
{"event":"curriculum_cancelled","data":{"hardknock_session_id":"<session>","curriculum_id":"curriculum-00000000-0000-4000-8000-000000000005"}}
{"event":"skill_package_requested","data":{"hardknock_session_id":"<session>","skill":"process-task-successfully","profile":"resilience-basic"}}
```

Start only queues work; it does not report completion. Poll replies include `curriculum_progress` or `curriculum_completed`, with authoritative status (`planned`, `running`, `completed`, `partially_completed`, `cancelled`). Progress is `[sequence,event]`, at most 16 rows per reply. Advance `after` to consume more; local `show`/`report` retains full evidence. Events include planned, started, trial started/completed, evidence gap observed, and maturity changed. A gap can close with known failure; that does not mean the condition is safe.

Results report trial outcomes, generated IDs, budget, reservations/recorded usage, profile coverage and policy-derived maturity. Summary lists are capped; raw scripts, model prompts and outputs are omitted. Package replies include bounded condition observations and item IDs; the local package retains full versioned provenance. Session end and shutdown cancel pending/running curricula regardless of the strategy-only `continue_after_session_end` setting. There is no continuous autonomous scheduler.
