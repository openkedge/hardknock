# Agent experience contract

Hardknock launches noninteractive agents; it does not intercept their internal commands or give them additional permissions. The existing `AgentAdapter::build_command(task)` interface is preserved. Context preparation happens in the shared workflow before command execution.

## Input: Reality-local context

Use `--with-experience` for a generic agent or explicit script. Test agents enable it by default. Hardknock provides:

```text
.hardknock/context.md     readable scoped recommendations
.hardknock/context.json   schema_version 1, task, QueryContext, relevant_lessons
```

Each JSON `relevant_lessons` entry is a `RetrievedLesson`: the full immutable-at-delivery Lesson snapshot, relevance score, `matched_context` signals/weights, and recommendation category. The Lesson includes confidence, avoid/prefer actions, scope, evidence IDs, and origin. The context includes the Reality working directory, source repository, markers, tags, and selected environment facts. It does not copy arbitrary environment variables or credentials.

Both files exist even if there are no eligible recommendations. At most 20 recommended Lessons are included. The directory is reserved: existing repository content or a symlink causes an intervention. Hashed copies of both inputs are saved as Experience artifacts before the agent can modify them. The files are advisory and do not authorize execution, access to secrets, or sharing evidence.

Hardknock does not automatically add instructions to an opaque agent's prompt. Tell the agent to read the context files and consider their scope before acting. The task is substituted into exactly one complete `{task}` argv element; Hardknock performs no implicit shell expansion.

```bash
hardknock --repo /path/to/clean-repository run \
  --with-experience --action 'npm install' \
  --agent-command 'my-agent --prompt {task}' \
  --check './test.sh' \
  'Read .hardknock/context.md and context.json. Use applicable evidence to complete the task. Report any Lesson use in .hardknock/usage.json.'
```

Proposed actions are caller-supplied retrieval hints, not intercepted or forbidden commands. Generic agents inherit their environment and keep their existing authentication, sandbox, and approval settings. Script/test runs use Hardknock's controlled environment. All agents receive closed stdin and a process deadline.

## Output: optional application report

An agent can write `.hardknock/usage.json`:

```json
{
  "schema_version": 1,
  "applications": [
    {
      "lesson_id": "lesson-00000000-0000-4000-8000-000000000001",
      "influence": "applied",
      "resulting_action": {
        "type": "shell_command",
        "pattern": "./agent-script.sh alternative"
      }
    }
  ]
}
```

Use an actual delivered ID. Valid influences are `retrieved`, `consulted`, `applied`, `ignored`, and `contradicted`. `resulting_action` is optional/null when no changed action is reported. Report disagreement honestly; do not claim success because advice was present.

Reports must be regular, nonsymlink files of at most 64 KiB with schema version 1, no unknown fields, at most 20 entries, and no duplicate or undelivered Lesson IDs. The context directory must not have been replaced by a symlink. Hardknock reads the report after the agent exits and before evaluation, saves a hashed copy if valid, and records malformed reports in `application_report_errors`. Invalid reports do not turn a passing evaluation into failure, and do not establish application.

Opaque reports are **SelfReported**, including claims of application or contradiction. They cannot promote a Lesson to Validated. Evaluation is an independent set of required checks. Agent stdout, the report, and the checked outcome remain inspectable together.

The fixture adapter separately observes exact `RETRIEVED`, `APPLIED`, `IGNORED`, and strategy lines emitted by committed scripts parsing the injected context. It requires the delivered ID and the preferred action before recording **Observed** application. This is a trusted fixture protocol, not tamper-proof instrumentation for arbitrary agents. Human inspection of another agent's log does not silently upgrade its database verification level.

## Codex CLI smoke test

On 2026-08-27, Codex CLI 0.149.1 (configured model `gpt-5.6-sol`) successfully consumed advice learned by test-agent on A and applied it to B through the generic adapter. Invocation:

```bash
hardknock --repo /path/to/B run \
  --with-experience --action 'npm install' \
  --agent-command 'codex exec --sandbox workspace-write --ephemeral --color never {task}' \
  --check './test.sh' --timeout-secs 120 \
  'Read .hardknock/context.md and context.json. Choose and execute an explicit agent-script.sh mode using the scoped Lesson, then run ./test.sh. Do not change scripts, manifests, or checks. Do not run real package managers or access secrets. Write usage.json with the actual delivered ID and exact action; report blockers honestly.'
```

The tested command used an absolute path to the installed `codex` executable. No model override, credential copying, sandbox bypass, or rules bypass was used. Codex ran `./agent-script.sh alternative`, passed `./test.sh`, and wrote the application report. Hardknock recorded `Applied / SelfReported`, successful evaluation, and the executable identity; confidence stayed at 0.90. Generic adapter model/version fields remain unset rather than guessed; this smoke-test version/model was read from the captured CLI header.

This task's fixture outputs were produced in the disposable repository; the agent runtime may maintain state elsewhere under its own policy. The fixture does not download packages. Calling a real model requires the user's existing agent account/network access and is not part of the offline test suite. See [OpenAI's noninteractive-mode documentation](https://learn.chatgpt.com/docs/non-interactive-mode) for `codex exec`, explicit sandbox selection, and ephemeral sessions. Check your installed agent's help and organization policy; do not bypass a permissions or authentication failure.

## Limits

No named vendor adapter, command interception, automatic parsing of vendor event streams, model-brand promotion rule, MCP server, or enforcement is implemented. The generic adapter records the executable; future adapters may add verified version/model information and independent action observers. Git worktrees remain repository isolation, not a host security boundary. Tasks, logs, and Lesson text can contain sensitive data; review them before sending them to a model or sharing artifacts.
