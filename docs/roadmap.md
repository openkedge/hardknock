# Hardknock Roadmap

## From Safe Failure to Durable, Executable Operational Knowledge

_Last updated: September 7, 2026 · Current local implementation: V0.17 · Next planned product increment: V0.18_

## Status and reading guide

This roadmap is both a delivered-history map and the forward architecture for
Hardknock. Status language is deliberately conservative:

- **Implemented locally** means code, deterministic fixtures, and default
  model-free tests exist in this repository.
- **Architecture refinement** means the roadmap standardizes a concept that may
  not yet exist under the canonical name in every earlier implementation.
- **Planned** means no completion claim is made.
- **Live acceptance pending** means local behavior exists but still needs
  provider-, platform-, or installed-agent evidence.

| Horizon | Status | Focus |
| --- | --- | --- |
| V0.1–V0.17 | Implemented locally; architecture refinement continues | Experience capture through safe abstraction and held-out transfer |
| V0.18 | Planned next | Hierarchical operational knowledge and precedence semantics |
| V0.19–V0.20 | Planned | Composition and long-horizon experience |
| V0.21+ | Directional | Organizational, continuous, and production Hardknock |
| Runtime/provider acceptance | Ongoing in parallel | Containers, PostgreSQL, installed agents, and provider measurements |

The current shipped boundary is documented in the
[V0.17 implementation report](implementation-v017.md), with operating semantics
in the [experience abstraction guide](experience-abstraction.md). Local implementation is
not evidence of universal production safety, and aspirational schemas below
must not be read as already shipped APIs.



## 1. Updated Thesis

Hardknock should no longer be framed primarily as:

> **A place where agents safely fail and learn lessons.**

The stronger thesis is:

> **Hardknock turns controlled experience—including failure—into durable, evidence-backed operational knowledge that changes future agent behavior.**

The resulting knowledge includes both:

```text
Positive Knowledge
  → what works
  → Skills
  → Procedures

Negative Knowledge
  → what must not happen
  → Constraints
  → Invariants
  → Preconditions
  → AntiPatterns
```

plus:

```text
Recovery Knowledge
  → what to do when failure still occurs
```

The central system loop becomes:

```text
                         EXPERIENCE
                             │
                             ↓
                   Outcome Classification
                             │
                             ↓
                         Reflection
                             │
                             ↓
                  Candidate Explanations
                             │
                             ↓
                    Knowledge Extraction
                             │
          ┌──────────────────┼──────────────────┐
          ↓                  ↓                  ↓
        Lesson             Skill            Constraint
                                                  │
                             ┌────────────────────┼────────────────┐
                             ↓                    ↓                ↓
                         Precondition          Invariant       AntiPattern

                             +
                          Recovery
                             │
                             ↓
                 Counterexample / Replay
                             │
                             ↓
                    Empirical Validation
                             │
                             ↓
                   Knowledge Promotion
                             │
                             ↓
                    Durable Knowledge
                             │
                  ┌──────────┴──────────┐
                  ↓                     ↓
            Agent Guidance        Guard Candidate
                                        │
                                        ↓
                                    OpenKedge
```

The architectural relationship should be:

> **Hardknock learns empirically. OpenKedge enforces deterministically.**

---

# 2. Canonical Knowledge Model

The roadmap should standardize six primary objects.

## Experience

**What happened?**

Raw evidence from an encounter:

```text
goal
context
starting state
actions
perturbations
observations
outcome
surprise
uncertainty
provenance
```

Experience is immutable historical evidence.

---

## Reflection

**What might this experience mean?**

Reflection is interpretation.

It may contain:

```text
suspected assumptions
possible causes
unexpected observations
alternative strategies
candidate implications
```

Reflection is explicitly fallible.

It is not durable operational truth.

---

## Lesson

**What should be remembered next time?**

A Lesson is generalized knowledge:

```text
statement
scope
evidence
confidence
applicability
contradictions
provenance
freshness
```

Example:

> Reconcile authoritative state before retrying a mutation after an ambiguous external outcome.

---

## Skill

**What should the agent know how to do?**

A production-grade Hardknock Skill should eventually contain:

```text
procedure
preconditions
required observations
capabilities
operating envelope
known failure modes
constraints
recovery
evidence
```

A Skill is therefore much richer than a `SKILL.md`.

---

## Constraint

**What must not happen, or what must hold?**

This becomes the canonical negative-knowledge primitive.

Recommended structure:

```text
Constraint
├── Precondition
├── Invariant
└── AntiPattern
```

### Precondition

Something that must be true before an action.

### Invariant

Something that must remain true across execution.

### AntiPattern

A recognizable action/state sequence empirically associated with unsafe or ineffective behavior.

Example:

```text
mutation timeout
↓
retry without reconciliation
↓
possible duplicate or stale mutation
```

---

## Recovery

**What should happen after failure occurs?**

Recovery should include:

```text
failure signature
containment
reconciliation
restoration
verification
scope
evidence
```

The complete operational loop becomes:

```text
Detection
   ↓
Prevention if possible
   ↓
Containment
   ↓
Recovery
   ↓
Reconciliation
   ↓
Prevention refinement
```

---

# 3. Unified Knowledge Lifecycle

All durable knowledge artifacts should use a governed lifecycle.

Recommended canonical progression:

```text
OBSERVED
   ↓
CANDIDATE
   ↓
REPRODUCED
   ↓
SUPPORTED
   ↓
VALIDATED
   ↓
HARDENED
```

And lifecycle exits:

```text
HARDENED
   ↓
STALE
   ↓
REVALIDATION
```

or:

```text
SUPPORTED / VALIDATED
   ↓
CONTRADICTED
   ↓
WEAKENED
   ↓
RETIRED
```

Important principle:

> **Learning is a governed state transition, not prompt append.**

A single failure should never silently become permanent agent doctrine.

---

# 4. Every Knowledge Artifact Requires Scope

Use the general model:

```text
Knowledge
+
Applicability Predicate
+
Evidence
+
Provenance
```

Scope may include:

```text
environment
system
repository
resource type
action type
TaskFamily
failure mode
tool
software version
dependency version
agent/runtime
risk class
```

Therefore:

```text
"Never retry PUT"
```

is unacceptable.

But:

```text
After an ambiguous timeout on a non-idempotent mutation,
reconcile authoritative operation state before retrying.
```

may become supported under a defined scope.

This prevents negative transfer.

---

# 5. Provenance Is Mandatory

Every promoted artifact should answer:

```text
Where did this knowledge come from?

Which Experiences support it?

Which failures contradict it?

Which Experiments tested it?

Which agent/model/runtime contributed?

Which evaluator judged the outcome?

Which software/environment versions were involved?

When was it last validated?
```

Hardknock should accumulate:

> **evidence-backed experience**

rather than operational folklore.

---

# PHASE I — BUILD THE KNOWLEDGE CORE

# V0.1 — Reality, Execution, Evaluation, Experience

## Goal

Capture structured encounters.

```text
Agent
 ↓
Reality
 ↓
Execution
 ↓
Evaluation
 ↓
Experience
```

### Core

* Reality abstraction
* Git-worktree Reality
* agent adapter
* deterministic evaluators
* Experience schema
* artifact/provenance storage
* CLI
* deterministic fixtures

### Question answered

> What happened?

---

# V0.2 — Structured Knowledge From Experience

This phase should be expanded substantially from the old roadmap.

## Goal

Introduce the canonical knowledge architecture immediately.

```text
Experience
   ↓
Reflection
   ↓
Knowledge Candidates
```

### First-class objects

```text
Reflection
Lesson
Skill
Constraint
Recovery
```

Constraint subtypes:

```text
Precondition
Invariant
AntiPattern
```

Also introduce:

* Failure Signature
* structured scope
* provenance
* maturity lifecycle
* contradiction records
* applicability predicates
* relations between knowledge artifacts

### Important separation

```text
Reflection ≠ Lesson

Lesson ≠ Invariant

Invariant ≠ Guard

Recovery ≠ Skill
```

This should become foundational rather than something introduced deep in the roadmap.

---

# V0.3 — Agent Integrations and Knowledge Application

## Goal

Connect:

```text
Claude
Codex
Hermes
OpenClaw
```

through the common Hardknock Bridge.

### Runtime context should include

```text
Relevant Skills
Relevant Lessons
Applicable Constraints
Relevant Recoveries
Known Unknowns
```

### Important

Constraints at this stage remain primarily:

```text
Advice
Warning
Replan signal
```

Hard enforcement remains separate.

### Signature

> **Models change. Experience survives.**

---

# PHASE II — MAKE KNOWLEDGE EMPIRICAL

# V0.4 — Agent-Native Experimentation

## Goal

Turn uncertainty into controlled experiments.

> **Stop guessing. Try it.**

```text
Question
 ↓
ExperimentRequest
 ↓
Equivalent Starting State
 ↓
Reality A / Reality B
 ↓
Evaluation
 ↓
Evidence
```

### Adds

* controlled candidate comparison
* counterfactual pairs
* experiment quality
* Reality lineage
* evidence relationships
* ExperienceBudget

This is where candidate knowledge begins earning empirical support.

---

# V0.5 — Dojos, Curriculum, Counterexamples, and Skill Hardening

This phase should absorb several ideas from the new architecture.

## Goal

Actively attack Hardknock's own learned knowledge.

```text
Candidate Knowledge
       ↓
Counterexample Search
       ↓
Scenario Mutation
       ↓
Replay / Chaos
       ↓
Support or Contradiction
       ↓
Scope Refinement
```

### Curriculum targets

Not only Skills, but:

```text
Skill
Lesson
Constraint
Invariant
AntiPattern
Recovery
Reflex
Known Unknown
```

### Dojo taxonomy

Retain technology Dojos:

```text
Kubernetes
PostgreSQL
Git
cloud
```

but add **misconception Dojos**:

```text
Stale World
Partial Success
Retry Hazard
Correlated Failure
Hidden Dependency
Authority Drift
Irreversibility
Conflicting Evidence
Delayed Consequence
Local-vs-Global Optimum
```

This should become a distinctive Hardknock concept.

### Signature

> **Production should not be your agent's first teacher.**

---

# PHASE III — PERSISTENT KNOWLEDGE

# V0.6 — Persistent Development, Revalidation, and Forgetting

## Goal

Ask:

> Is the system actually becoming better over time?

### Adds

* Experience Profile
* knowledge revisions
* longitudinal metrics
* development episodes
* evidence freshness
* contradiction management
* revalidation queues
* active vs archival knowledge
* profile rebuild

### Crucial new emphasis

Forgetting becomes first-class:

```text
Learn
 ↓
Validate
 ↓
Use
 ↓
Revalidate
 ↓
Weaken
 ↓
Supersede / Retire
```

Hardknock should retain raw evidence while allowing operational knowledge to disappear from active guidance.

### Major benchmark

The **stale-rule / superstition test** remains essential.

A system that remembers every old rule forever is not learning well.

---

# V0.7 — Portable Experience, Trust, and Federation

## Goal

Transfer knowledge without transferring unquestioned trust.

```text
Experience Package
 ↓
Integrity / Provenance
 ↓
Compatibility
 ↓
Quarantine
 ↓
Local Reproduction
 ↓
Promotion
```

### Package can contain

```text
Skills
Lessons
Constraints
Recoveries
Operating Envelopes
Evidence summaries
Known Unknowns
```

Imported Constraints must **never** automatically become hard guards.

### Signature

> **Experience is portable. Trust is local.**

---

# PHASE IV — SAFE REALITY INTERACTION

# V0.8 — Transactional Effects

## Goal

Separate speculation from authoritative mutation.

```text
Proposed Effect
 ↓
Prepare
 ↓
Experiment / Validate
 ↓
Commit
```

### Adds

* Effect
* Prepare/Commit
* receipts
* reconciliation
* compensation
* stale commit detection
* idempotency
* partial commit semantics

### Signature

> **Agents may speculate freely. Reality changes only through an explicit effect boundary.**

---

# V0.9 — Capability-Isolated Execution

## Goal

Prevent agents from bypassing the Effect boundary.

### Adds

* Container Reality
* filesystem capability
* network capability
* credential isolation
* no ambient host credentials
* execution proxy
* Credential Broker
* capability tokens

### Signature

> **A guardrail you can bypass is not a boundary.**

---

# V0.10 — Tool-Level Micro-Sandboxes

## Goal

Apply least authority to each action.

```text
Agent
 ↓
Tool
 ↓
Capability Intersection
 ↓
Micro-Sandbox
 ↓
Execution
 ↓
Attestation
```

### Adds

* ToolDefinition
* capability Tool manifests
* tool integrity
* per-tool credentials
* execution attestations
* WASI/container execution
* capability-minimization curriculum

### Signature

> **Least privilege should apply to actions, not just agents.**

---

# PHASE V — FROM KNOWLEDGE TO ASSURANCE AND GOVERNANCE

# V0.11 — Behavioral Contracts, Evidence Assurance, and Guard Candidates

This phase should now explicitly connect the new knowledge architecture to OpenKedge.

## Goal

Answer:

> What has this Skill or Constraint actually been shown to preserve?

### Behavioral contract

```text
Skill
+
Preconditions
+
Invariants
+
Forbidden Outcomes
+
Capability Envelope
+
Recovery
+
Evidence Manifest
```

### Empirical assurance

Support:

```text
Behavior
Resilience
Recovery
Constraint compliance
Capability discipline
Effect discipline
Evidence freshness
```

Do not compress these into one score.

---

## Guard Candidate Compilation

A validated Constraint or Invariant may produce:

```text
GuardCandidate
```

Example:

```text
Constraint

healthy_replicas_after(action)
>=
minimum_quorum
```

can compile toward:

```text
OpenKedge Guard Candidate
```

But Hardknock should **not** decide on its own that this must become a production deny rule.

The lifecycle should be:

```text
Experience
 ↓
Constraint Candidate
 ↓
Counterexample Validation
 ↓
Validated Invariant
 ↓
Guard Candidate
 ↓
Governance Review / Policy
 ↓
OpenKedge Enforcement
```

This distinction is critical:

> **Evidence establishes the rule's empirical support. Authority determines whether and how it is enforced.**

---

# 6. Progressive Enforcement

Use an enforcement ladder:

```text
Hint
 ↓
Recommendation
 ↓
Warning
 ↓
Require Verification
 ↓
Require Approval
 ↓
Guard Candidate
```

Then governance may convert an approved Guard Candidate into:

```text
Hard Deny
```

Do not make:

```text
confidence > threshold
```

automatically create a hard production prohibition.

Confidence is evidence.

Enforcement is authority.

---

# PHASE VI — USE KNOWLEDGE LIVE

# V0.12 — Adaptive Runtime

## Goal

Use accumulated operational knowledge while the agent works.

Runtime outcomes:

```text
ACT
EXPERIMENT
REPLAN
RECOVER
REQUIRE APPROVAL
ABSTAIN
```

### Inputs

```text
Skill
Lesson
Constraint
Recovery
Operating Envelope
Assurance
Risk
Authority
Known Unknowns
```

### Constraint behavior

Example:

```text
Constraint matched
+
evidence Supported
→ WARN / REPLAN

Constraint validated
+
high-risk action
→ REQUIRE VERIFICATION

Approved OpenKedge Guard
→ BLOCK
```

This cleanly separates learning from enforcement.

### Signature

> **Hardknock should know when experience is strong enough to act—and when it is not.**

---

# PHASE VII — MAKE THE KNOWLEDGE SCIENTIFIC

# V0.13 — Epistemic Diversity and Correlated Faults

## Goal

Avoid mistaking repeated belief for independent evidence.

```text
5 agents agree
```

may still mean:

```text
1 bad Lesson
repeated five times
```

### Adds

* Claim
* Evidence Path
* Epistemic Dependency Graph
* fault domains
* diversity assessment
* federation echo detection
* experience blast radius
* quarantine
* challenge planning

### Important extension

Hardknock's own:

```text
Lesson
Constraint
Invariant
```

must appear as epistemic dependencies.

A bad Constraint propagated to five agents is a common-mode failure source.

### Signature

> **Count evidence paths, not agents.**

---

# V0.14 — Causal Experience and Constraint Refinement

## Goal

Prevent correlations from hardening into bad Lessons or Constraints.

Correct conceptual sequence:

```text
Observation
 ↓
Reflection
 ↓
Candidate Causal Hypothesis
 ↓
Intervention
 ↓
Counterfactual Evidence
 ↓
Supported / Contradicted Mechanism
```

Do **not** label the pre-experiment stage simply “causal diagnosis.”

### Adds

* CausalHypothesis
* CausalVariable
* Intervention
* confounders
* hypothesis discrimination
* Contextual Causal Model
* mechanism-guided Recovery

### Major new output

Causal evidence can refine negative knowledge.

Before:

```text
Never retry under high latency.
```

After intervention:

```text
After an ambiguous mutation timeout,
do not retry from an unreconciled state.
```

This directly lowers **False Constraint Rate**.

### Signature

> **A postmortem is a hypothesis. A controlled intervention is evidence.**

---

# V0.15 — Predictive Experience and Prevention

## Goal

Learn the shape of failure before the endpoint.

```text
Past

timeout
 ↓
stale retry
 ↓
FAIL


Future

timeout
 ↓
retry proposed
 ↓
Predictive AntiPattern matched
 ↓
refresh state
 ↓
PASS
```

### Adds

* Execution Trajectory
* Early Warning Signature
* Failure Forecast
* Preventive Intervention
* intervention windows
* predictive Reflexes
* precision/recall
* forecast lead

### Relationship to negative knowledge

AntiPatterns become especially valuable here.

An AntiPattern is no longer merely:

```text
something bad that happened
```

but can become:

```text
an executable recognizable precursor sequence
```

### Signature

> **Hardknock learns not only what failure looks like, but what it looks like on the way there.**

---

# PHASE VIII — IMPROVE HOW HARDKNOCK LEARNS

# V0.16 — Experience Economics

## Goal

Answer:

> Which unknown is worth spending the next experiment on?

### Inputs

```text
stale knowledge
contradictions
missing Recoveries
constraint uncertainty
false constraints
causal uncertainty
forecast misses
assurance gaps
runtime abstentions
```

### Adds

* Experience Opportunity
* Value Vector
* Experiment Cost
* Evidence Saturation
* Experience Portfolio
* Experience Debt
* adaptive replanning
* experiment early stopping

### Important new priority

`FalseConstraintRisk` should become a first-class opportunity reason.

A Constraint that blocks legitimate work across thousands of tasks may deserve faster investigation than a narrow failure.

### Signature

> **The scarce resource is not memory. It is useful experience.**

---

# V0.17 — Experience Abstraction and Safe Generalization

**Status: implemented locally in V0.17.** See the
[experience abstraction guide](experience-abstraction.md) and
[V0.17 implementation report](implementation-v017.md). Provider-scale and live
agent acceptance remain ongoing; the roadmap below preserves the architectural
intent and boundary for the delivered implementation.

## Problem

Specific lessons may reveal a reusable operational principle.

Examples:

```text
Git
  refetch remote state after ambiguous push

Database
  reread authoritative row after uncertain write

Cloud
  reread resource after timed-out mutation

Deployment
  reconcile generation after ambiguous rollout
```

Potential abstraction:

> **After an operation with an uncertain external outcome, re-establish authoritative state before performing another state-dependent mutation.**

### Goal

```text
Specific Lessons
Specific Constraints
Specific Recoveries
       ↓
Candidate Experience Pattern
       ↓
Generalization Hypothesis
       ↓
Held-Out Context Tests
       ↓
Counterexamples
       ↓
Supported Abstraction
       ↓
Specializations / Exceptions
```

### Adds

* ExperiencePattern
* AbstractLesson
* AbstractConstraint
* TransferHypothesis
* GeneralizationBoundary
* Specialization
* Exception
* TransferEvidence

### Core principle

> **Generalize only as far as transfer evidence permits.**

---

# PHASE IX — KNOWLEDGE COMPOSITION

# V0.18 — Hierarchical Operational Knowledge

## Goal

Build a structured hierarchy rather than a flat Lesson database.

Example:

```text
Authoritative-State Principle
│
├── Ambiguous Write
│   ├── Database
│   ├── Cloud API
│   └── Deployment
│
├── Retry Hazard
│
└── Stale Snapshot Mutation
```

### Requirements

More-specific knowledge must override broader knowledge when supported.

Support:

```text
general rule
specialization
exception
scope split
contradiction
```

The architecture becomes closer to a real operational knowledge system.

---

# V0.19 — Skill and Constraint Composition

## Goal

Understand whether individually safe components remain safe when combined.

Example:

```text
Skill A
  rotate credentials

Skill B
  restart service

Skill C
  deployment rollback
```

Each works alone.

Sequence:

```text
rotate
→ restart
→ rollback
```

may fail.

### Adds

* Skill composition
* Constraint composition
* sequence invariants
* interaction AntiPatterns
* composition Recovery
* composition assurance

### Signature

> **Validated components do not imply a validated composition.**

---

# V0.20 — Long-Horizon Experience

## Goal

Extend Hardknock from actions and short tasks to long-running plans.

### Adds

* Plan Trajectory
* plan checkpoints
* commitment points
* cumulative assumptions
* plan drift
* stale-plan detection
* long-horizon AntiPatterns
* subgoal Recovery
* replanning boundaries

### Question

> Can Hardknock recognize that the assumptions supporting the plan have stopped being true?

---

# PHASE X — ORGANIZATIONAL EXPERIENCE

# V0.21 — Experience-Guided Agent Teams

Use agents with distinct:

```text
roles
experience
capabilities
epistemic dependencies
```

rather than generic swarms.

Example:

```text
Planner
Investigator
Executor
Reviewer
Recovery Agent
```

Hardknock coordinates evidence and experience across them.

---

# V0.22 — Distributed Experience Control Plane

Support multiple nodes:

```text
developer
CI
staging
production shadow
team environments
```

sharing:

```text
Experience
Lessons
Constraints
Recoveries
Warnings
Certifications
Contradictions
Retirements
```

while preserving:

```text
local trust
local authority
local validation
```

---

# V0.23 — Governed Continuous Learning

Only here introduce persistent autonomous experience acquisition.

```text
Runtime
 ↓
Experience Debt
 ↓
Portfolio
 ↓
Approved Dojo
 ↓
Bounded Learning Job
 ↓
Evidence
```

Never become an uncontrolled production chaos daemon.

---

# V0.24+ — Production Hardening

Later tracks can include:

```text
stronger microVM isolation
production Effect Adapters
organizational governance
enterprise policy
distributed evidence registry
visual experience observability
```

These should follow proof of the learning architecture rather than precede it.

---

# 7. Revised Measurement Framework

The architecture update means Hardknock should explicitly measure **learning quality**, not merely task execution.

The primary longitudinal metrics should become:

### Repeat Failure Rate

```math
RFR =
P(\text{same failure recurs after applicable knowledge exists})
```

Lower is better.

### Generalized Avoidance

Does learned knowledge prevent related held-out failures?

### False Constraint Rate

```math
FCR =
\frac{\text{valid actions incorrectly discouraged or prevented}}
{\text{evaluated constraint applications}}
```

This is crucial now that negative knowledge is first-class.

### Recovery Success Rate

Does the agent restore a valid state after known failure?

### Time to Recovery

Does accumulated experience shorten recovery?

### Experience Transfer Rate

Does validated knowledge help in a related new context?

### Knowledge Retention

Does applicable knowledge remain operationally useful over long horizons?

### Contradiction Resolution Rate

Does the system revise or retire knowledge after strong counterevidence?

### Stale Knowledge Application Rate

How often does obsolete knowledge still influence behavior?

### Repeat Experiment Rate

How often does Hardknock unnecessarily rediscover already mature knowledge?

---

# 8. Revised Definition of a Hardknock Skill

A production Skill should eventually be represented approximately as:

```text
Skill
├── Procedure
├── Applicability
├── Preconditions
├── Required Observations
├── Capabilities
├── Constraints
│   ├── Invariants
│   └── AntiPatterns
├── Operating Envelope
├── Known Failure Modes
├── Reflexes
├── Recovery
├── Evidence
├── Provenance
├── Freshness
└── Assurance
```

This is a substantially stronger concept than a prompt-based Skill.

---

# 9. Revised OpenKedge Relationship

The architecture should be very explicit here.

```text
                     HARDKNOCK

                  Controlled Experience
                           ↓
                       Reflection
                           ↓
                    Lesson / Skill
                           ↓
                       Constraint
                           ↓
                Counterexample Search
                           ↓
                  Validated Invariant
                           ↓
                     Guard Candidate
                           │
                           │ evidence
                           ↓
                     OPENKEDGE

                    Authority Policy
                           ↓
                    Guard Approval
                           ↓
                      Enforcement
                           ↓
                    Evidence Receipt
                           │
                           └──────────────→ Hardknock Experience
```

This creates a powerful closed loop:

```text
Hardknock
  learns what appears unsafe

OpenKedge
  decides what is forbidden

Execution
  produces new evidence

Hardknock
  learns again
```

The separation should remain:

```text
Hardknock
  epistemic authority

OpenKedge
  execution authority
```

More precisely:

> **Hardknock establishes empirical support for operational knowledge. OpenKedge establishes authority over real-world mutation.**

---

# 10. Updated Release Eras

For external communication, stop presenting 20+ versions individually.

Group them into product eras.

## Hardknock 0.x — Experience Engine

```text
V0.1–V0.5
```

Delivers:

```text
Experience
Reflection
Lesson
Skill
Constraint
AntiPattern
Invariant
Recovery
Experiment
Chaos
Curriculum
```

Message:

> **Let agents fail here, not in production.**

---

## Hardknock 1.x — Persistent Operational Knowledge

```text
V0.6–V0.7
```

Delivers:

```text
Experience Profiles
knowledge lifecycle
forgetting
revalidation
federation
portable experience
```

Message:

> **Models change. Experience survives.**

A compelling longitudinal benchmark should be the qualification gate for calling this `1.0`.

---

## Hardknock 2.x — Safe Experimental Runtime

```text
V0.8–V0.10
```

Delivers:

```text
Transactional Effects
Capability-Isolated Reality
Micro-Sandbox Tools
Execution Attestation
```

Message:

> **Reason broadly. Act narrowly.**

---

## Hardknock 3.x — Operational Assurance

```text
V0.11–V0.12
```

Delivers:

```text
Behavioral Contracts
Evidence Manifests
Guard Candidates
Certification
Adaptive Runtime
Abstention
```

Message:

> **A Skill should carry its evidence.**

---

## Hardknock 4.x — Scientific Experience

```text
V0.13–V0.17
```

Delivers:

```text
Epistemic Diversity
Causal Experience
Predictive Failure Prevention
Experience Economics
Safe Generalization
```

Message:

> **Don't just remember what happened. Learn what survives experiment.**

---

# 11. Revised Immediate Roadmap

The practical sequence is now:

```text
V0.1   Experience foundation

V0.2   Canonical knowledge model
       Reflection
       Lesson
       Skill
       Constraint
       AntiPattern
       Invariant
       Recovery

V0.3   Agent integrations + knowledge application

V0.4   Counterfactual experimentation

V0.5   Dojos + curriculum + counterexample validation

V0.6   Persistent development + forgetting

V0.7   Portable experience + trust

V0.8   Transactional external effects

V0.9   Capability-isolated execution

V0.10  Per-tool micro-sandboxes + attestations

V0.11  Behavioral assurance + Guard Candidates

V0.12  Adaptive runtime

V0.13  Epistemic diversity / common-mode learning faults

V0.14  Causal experience

V0.15  Failure forecasting + prevention

V0.16  Experience economics

V0.17  Safe abstraction + transfer

V0.18  Hierarchical operational knowledge

V0.19  Skill/Constraint composition

V0.20  Long-horizon experience

V0.21+ Organizational / continuous / production Hardknock
```

---

# 12. What Changed From the Previous Roadmap

The architecture update causes five important changes.

### 1. Negative knowledge moves to the foundation

Previously Lessons/Reflexes gradually carried much of this responsibility.

Now:

```text
Constraint
Invariant
Precondition
AntiPattern
```

become explicit from V0.2.

This is the biggest change.

### 2. Counterexample search moves earlier

Counterexample testing should not wait for advanced causal learning.

Basic:

```text
Does this Lesson overgeneralize?
```

belongs in V0.5.

Advanced:

```text
Which variable is actually causal?
```

remains V0.14.

### 3. Forgetting becomes part of persistence

V0.6 must explicitly cover:

```text
staleness
supersession
retirement
reactivation
```

rather than treating persistent learning as only accumulation.

### 4. Hardknock → OpenKedge becomes an explicit compilation boundary

The architecture should support:

```text
validated Constraint
→ Guard Candidate
```

but never:

```text
Lesson confidence
→ automatic production deny
```

### 5. Metrics must include negative transfer

With first-class Constraints, a system can become worse by learning too many prohibitions.

Therefore:

```text
False Constraint Rate
```

becomes as important as:

```text
Repeat Failure Rate
```

This guards against building an agent that “learns” by becoming afraid to do anything.

---

# 13. The New Core Hardknock Flywheel

The overall architecture can now be reduced to one diagram:

```text
                          EXPERIENCE
                              │
                              ↓
                       WHAT HAPPENED?
                              │
                              ↓
                         REFLECTION
                              │
                              ↓
                       WHAT MIGHT IT MEAN?
                              │
                              ↓
               ┌──────────────┼──────────────┐
               ↓              ↓              ↓
             SKILL          LESSON        CONSTRAINT
                                                │
                                     ┌──────────┼──────────┐
                                     ↓          ↓          ↓
                                PRECONDITION INVARIANT ANTIPATTERN

                              +
                           RECOVERY
                              │
                              ↓
                         EXPERIMENT
                              │
                              ↓
                 COUNTEREXAMPLES / CHAOS
                              │
                              ↓
                         CAUSAL TEST
                              │
                              ↓
                   EMPIRICAL VALIDATION
                              │
                              ↓
                         PROMOTION
                              │
           ┌──────────────────┼────────────────────┐
           ↓                  ↓                    ↓
      AGENT GUIDANCE     SAFETY ENVELOPE      GUARD CANDIDATE
           │                  │                    │
           │                  │                    ↓
           │                  │                OPENKEDGE
           │                  │                    │
           └──────────────────┼────────────────────┘
                              ↓
                       LIVE EXECUTION
                              │
                              ↓
                         NEW EXPERIENCE
```

---

# 14. Ultimate Category

I would update the long-term category slightly.

Earlier:

> **Agent Experience Infrastructure**

remains an excellent category.

But the complete system is becoming:

# **The Experience Control Plane for Autonomous Agents**

because it eventually governs:

```text
what the system has experienced
what it believes it learned
what evidence supports that belief
where the knowledge applies
what it should avoid
what invariants should hold
how to recover
when knowledge is stale
when to experiment
when to abstain
when a learned constraint deserves governance
```

The shortest technical description becomes:

> **Hardknock converts controlled experience into scoped, evidence-backed Skills, Constraints, Invariants, AntiPatterns, and Recoveries—and continuously tests whether that knowledge still deserves to influence future agents.**

The corresponding OpenKedge relationship becomes:

> **Hardknock learns what reality teaches. OpenKedge decides what reality permits.**
