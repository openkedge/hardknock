# Portable agent experience contract

**Reflection proposes experience. Experiment promotes it.**

Experience belongs to Hardknock. Vendor adapters translate observable decisions and events; they do not define lesson truth, directly read SQLite, or request hidden chain of thought.

An agent receives its task and current context, compact relevant experience, evidence summaries, and optional reflex/recovery advice. An agent can report observable actions, lesson usage or disagreement, and run outcomes, and can explicitly request bounded strategy experiments. Recovery tools and skill-validation requests remain future extensions. The implemented input/output boundary is listed in [Bridge v1](bridge-protocol.md).

## Experience on demand (V0.4)

The shared `hardknock try --session <id>` helper and `experiment_requested` event accept explicit candidates, evaluator, budget, criteria, and capability declarations. The Bridge supplies the registered requester and recorded repository starting commit, returns acceptance/rejection, and exposes cursor-based progress and a compact structured result. Claude hook-injected context and Codex turn context describe this same contract; no MCP server is required.

The agent remains the decision-maker. Hardknock does not start experiments automatically, grant native approvals, adopt a winning Reality, or commit changes. Results identify which candidate performed better under the specified checks, including ties and uncertainty. Confounded comparisons are not causal lessons. Controlled comparisons may propose Candidate Lessons, but never promote them automatically.

The current provider cannot fork a running session or enforce host/network isolation. Acceptance discloses the recorded-commit fallback, excludes dirty/ignored inputs and process state, and warns that commands must be trusted. Session end cancels active agent requests by default. Operational candidate prompts/commands are explicit reproducibility data and are persisted; do not submit secrets or private conversation transcripts. See [agent experiments](agent-experiments.md), [quality](experiment-quality.md), and [budgets](experience-budget.md).

## Experience injection

A brief contains an ID, asset kind, summary, confidence, relevance, scope, and evidence count. Context defaults to five eligible lessons, never an entire Experience. Confidence is a heuristic, not a calibrated probability. Evidence counts are references, not automatically independent controlled trials.

The injected document states:

> Treat Hardknock lessons as evidence-backed prior experience. Reconsider them when the current context differs materially.

It labels experience as evidence, not system policy. A supported lesson normally advises, an active reflex requests reconsideration, and only independently configured policy can prohibit an action. Native user approvals remain native user approvals.

## Usage and disagreement

Delivery alone is not application. A preferred shell action must complete successfully in native lifecycle evidence with the right workspace and a clean baseline before it can count as observed application. A model saying “I used the lesson” is insufficient. The local adapter remains a trusted reporter; this is not independent verification of every external effect.

`lesson_rejected` accepts `context_mismatch`, `environment_changed`, `contradicted_by_observation`, `alternative_unavailable`, and `other`, plus an optional short detail. It records `LessonInfluence::Rejected` for that run. Two distinct session rejections, or an environment-change rejection, mark a validated lesson `needs_revalidation`; its status and historical evidence remain intact. This is not a vote to retire a lesson.

## Cross-agent evidence

`hardknock lesson show <id>` includes `AgentEvidenceContribution` records: agent identity, Experience ID, evidence relationship, and observed role (`discovery`, `counterfactual`, `application`, `successful_transfer`). Counterfactual trials retain their actual `scripted-trial` identity; they are not relabeled as a model run.

A native observation can be followed by an explicit paired **controlled reconstruction** when it has a clean committed Git baseline, a completed observed shell action, a scoped candidate and a configured evaluator. The plan marks `external_reconstruction: true`. This tests the two actions under a new common controlled environment; it does not pretend to replay the agent's inherited environment or reasoning. The source observation is immutable.

Validation still requires controlled support plus an evaluated, observed application in a distinct repository tree. A different agent name does not by itself improve confidence or make evidence epistemically independent. Dirty/unversioned starts cannot support this reconstruction or observed-transfer promotion.

## Explicit governed effects (V0.8)

The Bridge accepts `effect_proposed`, `effect_commit_requested`, `effect_discard_requested`, `effect_status`, and `effect_reconcile_requested`. Proposal uses a structured `EffectRequest`; a successful response always says `committed: false` and states that no authoritative mutation occurred.

Bridge agents receive observe/propose/prepare capability and may discard their uncommitted staging. They do not receive commit or compensate capability. `effect_commit_requested` therefore records a rejected attempt and returns `authorization_required`; model text such as “I approve” cannot substitute for user, CI, policy, or external approval authority.

This is explicit tool use, not transparent interception. Native tool calls, shell commands, and network requests outside the registered effect tools retain the ordinary host boundary.

## Capability-isolated agent execution (V0.9)

An integrated container agent uses host-mediated reasoning and Reality-mediated execution. Hardknock creates a manifest before the agent command, issues a signed token bound to the Reality/manifest revision, and routes shell/file tools through the execution proxy. The model's host credential does not enter the container.

The per-Reality Bridge relay accepts only that Reality's token. `hk-effect` exposes propose, status, and discard; proposal may prepare but always returns `committed:false`. The relay never exposes commit, and the Effect Manager repeats exact kind/target/operation/action checks after authentication. A model statement or stolen token from another Reality cannot become mutation approval.

Capability denials are observable events that may explain a failed attempt or suggest a later user-reviewed profile revision. They are not permission for the agent to broaden its own manifest. Experience/federation records retain execution-assurance metadata so a receiver can distinguish cooperative worktree evidence from container-gated evidence.

Container integration currently covers one execution. Native lifecycle adapters still observe their host workspace and are not retroactively contained. Automatic retry/reflection and the trusted evaluator remain host-side until those execution paths are routed through the proxy.

## Named tools and attestations (V0.10)

Adapters should expose canonical Hardknock `ToolDefinition` objects when their
lifecycle supports structured tools. The agent can query names, schemas, and
capability summaries on demand; the entire registry is not injected into every
prompt. Each invocation receives the intersection of Reality, tool, and
temporary policy capabilities, then returns a `ToolExecutionReceipt` pointing
to an immutable execution attestation. A tool may propose or prepare a
structured Effect without receiving commit authority or database credentials.
