# Hardknock

**The empirical experience engine for autonomous AI agents.**

Agent experience infrastructure for safe experimentation, empirical learning, and resilience.

> **Let agents fail here, not in production.**

**Agents reason. Hardknock gives them experience.**

Claude Code, Codex, Hermes, OpenClaw, Kiro, and other agents decide what to do. Hardknock is being built to give them disposable environments to try it, record what actually happened, test what they think they learned, and carry validated experience into future work. It sits underneath your agent; it is not another agent.

<p align="center">
  <img src="mascot.png" alt="Lottie the Axolotl, the Hardknock mascot, holding a wrench and an experiment checklist" width="240">
</p>

**Meet Lottie the Axolotl.** 🌸 She has broken this build 47 times in the Dojo so your production agent doesn't have to. Axolotls regenerate after injury; Lottie represents the same ambition for agents: recover, test alternatives, and retain what helped.

> **Pre-alpha · Milestones 0–2 implemented.** The local execution substrate runs commands in Git worktrees and records their output. Evaluation, Experiences, Lessons, and the learning loop remain planned. The learning demo below is illustrative.

[Run the prototype](#run-the-current-prototype) · [Learning-loop preview](#learning-loop-preview-planned) · [Experience](#experience-is-evidence) · [V0.1 scope](#v01-scope) · [Contributing](#contributing)

## Run the current prototype

**Implemented:** a Rust CLI, detached Git Realities, a generic agent-command adapter, stdout/stderr and diff artifacts, SQLite execution records, JSON output, deadlines, and cleanup on Ctrl-C. Linux and macOS are the current targets. Build with stable Rust, Git, and a C compiler:

```bash
cargo build --locked
./target/debug/hardknock --help
./target/debug/hardknock --version

./target/debug/hardknock --repo /path/to/clean-repository run \
  --agent-command 'sh -c "{task}"' \
  'printf "hello from the Dojo\n"'

./target/debug/hardknock reality list
./target/debug/hardknock execution list
```

Use a repository with a committed starting state and no staged, unstaged, or untracked changes. The run saves output and a patch under `~/.hardknock`, then discards the trial worktree; use `--keep` to retain it. `HARDKNOCK_HOME` selects another dedicated data directory outside the source repository.

**Experimental safety boundary:** Git worktrees are not secure sandboxes. Network, credentials, the host filesystem, and Git objects/refs are shared. Run only trusted commands on disposable tasks. Process exit zero is recorded but is **not yet evaluated task success**.

**Planned:** `--check`, named Claude/Codex adapters, Experience and Lesson commands, counterfactual experiments, confidence, retrieval, and retry. See the [implemented CLI reference](docs/cli.md), [architecture](docs/architecture.md), and [next milestones](docs/roadmap.md).

## The problem

Agents can remember failures, reflect on them, and save successful workflows as skills. But a plausible explanation can become a durable lesson before anyone checks whether it is right.

An agent changes a dependency, the build fails, and it remembers: “Never use this package.” Was the package responsible? Or was it the version, the environment, a stale lockfile, or an unrelated flaky test?

Reflection, episodic memory, skill learning, sandboxing, replay, and evaluation are valuable pieces. The missing connection is a disciplined way to turn their observations into experience supported by experiments.

## The Hardknock idea

> **Reflection proposes experience. Experiment promotes it.**

```text
A common reflective loop
  Failure → Reflection → Memory

The Hardknock loop
  Failure → Reflection → Hypothesis → Controlled Experiment
                                             ↓
                                          Evidence
                                             ↓
                                    Validated Experience
```

**Reflection → Hypothesis. Not Reflection → Truth.** Reflection proposes an explanation; an execution supplies evidence. Experiments test whether the explanation deserves to influence future work.

Hardknock's purpose is to let agents **generate, validate, accumulate, revise, and eventually retire experience**. A failed trial remains useful evidence even when its proposed lesson turns out to be wrong.

## Learning-loop preview (planned)

**Illustrative UX, not an available command.** The outcomes and confidence scores below are mock data. Scores are not calibrated probabilities, and two matching trial pairs do not automatically establish validation; promotion criteria are still to be designed.

```console
$ hardknock run --agent hermes "Upgrade dependencies in this pnpm workspace"

🌸 Dojo ready · reality r-184
→ Hermes

✗ Attempt 1 failed · 13 tests failed
  signature: duplicate_lockfile

? Candidate lesson
  npm inside this pnpm workspace caused conflicting dependency state
  confidence 0.42

Testing counterfactuals from the same starting snapshot...
  pair 1  r-185  npm install    ✗
          r-186  pnpm install   ✓
  pair 2  r-201  npm install    ✗
          r-202  pnpm install   ✓

✓ lesson-042 validated under tested conditions
  confidence 0.42 → 0.94

↻ Retrying with lesson-042

✓ Complete in the Dojo
  214 tests passed · 7 files changed
  2 task attempts · 4 counterfactual trials
  patch ready for review · nothing deployed
```

The lesson should be inspectable, not just injected into a prompt:

```console
$ hardknock lesson show lesson-042

lesson-042                              VALIDATED

Context
  This repository and tested dependency snapshot
  pnpm-workspace.yaml present

Avoid under these conditions
  npm install

Prefer
  pnpm install

Evidence
  r-185    FAIL     r-186    PASS
  r-201    FAIL     r-202    PASS

Confidence
  0.94 · illustrative score

Scope
  Validated under tested conditions; recheck when context changes
```

## How it works

1. **Capture an execution.** Record the goal, starting state, actions, observations, and evaluation, including failures.
2. **Propose a hypothesis.** Reflection extracts a Candidate Lesson with a specific claim and scope.
3. **Fork and test.** Restore a known starting point, vary the relevant factor, and run controlled alternatives within a budget.
4. **Compare evidence.** Evaluate outcomes, inspect differences, and look for both support and contradiction.
5. **Retain and reuse.** Retrieve relevant, supported lessons for a retry or a related task; preserve their provenance and uncertainty.
6. **Revisit.** New failures or changed environments can weaken a lesson, trigger a retest, or retire it.

### Agent experience infrastructure

| Existing building block | What it provides | What Hardknock adds |
| --- | --- | --- |
| Memory | What an agent remembers | Whether that memory is supported by execution evidence |
| Sandbox | Containment of unsafe effects | Controlled failure that produces useful evidence |
| Evaluation | Whether an agent succeeded | Experiments about what to learn from success or failure |
| Skill framework | A procedure for what to do | Evidence about where it fails, warning signs, and recovery |

Hardknock is designed to compose with these systems. Its central resource is experience, not a memory database, sandbox runtime, evaluation suite, skill registry, or multi-agent orchestrator.

## Experience is evidence

```text
Experience ≠ Memory
Experience ≠ Lesson
Experience ≠ Skill
```

An **Experience** is evidence from an actual execution. A reflection alone cannot create one. Higher-order artifacts—skills, lessons, reflexes, and recoveries—are derived from one or more experiences.

| Artifact | Meaning |
| --- | --- |
| **Experience** | What actually happened: the execution record and its evidence. |
| **Skill** | What has been observed to work under recorded conditions. |
| **Lesson** | What tends to fail under particular conditions, the supported explanation, and what to try instead. |
| **Reflex** | A recognizable precursor to a known failure that can trigger warning, reconsideration, or replanning. Severe cases supported by strong evidence may justify blocking, but only with independent policy authorization. |
| **Recovery** | A validated procedure for returning to a safe state after failure. |

The proposed typed Experience schema captures:

| Field group | Contents |
| --- | --- |
| Intent and context | Goal, starting state or snapshot reference, environment, agent/model identity and version |
| Execution | Actions, inputs, tool calls, perturbations, predictions |
| Evidence | Observations, command output, filesystem changes, outcome, evaluation and evaluator version |
| Failure and recovery | Surprise or prediction error, failure signatures, recovery behavior and outcome |
| Provenance and uncertainty | Parent trial, experiment, timestamps, artifact references, evidence limitations, confidence and its basis |

Confidence belongs to a specific claim or evaluation, not to every byte in a log. Preserve the execution record; version and revise interpretations of it. “Validated experience” means evidence with conclusions validated under tested conditions, not an infallible account of the world.

Future derived artifacts could include heuristics, operating envelopes, invariants, and causal models. These are design directions, not V0.1 commitments.

## Evidence has a lifecycle

The proposed evidence ladder makes promotion explicit. `OBSERVED` describes a captured outcome; subsequent stages describe support for a claim derived from evidence.

```text
OBSERVED → CANDIDATE → COUNTERFACTUALLY SUPPORTED
                                  ↓
                             REPLICATED → VALIDATED → TRUSTED
```

| State | What it means |
| --- | --- |
| **Observed** | An execution and its outcome have been recorded. |
| **Candidate** | A scoped explanation has been proposed, but not yet tested. |
| **Counterfactually supported** | Controlled alternatives support the proposed relationship. |
| **Replicated** | Further controlled trials support it, with dependencies between trials accounted for. |
| **Validated** | It meets declared evaluation criteria within a tested scope. |
| **Trusted** | Sustained evidence supports use under an explicit adoption policy. |

These are evidence states, not mathematical proof. Passing an evaluator does not prove the evaluator is complete. Confidence must account for trial quality, nondeterminism, contradictory evidence, and the conditions actually tested. The scoring method and promotion thresholds remain open engineering questions.

Experience must also support **unlearning**:

```text
TRUSTED → CONTRADICTED → WEAKENED → RETIRED
                             ↘ retest under a revised scope
```

Transitions need not visit every state. A changed dependency, model, or environment can require immediate revalidation or retirement. Keep the history and the reason for the change; do not erase inconvenient evidence. “Trusted” never grants additional execution permissions.

## The Dojo

**Dojo** is Hardknock's proposed controlled experimentation subsystem. It organizes disposable **Realities** in which an agent can execute alternative strategies.

```text
Sandbox:        Safety → prevent unsafe effects
Hardknock Dojo: Safety → permit failure → learn
```

A **Reality** is an isolated execution state from a known starting point, within a declared isolation boundary. A failed Reality can be discarded while its evidence is retained.

```text
                      Starting Reality
                              │
                 ┌────────────┼────────────┐
                 ↓            ↓            ↓
                R1           R2           R3
                 │            │            │
            strategy A   strategy B   strategy C
                 │            │            │
                FAIL         PASS         PASS
                 │            │            │
                 └────────────┼────────────┘
                              ↓
                           evaluate
                              ↓
                        learn / retain
```

Long-term conceptual operations, **not the current Rust API**. The implemented `RealityProvider` supports create, fork, diff, and discard; its fork recreates the recorded starting commit.

```text
Reality.create()    establish a known starting state
Reality.fork()      derive a trial from that state
Reality.execute()   run permitted actions and record effects
Reality.diff()      inspect changes against the starting state
Reality.evaluate()  apply declared success and safety checks
Reality.commit()    accept selected artifacts through a policy gate
Reality.discard()   remove trial state while retaining evidence
```

`commit()` means accepting a result within the backend's supported boundary—for example, a reviewed patch. It is not a promise to deploy, merge, or roll back arbitrary external effects.

### What “safe” requires

Git worktrees separate working files; **they are not a security boundary**. Containers also require deliberate credential, filesystem, process, and network restrictions. The intended trial policy excludes production credentials and denies external writes and network access unless explicitly allowed, such as narrowly scoped agent inference access.

That policy is a target for stronger backends. The current Git-worktree runner does **not** enforce credential or network restrictions; it prints the shared-host boundary before execution.

A filesystem reset cannot undo an API call, email, payment, or cloud mutation. V0.1 must keep irreversible external effects outside its supported trial boundary. Reproducibility also depends on pinned inputs, dependency sources, clocks, randomness, and external services; a shared Git commit alone is insufficient.

## Counterfactual validation

Suppose an agent runs `npm install` inside a pnpm workspace and the build fails. It proposes:

> Running npm inside this pnpm workspace caused conflicting dependency state.

That is a **Candidate Lesson**, not truth. The proposed experiment restores the same starting state for each branch, varies the package-manager choice, and applies the same evaluation:

```text
                 identical starting state
                       /         \
                      /           \
              npm install       pnpm install
                   ↓                 ↓
                  FAIL              PASS
```

Keep starting files, toolchain versions, dependency inputs, and evaluation fixed wherever the backend can control them. Record anything that cannot be held constant. Repeat trials to check for flaky tests and nondeterministic outcomes.

If the difference persists, the evidence supports a contextual preference. It does not, by itself, establish that a lockfile conflict was the only cause: the package managers can change several aspects of dependency resolution. Inspect logs and diffs, and test narrower explanations where needed.

A resulting lesson might be:

> When operating inside this pnpm workspace under the tested dependency configuration, prefer pnpm rather than npm because npm may create conflicting package-manager state.

The claim is **empirically supported under tested conditions**. It is not a universal prohibition on npm. Replication can increase confidence without expanding the lesson's scope beyond the evidence.

## Chaos engineering for agents

Traditional chaos engineering introduces controlled perturbations to discover weaknesses before production incidents do. Hardknock proposes applying the same philosophy to **agent behavior**: do not merely wait for useful mistakes; create bounded adversity and observe how the agent responds.

> **Hardknock doesn't just teach an agent how to succeed. It discovers how that success breaks.**

Planned perturbation families include:

| Surface | Examples |
| --- | --- |
| Network and dependencies | Latency, packet loss, an unavailable dependency |
| Identity and permissions | A stale credential, permission denial |
| Tools and APIs | A partial API response, tool failure |
| Workspace and configuration | A conflicting concurrent change, malformed configuration |
| Information and instructions | Outdated documentation, ambiguous instruction, stale memory, misleading external information |

```text
Known Skill → Chaos Trials → Discover Failure Boundary
                                        ↓
                             Lesson + Reflex + Recovery
                                        ↓
                               More Resilient Skill
```

An **operating envelope** is the set of conditions under which a skill has been empirically observed to remain effective. Outside that envelope, an agent should carry uncertainty rather than assume success will transfer.

Chaos trials should specify the perturbation, control run, success checks, safety limits, and stopping conditions. They belong in disposable environments, not live production. This is a later milestone, not a claim about current functionality.

### Experience budget

**Stop guessing. Try it.** When uncertainty is high, spend a bounded experimentation budget on alternatives in disposable Realities instead of spending unlimited tokens imagining outcomes.

Proposed syntax:

```bash
hardknock try --trials 3 "find the safest migration strategy"
hardknock run --agent codex --experience-budget 5 "repair the failing build"
```

The intent is a cap on trial executions, with additional limits on tokens, tool calls, elapsed time, and cost. A budget must cover validation and retries, not just successful runs. Exact accounting is not yet specified. Experimentation complements inference-time reasoning: reasoning chooses useful experiments; experiments provide observations that reasoning alone cannot.

## Works with your agent

The integration model is **Claude + Hardknock, Codex + Hardknock, Hermes + Hardknock, OpenClaw + Hardknock**. Named agent adapters are planned. The implemented generic adapter can launch a configured noninteractive CLI through `--agent-command`; it does not imply vendor-specific integration or tested compatibility.

```text
                        USER
                         │
                         ▼
       ┌───────────────────────────────────┐
       │               AGENT               │
       │  Claude Code │ Codex │ Hermes     │
       │  OpenClaw │ Kiro │ future agents  │
       └─────────────────┬─────────────────┘
                         │
                         ▼
       ┌───────────────────────────────────┐
       │             HARDKNOCK             │
       │                                   │
       │  Experience Engine                │
       │  Lessons / Skills / Reflexes      │
       │  Recovery / Evidence              │
       │                                   │
       │  Dojo                             │
       │  Reality / Fork / Chaos           │
       │  Replay / Counterfactuals         │
       └─────────────────┬─────────────────┘
                         │
                         ▼
               EXECUTION ENVIRONMENT
```

| Integration stage | Contract |
| --- | --- |
| **Runner compatible** | Hardknock launches an existing CLI in an isolated environment. No agent modifications are required for this level; the adapter handles invocation and capture. |
| **Experience aware** | The agent queries an API/MCP-style interface for relevant lessons, skills, recovery patterns, and supporting evidence. |
| **Experimental** | The agent explicitly requests forks, multiple trials, counterfactual experiments, or chaos experiments. |
| **Reflex integrated** | Before consequential actions, relevant reflexes can return `continue`, `advise`, `warn`, `replan`, or `stage as experiment`; `block` requires independent policy authorization. |
| **Native** | Deeper integration with agent hooks, tools, skills, session state, and execution lifecycle. |

V0.1 is intended to start with the Runner model, initially targeting Claude Code and Codex, with Hermes where practical. Other CLI agents would follow through adapters. Illustrative runner shorthand:

```bash
hardknock run claude
hardknock run codex
hardknock run hermes
```

Launching a CLI does not imply support for its internal reasoning, session hooks, or reflex interception. Those require additional integration and explicit permissions.

## Experience survives the agent

> **Experience belongs to the environment, not necessarily to the model that discovered it.**

Codex could discover and validate a repository-specific lesson. Later, Claude could use its evidence in the same repository. Hermes could replicate the result—or contradict it. The intended durable asset is the evidence, not the identity of its first observer.

A separate illustrative shared lesson summary:

```yaml
id: lesson-117
scope:
  repository: payments-service
  package_manager: pnpm
  environment: recorded-trial-environment
discovered_by: codex
supported_by:
  - codex
  - claude
  - hermes
evidence_status: REPLICATED
evidence:
  codex: [r-301, r-302]
  claude: [r-315, r-316]
  hermes: [r-330, r-331]
```

This is a proposed record shape, not a report of completed cross-agent runs. Full provenance must retain model and agent versions, operating systems, dependency versions, infrastructure topology, and trial configuration.

Retrieval must check applicability. Do not blindly transfer lessons across repositories, environments, model versions, operating systems, dependency versions, or infrastructure topologies. Cross-agent replication should increase confidence, **not erase scope**. Evidence also stays subject to access controls; reuse is not permission to share private logs or secrets.

## CLI resource model

The proposed first-class resources are `experience`, `skill`, `lesson`, `reflex`, `recovery`, `experiment`, and `reality`. This is the **long-term CLI model**; only the basic `run` and Reality operations are implemented from this tree. The current CLI also has `execution list/show` for raw process records. See [docs/cli.md](docs/cli.md) for the implemented command set.

```text
hardknock
├── run
├── try
├── experience
│   ├── list
│   ├── show
│   └── replay
├── lesson
│   ├── list
│   ├── show
│   ├── test
│   └── retire
├── experiment
├── reflex
├── skill
├── recovery
├── chaos
├── reality
└── why
```

The signature inspectability command is a future interface:

```bash
hardknock why
```

When a reflex affects an agent decision, it should explain the relevant context, evidence, uncertainty, and policy authorization through a traceable chain:

```text
Decision → Reflex → Lesson → Experiment → Experience
```

## Target architecture

| Component | Responsibility |
| --- | --- |
| Agent adapters | Launch agents, normalize observable execution events, and expose supported integration capabilities |
| Dojo backends | Create Realities, enforce declared isolation limits, execute trials, and capture diffs and outputs |
| Experiment controller | Define controls and perturbations, schedule bounded trials, and compare evaluations |
| Experience Engine | Store typed evidence, propose lessons, track support and contradiction, and retrieve applicable artifacts |
| Policy and review gates | Keep trial permissions and artifact acceptance explicit; authorize any blocking behavior independently of confidence |

These are conceptual boundaries, not a commitment to separate services. The current substrate is a single modular Rust crate with SQLite metadata, local artifact files, a generic command adapter, and a Git-worktree `RealityProvider`. The Experiment Controller and Experience Engine remain planned. See the [architecture decisions and current guarantees](docs/architecture.md).

## V0.1 scope

**Initial focus: coding agents.** Use existing execution primitives and repository-local tasks to test the central idea.

### Dojo: implemented foundation

- Detached Git worktrees, with explicit warnings about shared host access.
- Clean committed starting snapshots and forks from the recorded starting commit.
- Generic process execution, command/output capture, filesystem diffs, and persistent execution records.
- Timeouts, signal handling, optional retention, and explicit orphan cleanup.

**Still planned:** test/evaluation results, broader environment reproducibility, and stronger isolation backends such as containers.

### Planned Experience Engine

- A typed Experience schema and evidence provenance.
- Candidate Lesson extraction from observed executions.
- A controlled counterfactual trial workflow.
- Confidence/evidence tracking, including contradictory outcomes.
- Retrieval of validated Lessons for relevant contexts.
- A retry that uses the retrieved experience.

The first useful demonstration should connect these pieces end to end: a coding agent fails, proposes a lesson, tests it, and completes a related attempt using the resulting evidence.

### Out of scope initially

- Universal rollback of arbitrary external APIs.
- Production cloud mutation virtualization.
- A full WASM runtime.
- General browser transaction semantics.
- Financial or irreversible external effects.
- Fully autonomous policy enforcement.
- Arbitrary causal proof.

## What we want to demonstrate

**Research/engineering hypothesis:** An autonomous agent that can safely generate controlled experiences, validate inferred lessons using counterfactual experiments, and retrieve those lessons in related tasks should make fewer repeated mistakes and recover more effectively than an otherwise equivalent agent relying only on reflection or successful-skill memory.

Compare equivalent agents on matched tasks and environments, with explicit budgets and held-out tasks. Count experiment costs as part of the system cost. Report failures and tradeoffs alongside improvements.

Measurements should include:

- **Effectiveness:** task success rate, repeated mistake rate, recovery success rate, time to recovery.
- **Safety and reliability:** unsafe action rate, lesson precision, false-positive reflex rate.
- **Cost:** tool calls, token cost, elapsed time, and trial count.
- **Transfer:** experience transfer rate to novel tasks within an applicable context.

The key test is **experience transfer**: does an agent avoid an analogous failure on a new task because of a lesson learned elsewhere? Retrying the same task successfully is useful, but does not establish transfer.

No benchmark results are available yet.

## Technical principles

| Principle | Commitment |
| --- | --- |
| **Evidence over introspection** | LLM reflection generates hypotheses, not facts. |
| **Safe failure is useful** | Failure within a controlled boundary is a source of information. |
| **Context matters** | Lessons are conditional beliefs with scope and confidence, not global prohibitions. |
| **Experience is inspectable** | Every durable conclusion should trace back to observable evidence. |
| **Experience is revisable** | Lessons can be weakened, contradicted, retested, or retired. |
| **Agents are interchangeable** | Evidence should survive changes in the reasoning model or agent implementation; applicability must be checked again. |
| **Failure should improve resilience** | Measure recognition, recovery, and operating judgment as well as completion. |

## Roadmap

All milestones are proposed, with no release dates committed.

| Milestone | Intended outcome |
| --- | --- |
| **V0.1 — Controlled coding experiments** | Runner adapters, disposable coding trials, typed evidence, counterfactual lesson validation, retrieval, and retry |
| **V0.2 — Richer experience and retrieval** | Better context matching, contradiction and retirement workflows, cross-agent replication, and transfer measurements |
| **V0.3 — Chaos and operating envelopes** | Controlled perturbations, failure-boundary discovery, and evidence for reflexes and recovery |
| **V0.4 — Deeper agent hooks** | Lifecycle integration and policy-authorized reflex responses before consequential actions |
| **Later — External-effect virtualization** | Explore explicit semantics for selected external systems, without claiming universal rollback |

## Project status

Hardknock is a **pre-alpha execution prototype**. Milestones 0–2 are implemented and covered by local unit/integration tests. The CLI builds from source; there is no published release package or benchmark result. CI is configured for Linux and macOS.

The empirical learning loop has not been implemented or demonstrated yet. Next come evaluation and structured Experiences, followed by a deterministic counterfactual fixture. APIs, command syntax, and schemas may change as those milestones expose what is needed.

## Contributing

Build, formatting, lint, and test instructions are in [CONTRIBUTING.md](CONTRIBUTING.md).

Early contributions are welcome as focused design discussions, reproducible failure cases, and small implementation proposals. Useful starting points include:

- A coding task where reflection produces a plausible but incorrect lesson.
- An Experience schema that preserves scope, provenance, and contradictory evidence.
- Safe Dojo backend boundaries and reproducible trial fixtures.
- Runner adapters for Claude Code, Codex, or Hermes.
- Evaluation designs that distinguish repeated-task success from experience transfer.

Open an issue or pull request with the problem, starting conditions, proposed behavior, and how you would test it. For experiments, include a control, an evaluation rule, and the conditions that would contradict the hypothesis. Use disposable fixtures; do not include credentials, private execution logs, or trials that mutate live services.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

`SPDX-License-Identifier: Apache-2.0`

See [NOTICE](NOTICE) for project notices. Third-party components retain their respective licenses.
