# Experience budgets

`ExperienceBudget` is a shared core type, re-exported by the Bridge. The old JSON name `max_trials` remains an input alias for `max_realities`; new output uses the latter. The old reserved V0.3 experiment request itself has been replaced with the structured V0.4 request.

| Ceiling | Default | Enforcement |
| --- | --- | --- |
| `max_realities` | 3 | Entire candidate set checked before creating worktrees |
| `max_agent_runs` | 3 | AgentTask candidates counted, including failed/cancelled launches |
| `max_duration_ms` | 300000 | Shared cancellation deadline across preparation, candidates, and evaluation; teardown grace |
| `max_commands_per_reality` | absent | Explicit shell entries **plus all evaluator commands**; rejected for opaque AgentTask runs |

The command ceiling counts scheduled command entries, not instructions or subprocesses inside a shell script. Native internal tool calls cannot be counted reliably. Their count is `null` (unknown), not zero. Duration, allocated Realities, agent runs and observed top-level command processes are recorded separately. Financial, token, CPU, memory and output-disk accounting are not implemented.

```toml
[experience_budget]
max_realities = 3
max_agent_runs = 3
max_duration_seconds = 300
# max_commands_per_reality = 8  # Shell-only experiments

[experiments]
max_parallel_realities = 3
provider_capacity = 8
continue_after_session_end = false

[experiments.agent_requests]
enabled = true
max_realities = 3
max_parallel = 2
allow_network = false
auto_request = false
```

The request's caps are clamped to local configuration and persisted as `effective_budget`. **The candidate list is never silently reduced.** `StrictBudgetPolicy` approves the entire comparison or rejects it, with a reason. Five candidates and a two-Reality budget produce rejection and zero worktrees. `BudgetDecision::Reduced` is reserved for a future policy; the implemented strict policy does not use it. Expensive interactive approval prompts are not implemented; change local ceilings deliberately before issuing a larger request.

Provider capacity uses nonblocking, home-wide file leases. All required slots must be available before any candidate Reality is created. This is a cap for the new experiment service, not a host-wide quota or a retroactive cap on older manual/chaos worktrees. Capacity and parallelism are bounded to 32. All candidate worktrees are prepared at one verification barrier; only `max_parallel_realities` candidate workers execute simultaneously. Agent-origin requests additionally use `max_parallel`.

Agent admission is independent of user CLI experimentation. Within a Bridge session, allocated candidates count cumulatively against the session Reality cap and local agent-run cap. Cancelled/failed reservations are not refunded, preventing retries from bypassing limits. Pre-execution rejected requests are not charged. The daemon uses a bounded queue (16 waiting requests, one experiment executing at a time, bounded parallel candidates inside it). Separate user CLI processes coordinate capacity leases but do not share a global billing account.

The deadline does not drop an in-flight runner future: cancellation kills its ordinary process group, waits for reaping, records partial evidence and cleans up. Some Git operations are synchronous and not preemptible; cleanup and persistence may take additional time. A process that deliberately creates a new session/group can escape this mechanism. This is not a hard operating-system resource sandbox.

The legacy `run --experience-budget N` continues to cap **additional** paired trials/retries, excluding the initial run, through the core budget type. Its preexisting per-run timeout behavior remains; its learning-loop API rejects shared duration budgets rather than pretending to enforce them. `hardknock try` is the entry point for V0.4 duration and provider-capacity semantics. Existing chaos/counterfactual engines retain their documented limits while sharing trial execution.

Network intent is advisory on the Git provider. `allow_network=false` does not disable sockets or remote inference. Agent requests for `allow_network=true` are rejected unless allowed in local agent-request configuration. External mutations and arbitrary filesystem scopes are unsupported regardless of network intent. Trust the commands and use a disposable local task.
