# Hardknock

**The empirical experience engine for autonomous AI agents.**

Agent experience infrastructure for safe experimentation, empirical learning, and resilience.

> **Let agents fail here, not in production.**

**Agents reason. Hardknock gives them experience.**

**Models change. Experience survives.**

Claude Code, Codex, Hermes, OpenClaw, Kiro, and other agents decide what to do. Hardknock is being built to give them disposable environments to try it, record what actually happened, test what they think they learned, and carry validated experience into future work. It sits underneath your agent; it is not another agent.

<p align="center">
  <img src="hardknock-murph.png" alt="Murph, the Hardknock Axolotl, holding a wrench and an experiment checklist" width="240">
</p>

**Meet Murph, the Hardknock Axolotl.** Axolotls regenerate after injury. She represents the same principle for autonomous agents: a failed attempt should leave the agent better prepared for the next one.

Hardknock preserves evidence from each attempt, uses reflection to propose lessons, and tests those lessons before carrying them into future work. Murph symbolizes that accumulated experience.

**Every knock leaves a lesson.**

> **Pre-alpha · V0.8 transactional Realities.** Hardknock can stage supported mock HTTP, database, message, and shadow-deployment effects, compare candidates without mutating authoritative fixture state, require exact-scope authorization, reject stale preparations, reconcile unknown outcomes, and retain commit/compensation evidence. Arbitrary shell and network effects are not intercepted. See the [V0.8 report](docs/implementation-v08.md) and [effects guide](docs/effects.md).

[Works With Your Agent](#works-with-your-agent) · [Run the prototype](#run-the-current-prototype) · [Run the learning demo](#run-the-learning-demo) · [Chaos demo](#dont-wait-for-useful-mistakes) · [Experience](#experience-is-evidence) · [Scope](#v01-scope) · [Contributing](#contributing)

## Run the current prototype

**Implemented:** a Rust CLI, detached Git Realities, generic and scripted execution, required checks, immutable Experiences, scoped Lessons, controlled experiments, deterministic retrieval, application provenance, bounded retries, transfer validation, hashed artifacts, SQLite, JSON output, deadlines, and cleanup on Ctrl-C. V0.2 adds local chaos campaigns, four perturbation types, sparse operating envelopes, scoped reflex tests, explicit activation, recovery experiments, and manual Skill registration. V0.3 adds a versioned local Bridge, native lifecycle adapters, asynchronous evaluation, redaction, and cross-agent evidence provenance. Linux and macOS are the current targets. Build with stable Rust, Git, and a C compiler:

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

**Experimental safety boundary:** Git worktrees are not secure sandboxes. Network, credentials, the host filesystem, and Git objects/refs are shared. Run only trusted commands on disposable tasks. Process exit zero is **not task success**; pass one or more `--check` commands to evaluate the result. No checks means task success is unknown.

V0.4 adds `hardknock try`, structured agent requests, explicit budgets, equivalent-state verification, bounded parallel candidates, comparison quality, replay/lineage, cancellation, and patch export. V0.5 adds curricula, Skill coverage/maturity, Experience Packages, and a held-out resilience benchmark. V0.6 adds persistent development profiles, immutable history, freshness-aware retrieval, and measured learning across episodes. See the [CLI reference](docs/cli.md), [development guide](docs/development.md), and [next phase](docs/roadmap.md).

## A Sandbox Can't Unsend an Email

Disposable filesystems solve only part of safe agent experimentation. Real agents call APIs, update databases, deploy services, and send messages. Those effects survive when the sandbox disappears.

> **Forking state is easy. Forking reality is the hard part.**

> **Hardknock separates experimentation from commitment.**

> **Let the agent try the mutation before the mutation becomes true.**

```text
Agent Action
    ↓
Proposed Effect
    ↓
Prepare in a transactional Reality
    ↓
Experiment
    ↓
Explicit Commit or Discard
```

`PREPARED` never means `COMMITTED`. A passing candidate leaves a structured effect ready for inspection; it does not change authoritative state. Commit re-reads external state, checks the prepared fingerprint and expiration, verifies authorization bound to the exact target and payload, and then invokes the adapter. Every successful commit has a receipt and a separate immutable Experience.

```bash
hardknock effect fixture-set --adapter mock-http \
  --target mock://deployment/service-a --state '{"version":1}'
hardknock effect propose --kind deployment --operation update \
  --target mock://deployment/service-a --payload '{"version":3}' --prepare
hardknock effect show effect-<uuid>
hardknock effect commit effect-<uuid> --yes
hardknock effect reconcile effect-<uuid>
hardknock effect capabilities
```

The deterministic benchmark injects four failed candidates, state drift, response loss, and a partial multi-effect commit. Failed candidates cause **4/4** authoritative mutations under direct execution, **4/4** with filesystem sandboxing alone, and **0/4** through supported Hardknock adapters. The injected unknown outcome is recovered **1/1** with one authoritative mutation and no duplicate. These are local fixture results, not a claim that arbitrary effects are transactional.

Transactional safety is adapter-scoped. V0.8 does not intercept arbitrary HTTP, shell commands, syscalls, real email, payments, AWS, Kubernetes, or PostgreSQL. Experimentation authority and mutation authority are separate capabilities.

## Teams Should Share Experience, Not Superstitions

> **Experience can travel. Trust has to be earned locally.**

> **Hardknock shares evidence, not commandments.**

A lesson learned by one agent can be valuable to another—but only if its provenance, scope, and evidence survive the transfer. Hardknock federates empirical experience rather than copying behavioral rules blindly.

```text
Team A → Evidence → Signed Experience Bundle → Team B
                                                ↓
                                      Context Compatibility
                                                ↓
                                      Local Reproduction
                                        /             \
                                    SUPPORT        CONTRADICT
```

Remote validation never becomes local validation automatically. A valid signature proves who produced a bundle; it does not prove the Lesson is correct in the receiver's environment. External Lessons, Reflexes, Recoveries, Skills, and envelopes remain visibly separate. In particular, a remote `BLOCK` Reflex becomes local `ADVISE` until local evidence and explicit policy permit more.

```bash
hardknock peer add --name platform-team --public-key ./platform-team.pub
hardknock federate export --lesson lesson-<uuid> --output lesson.hkexp
hardknock federate import lesson.hkexp
hardknock federate test federated-<uuid>
hardknock provenance lesson-<uuid>
hardknock conflict list
hardknock profile federation
```

The deterministic three-node benchmark records **2/2** critical successes with evidence federation versus **1/2** for isolated teams and **1/2** for naive shared rules. Node B supports and benefits from the Lesson; Node C contradicts it and keeps the local action. This is designed fixture evidence, not a production reliability estimate. Hardknock turns individual agent failures into institutional experience without requiring every agent to make the same mistake independently, while imported experience remains contextual and advisory until locally supported.

## Does Your Agent Actually Get Better?

Hardknock measures observed behavior across tasks: success, repeated mistakes, recovery, transfer, and remaining uncertainty. Profiles can follow an agent, repository, or explicit task family. They are rebuilt from evidence; they do not assign a universal intelligence score.

**Models change. Experience survives.** The longitudinal fixture replaces the agent/model identity while retaining the origin of each Lesson and recording the new agent's applications. A new model name is not independent replication.

```bash
# Use a fresh dedicated home; the benchmark refuses to mix in existing evidence.
export HARDKNOCK_HOME="$(mktemp -d "${TMPDIR:-/tmp}/hardknock-v06.XXXXXX")"
hardknock benchmark longitudinal --output /tmp/hardknock-longitudinal.json
hardknock profile show --agent test-agent
hardknock profile history --agent test-agent
hardknock growth --agent test-agent
hardknock timeline --agent test-agent --limit 20
```

Measured local results: five episodes, 30 evaluated tasks per arm, no external model or API.

| Arm | Task success | Repeated mistakes | Recovery success | Stale-rule success |
| --- | --- | --- | --- | --- |
| Stateless | 3/30 | 25/30 | 0/15 | 3/3 |
| Reflection memory | 9/30 | 18/30 | 0/15 | 0/3 |
| Hardknock | 23/30 | 4/30 | 12/15 | 2/3 |

The new agent successfully applied the old validated Lesson on **3/3 observed opportunities**. On the environment update, Hardknock first failed, then tested the old rule, recorded its contradiction, and stopped delivering it. Stateless already chose the newly valid default; it did better on that subset. The reflection baseline retained its stale preference.

These are designed fixture results, not estimates of production reliability or general model improvement. Hardknock spends additional bounded training executions. Recovery rates include unchanged retries in the baselines; normal profile metrics separately count typed Recovery attempts. Missing evidence stays **UNKNOWN**. [Definitions, reproducibility, and limitations](docs/implementation-v06.md).

## Experience Should Have a Curriculum

Human experts encounter edge cases, recover from mistakes, and learn where familiar techniques stop working. Hardknock gives agents that opportunity through explicitly started, bounded trials.

**Production should not be your agent's first teacher.**

**Hardknock turns failure from an incident into a curriculum.**

```text
Known Skill → Find Evidence Gaps → Generate Trials
    → Experience Failure in the Dojo → Learn → Harden Skill
```

For an already registered Skill:

```bash
hardknock curriculum plan --skill deploy-rolling-update --profile resilience-basic --budget 5
hardknock curriculum why <curriculum-id>
hardknock curriculum run <curriculum-id>
hardknock curriculum report <curriculum-id>

# Convenience wrapper: plan + run
hardknock skill harden deploy-rolling-update --profile resilience-basic --budget 5
hardknock skill package deploy-rolling-update
```

`resilience-basic` describes local fixture faults, not real infrastructure outages. A fresh Skill can expose failures without becoming Hardened: tested recoveries and reflex negative controls may require another explicitly started curriculum. All controls and response arms consume the aggregate budget. There is no background scheduler.

**Profile Coverage = observed relevant conditions / configured relevant conditions.** Unknown remains UNKNOWN. `Hardened` means the Skill satisfies the configured Hardknock evidence policy across the tested curriculum. It does not imply correctness under untested conditions.

The deterministic held-out benchmark observes 0/2 successes without experience, 1/2 using Lesson advice, and 2/2 using the full package's tested recovery procedures. This measures two local fixture cases, not autonomous model improvement or production reliability. [Run the complete demo and benchmark](docs/curriculum.md).

## Stop Guessing. Try It.

Most agent systems spend more inference when they are uncertain. Hardknock can spend a bounded experience budget instead: fork disposable realities, try competing strategies, evaluate the results, and return evidence.

```text
Agent:       "I have two plausible migration strategies."
Hardknock:   "Try both."

                 same starting state
                    /         \
              strategy A   strategy B
                 FAIL         PASS
                    \         /
                      evidence
                         ↓
                   agent decides
```

**Hardknock turns uncertainty into controlled experimentation.** After initializing the offline [strategy-choice fixture](docs/agent-experiments.md#run-the-deterministic-demo):

```bash
hardknock try --agent test-agent \
  --candidate 'direct=direct-upgrade' \
  --candidate 'staged=staged-upgrade' --check './test.sh'
```

The fixture returns direct **FAIL**, staged **PASS**, quality **CONTROLLED**, a staged recommendation, two Experiences, and one Candidate Lesson. The source repository is unchanged and trial worktrees are discarded. Export a saved patch with `reality export`; there is no automatic commit. If both candidates pass, there is no invented winner. If agent and strategy both change, the result is **CONFOUNDED**, not a causal lesson.

An integrated agent uses the same command with `--session` or the structured Bridge request. Session/process snapshots are not available: the reply explicitly identifies the recorded-commit fallback. See [budgets](docs/experience-budget.md) and [quality guarantees](docs/experiment-quality.md).

## The problem

Agents can remember failures, reflect on them, and save successful workflows as skills. But a plausible explanation can become a durable lesson before anyone checks whether it is right.

An agent changes a dependency, the build fails, and it remembers: “Never use this package.” Was the package responsible? Or was it the version, the environment, a stale lockfile, or an unrelated flaky test?

Reflection, episodic memory, skill learning, sandboxing, replay, and evaluation are valuable pieces. The missing connection is a disciplined way to turn their observations into experience supported by experiments.

## The Hardknock idea

> **Reflection proposes hypotheses. Experiments provide evidence.**

```text
A common reflective loop
  Failure → Reflection → Memory

The Hardknock loop
  Failure → Reflection → Hypothesis → Controlled Experiment
                                             ↓
                                          Evidence
                                             ↓
                                    Supported Lesson
```

**Reflection → Hypothesis. Not Reflection → Truth.** Reflection proposes an explanation; an execution supplies evidence. Experiments test whether the explanation deserves to influence future work.

Hardknock's purpose is to let agents **generate, validate, accumulate, revise, and eventually retire experience**. A failed trial remains useful evidence even when its proposed lesson turns out to be wrong.

Failure should change the next attempt: evidence informs reflection, tested lessons guide adaptation, and the agent retries with a reason to act differently.

## Run the learning demo

The local fixtures simulate conflicting package-manager state without network access, model calls, or npm/pnpm installations. Initialize fixtures A and B as separate Git repositories using the [demo instructions](docs/experiments.md#run-the-offline-demo), with one shared data home:

```bash
hardknock --repo /path/to/A run --agent test-agent --check './test.sh' \
  --retry-with-experience --max-retries 1 'upgrade demo dependencies'

hardknock --repo /path/to/B run --no-experience --agent test-agent \
  --check './test.sh' 'upgrade service and worker'

hardknock --repo /path/to/B run --agent test-agent \
  --check './test.sh' 'upgrade service and worker'
```

The tested sequence is:

```text
Fixture A · demo package
  First attempt       simulated npm     FAIL
  Counterfactual      simulated npm     FAIL
                      simulated pnpm    PASS
  Lesson              supported         confidence 0.78
  Opt-in retry        Lesson applied    PASS

Fixture B · distinct service/worker packages and task
  Without experience  simulated npm     FAIL · repeated mistake 1
  With experience     simulated pnpm    PASS · repeated mistake 0
  Lesson              VALIDATED         confidence 0.90
```

This is a summary of tested fixture behavior. A produces four immutable Experiences including the retry; B adds a control and an advised run. The original failure and Experiment never change. A's retry exits **0**, B's control **1**, and B's advised run **0**. No source checkout needs resetting: every attempt starts in a fresh Reality. Without the retry flag, A still exits 1 after recording support.

```bash
hardknock experience list
hardknock experience show exp-<uuid>
hardknock lesson list
hardknock lesson show lesson-<uuid>
hardknock --repo /path/to/B lesson search --action 'npm install'
hardknock experiment list
hardknock experiment show experiment-<uuid>
hardknock why --experience exp-<transfer-uuid>
```

For other workflows, use `run --script`, `lesson propose`, and `experiment run --lesson`. Replacement matches the entire recorded script; it does not intercept commands inside an opaque agent. See [CLI usage](docs/cli.md) and [experimental limits](docs/experiments.md#equivalence-and-limits).

**Validated** means Hardknock observed supporting evidence in both a controlled counterfactual and at least one distinct application context. It does not imply universal correctness. Distinctness requires a different repository tree; an identical clone or renamed task cannot boost confidence. Confidence is a heuristic, not a calibrated probability. See the [phase report](docs/implementation-transfer.md) for the checks, contradiction case, and Codex CLI smoke test.

## Don't Wait for Useful Mistakes

Production incidents provide valuable experience, but they are an expensive and incomplete curriculum. Hardknock can deliberately create controlled adversity inside the Dojo to discover how an agent behaves outside nominal conditions.

```text
Skill → Chaos → Observed failure conditions → Lesson → Reflex → Recovery
```

**Hardknock doesn't just teach agents how to succeed. It discovers how their success breaks.**

```bash
hardknock chaos run --fixture retry-resilience \
  --perturb-sweep delay=0,100,500,1000,2000
```

The bundled local fixture observes control PASS, 0/100/500ms PASS, 1000ms DEGRADED, and 2000ms FAIL. It simulates dependency delay; it does not shape network traffic. The result records Experiences, a Candidate Lesson/Reflex/Recovery, and an Operating Envelope containing those tested points. All untested conditions remain unknown.

Use the emitted IDs with `reflex test`, `reflex enable`, and `recovery test`. Tests can support a response but do not activate it. A transient-failure negative case records when the original action would have succeeded and disables the overbroad Reflex. See the [runnable chaos guide](docs/chaos.md), [Reflex rules](docs/reflexes.md), and [recovery protocol](docs/recovery.md).

## How it works

These steps work for the local fixtures and explicit scripts. Generic agents can opt into the [context-file contract](docs/agent-integration.md); their internal action claims remain self-reported.

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

| Concept | Meaning |
| --- | --- |
| **Experience** | What actually happened: the execution record and its evidence. |
| **Skill** | A procedure describing what an agent can do, with evidence of where it has worked. |
| **Reflection** | An interpretation of what happened and why; it proposes hypotheses for testing. |
| **Lesson** | Durable, contextual knowledge proposed from Experience and revised as experiments support or contradict it. |
| **Reflex** | A recognizable precursor to a known failure that can trigger warning, reconsideration, or replanning. Severe cases supported by strong evidence may justify blocking, but only with independent policy authorization. |
| **Recovery** | A scoped restoration procedure with explicit candidate, support, and contradiction evidence. |
| **Perturbation** | A deliberate experimental condition applied inside a Reality. |
| **Chaos Campaign** | A healthy control followed by bounded perturbation trials. |
| **Operating Envelope** | Observed behavior at explicitly tested conditions; all other conditions remain unknown. |

The full target Experience schema captures the following; the implemented subset and deferred fields are documented in [the model reference](docs/experience-model.md):

| Field group | Contents |
| --- | --- |
| Intent and context | Goal, starting state or snapshot reference, environment, agent/model identity and version |
| Execution | Actions, inputs, tool calls, perturbations, predictions |
| Evidence | Observations, command output, filesystem changes, outcome, evaluation and evaluator version |
| Failure and recovery | Surprise or prediction error, failure signatures, recovery behavior and outcome |
| Provenance and uncertainty | Parent trial, experiment, timestamps, artifact references, evidence limitations, confidence and its basis |

Confidence belongs to a specific claim or evaluation, not to every byte in a log. Preserve the execution record; version and revise interpretations of it. “Validated experience” means evidence with conclusions validated under tested conditions, not an infallible account of the world.

Operating envelopes are implemented as sparse tested points. Future derived artifacts could include broader heuristics, invariants, and causal models.

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

**Dojo** is Hardknock's controlled experimentation subsystem. It organizes disposable **Realities** where agents can encounter failures, inspect evidence, form reflections, test candidate lessons, and adapt their behavior.

Repeated execution alone can lose the experience embedded in failed attempts. Dojos preserve that evidence so agents can acquire lessons from controlled failure modes before encountering similar failures in production. The current backend uses trusted local tasks and Git worktrees, subject to the shared-host limits below.

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

Traditional chaos engineering introduces controlled perturbations to discover weaknesses before production incidents do. Hardknock applies this philosophy to deterministic local **agent behavior**: do not merely wait for useful mistakes; create bounded adversity and observe how the agent responds.

> **Hardknock doesn't just teach an agent how to succeed. It discovers how that success breaks.**

The implemented local mechanisms are environment overrides, file mutations, command failures, and command delays. These broader perturbation families remain future work:

| Surface | Examples |
| --- | --- |
| Network and dependencies | Latency, packet loss, an unavailable dependency |
| Identity and permissions | A stale credential, permission denial |
| Tools and APIs | A partial API response, tool failure |
| Workspace and configuration | A conflicting concurrent change, malformed configuration |
| Information and instructions | Outdated documentation, ambiguous instruction, stale memory, misleading external information |

```text
Known Skill → Chaos Trials → Observe Failure Conditions
                                        ↓
                             Lesson + Reflex + Recovery
                                        ↓
                               More Resilient Skill
```

An **operating envelope** records empirically observed success, degradation, or failure at tested conditions. Untested conditions remain unknown; an agent should not assume success will transfer.

Chaos trials should specify the perturbation, control run, success checks, safety limits, and stopping conditions. They belong in disposable environments, not live production. The local subset is implemented in V0.2; real infrastructure chaos remains deferred.

### Experience budget

**Stop guessing. Try it.** When uncertainty is high, spend a bounded experimentation budget on alternatives in disposable Realities instead of spending unlimited tokens imagining outcomes.

Implemented syntax (inside an initialized strategy fixture):

```bash
hardknock try --agent test-agent --budget-realities 3 --budget-duration 5m \
  --candidate 'direct=direct-upgrade' --candidate 'staged=staged-upgrade' \
  --check './test.sh'
```

Budgets cap requested Realities, agent runs, and experiment duration. Optional command ceilings cover explicit shell entries and evaluator processes; opaque native tool-call caps are rejected as unenforceable. Over-budget comparisons are rejected before creating Realities. The existing `run --experience-budget N` still caps additional paired trials/retries. Token and financial cost accounting remain future work. [Budget semantics](docs/experience-budget.md) distinguish hard scheduling caps from advisory Git-provider capabilities.

## Works With Your Agent

```text
Claude Code + Hardknock    native lifecycle hooks
Codex       + Hardknock    App Server structured events
Hermes      + Hardknock    Python plugin hooks
OpenClaw    + Hardknock    typed plugin hooks
```

Hardknock does not replace the agent. It provides controlled experience, lessons supported by evidence, reflexes, recovery knowledge, and a place to experiment within the [documented safety limits](#run-the-current-prototype). A lesson recorded from Codex can be retrieved for Claude: empirical experience is stored independently of the reasoning engine that produced it.

```text
                     AGENTS

       Claude      Codex      Hermes      OpenClaw
          │          │           │           │
          └──────────┴───────────┴───────────┘
                         │
                   Agent Adapters
                         │
                  Hardknock Bridge
                         │
          ┌──────────────┼──────────────┐
          ↓              ↓              ↓
      Experience       Reflex          Dojo
        Engine          Engine         Engine
          └──────────────┼──────────────┘
                         ↓
                     Evidence
```

```bash
hardknock bridge start
hardknock integrate claude install
hardknock integrate codex check
hardknock integrate hermes install
hardknock integrate openclaw install
hardknock integrate doctor
hardknock agent capabilities
hardknock events tail --follow
hardknock bridge stop
```

Configure workspace evaluators before expecting successful task evidence. Installers preserve unrelated settings; Hermes/OpenClaw host enablement and trust remain native user decisions. See the [integration guide and capability matrix](docs/integrations.md), [Bridge protocol](docs/bridge-protocol.md), and [portable contract](docs/agent-experience-contract.md).

**Verification boundary:** offline Codex-shaped failure → controlled experiment → Claude-shaped successful transfer is tested. Real Codex 0.149.1 passes schema/initialization checks; a live model smoke exercised approval cancellation and recorded a non-success outcome. Claude, Hermes, and OpenClaw were not installed for live testing. This is not yet a completed two-agent live demonstration.

| Integration stage | Current support |
| --- | --- |
| Runner | Existing `run --agent-command`, `--script`, and fixture adapters; plugins are optional |
| Experience aware | Context files for generic agents; concise context through native hooks/Bridge |
| Lifecycle integrated | Session, action, result and completion normalization with asynchronous recording |
| Reflex integrated | Cached advice; Codex can pause only at native approval requests, not every item notification |
| Native experimentation | Explicit structured Bridge requests and shared `try --session` helper; fake Claude/Codex request → progress → result tested; live experiments and skill-validation tools remain unverified/deferred |

Learning advice never grants extra permissions or becomes a policy denial. No adapter requests hidden reasoning. Each adapter guide lists exactly what it observes, stores, influences, and cannot see.

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

Implemented resources include `experience`, `skill`, `lesson`, `reflex`, `recovery`, `experiment`, `chaos`, `envelope`, and `reality`, plus `run`, `why`, `status`, and raw `execution list/show`. The tree below also includes the still-planned `try` and general Experience replay commands. See [docs/cli.md](docs/cli.md) for the implemented command set.

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
├── envelope
├── reality
└── why
```

The implemented inspectability command explains recorded Lesson and Reflex influence:

```bash
hardknock why
```

Current explanations follow application → Lesson → Experiment → source Experience, or a historical Reflex match → source Lesson → chaos Trial → Experience. Blocking still requires future independent policy authorization:

```text
Decision → Reflex → Lesson → Chaos Trial → Experience
```

## Target architecture

| Component | Responsibility |
| --- | --- |
| Agent adapters | Launch agents, normalize observable execution events, and expose supported integration capabilities |
| Dojo backends | Create Realities, enforce declared isolation limits, execute trials, and capture diffs and outputs |
| Experiment controller | Define controls and perturbations, schedule bounded trials, and compare evaluations |
| Experience Engine | Store typed evidence, propose lessons, track support and contradiction, and retrieve applicable artifacts |
| Policy and review gates | Keep trial permissions and artifact acceptance explicit; authorize any blocking behavior independently of confidence |

These are conceptual boundaries, not a commitment to separate services. The implementation is one modular Rust crate with SQLite, local artifacts, a generic adapter, and a Git-worktree `RealityProvider`. It includes experiments, immutable evidence, scoped retrieval, retries, and Lesson validation. See [architecture and guarantees](docs/architecture.md).

## V0.1 scope

**Initial focus: coding agents.** Use existing execution primitives and repository-local tasks to test the central idea.

### Dojo: implemented foundation

- Detached Git worktrees, with explicit warnings about shared host access.
- Clean committed starting snapshots and forks from the recorded starting commit.
- Generic process execution, command/output capture, filesystem diffs, and persistent execution records.
- Timeouts, signal handling, optional retention, and explicit orphan cleanup.

**Also implemented:** required command checks and explicit scripted replay with snapshot/environment verification. Broader environment reproducibility and stronger isolation backends such as containers remain planned.

### Implemented Experience Engine foundation

- Typed immutable Experiences with context, failure signatures, and artifact provenance.
- Manual and deterministic Candidate Hypotheses and scoped, versioned Lessons.
- Explicit baseline/alternative scripts in fresh Realities with equivalence checks.
- Centralized support/contradiction classification and heuristic confidence.
- Candidate → CounterfactuallySupported → Validated transitions, contradiction and explicit retirement.
- Deterministic scoped retrieval, context injection, observable application and retry lineage.
- Bounded opt-in retry and a distinct transfer fixture with an experience-disabled control.

**V0.2 also implemented:** manually registered supported Skills, healthy-control campaigns, reversible local perturbations, observed operating points, Candidate/Supported/Active Reflexes, false-positive detection, and bounded recovery tests. See [the resilience report](docs/implementation-v02.md). **V0.3 preview:** the common Bridge and four native adapters are implemented with fixture coverage; [live acceptance remains pending](docs/implementation-v03.md). No MCP server is included.

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

The deterministic transfer comparison measures success **0/1 → 1/1** and repeated mistakes **1/1 → 0/1** for fixture B. This is one designed local failure class, not a general agent benchmark. A benchmark CLI and broader evaluations remain deferred.

## Technical principles

| Principle | Commitment |
| --- | --- |
| **Evidence over introspection** | LLM reflection generates hypotheses, not facts. |
| **Safe failure is useful** | Failure within a controlled boundary is a source of information. |
| **Context matters** | Lessons are conditional beliefs with scope and confidence, not global prohibitions. |
| **Experience is inspectable** | Every durable conclusion should trace back to observable evidence. |
| **Interpretations are revisable** | Lessons can be weakened, contradicted, retested, or retired. |
| **Agents are interchangeable** | Evidence should survive changes in the reasoning model or agent implementation; applicability must be checked again. |
| **Failure should improve resilience** | Measure recognition, recovery, and operating judgment as well as completion. |

## Roadmap

The broader release goals below include work beyond the implemented local loop. No release dates are committed; see the [implementation roadmap](docs/roadmap.md) for exact status.

| Milestone | Intended outcome |
| --- | --- |
| **V0.1 — Controlled coding experiments** | Runner adapters, disposable coding trials, typed evidence, counterfactual lesson validation, retrieval, and retry |
| **V0.2 — Local resilience** | Deterministic chaos, observed operating envelopes, scoped reflexes, false positives, and recovery |
| **V0.3 — External-agent integration** | Authenticated Bridge, lifecycle adapters, cached advice, provenance; live acceptance still pending |
| **V0.4 — Agent-Native Experimentation** | Implemented for trusted local alternatives; bounded requests, quality, evidence, replay and cancellation |
| **V0.5 — Skill Hardening and Autonomous Curriculum** | Implemented locally: evidence-gap plans, bounded trials, coverage/maturity, packages, held-out benchmark |
| **V0.6 — Persistent Agent Development** | Implemented locally: profiles, snapshots, revisions, freshness, revalidation, longitudinal and model-transfer fixtures |
| **V0.7 — Experience Federation and Team Learning** | Next: explicit sharing, trust review, provenance and conflict handling; no automatic federation yet |
| **Later — External-effect virtualization** | Explore explicit semantics for selected external systems, without claiming universal rollback |

## Project status

Hardknock is a **pre-alpha empirical learning prototype**. The retrieval, retry, transfer-validation, and local resilience loops are implemented and tested. It builds from source; no release package is published. CI is configured for Linux and macOS; local verification was on macOS.

The fixtures demonstrate limited transfer from one task to a related, distinct repository. They do not establish general agent performance or universal causal claims. APIs, command syntax, and schemas remain subject to change.

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
