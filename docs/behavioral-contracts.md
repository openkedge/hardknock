# Behavioral Contracts

A Behavioral Contract names what one Skill, Tool, Recovery, or Effect Plan is
expected to preserve. V0.11 prioritizes Skills and Tools. Contracts are data,
not prompts: evaluator checks, state predicates, effect predicates, capability
predicates, invariants, and forbidden-outcome detectors are evaluated without
an LLM.

## Project contract file

Store project contracts under `.hardknock/contracts/*.toml`:

```toml
schema = "hardknock.contract.v1"
name = "deploy-rolling-update"
version = "1"

[evaluation_requirements]
evaluators = ["hardknock.outcome"]
observable_state_paths = ["deployment.healthy_replicas"]
effects_observable = true
capabilities_observable = true

[[postconditions]]
type = "evaluator-check"
evaluator = "hardknock.outcome"
expression = "success"

[[invariants]]
description = "At least two replicas remain healthy"
severity = "high"
phases = ["during_execution", "after_execution"]
type = "state-predicate"
path = "deployment.healthy_replicas"
operator = "greater-than-or-equal"
value = 2

[[forbidden_outcomes]]
description = "A losing experiment committed an external effect"
severity = "critical"
type = "effect-predicate"
predicate = { kind = "experimental_effect_leak" }
```

Validate without persistence, then explicitly accept and bind a revision:

```bash
hardknock contract validate .hardknock/contracts/deploy.toml
hardknock contract register .hardknock/contracts/deploy.toml --skill deploy
```

Registration is append-only. Registering the same name and subject creates the
next immutable revision. Existing certificates continue to name their old
contract revision. `contract diff` reports added and removed condition
fingerprints and warns about possible weakening.

## Evaluation

Precondition failure yields `not_applicable`. A violated postcondition,
invariant, or detected forbidden outcome yields `violated`. Missing state,
effect, capability, evaluator, or custom evidence yields `inconclusive`.
Inconclusive is never converted to satisfied.

Contract observability is declared through `evaluation_requirements` and is
checked before certification. An unobservable required clause is an assurance
gap and prevents eligibility. This declaration says Hardknock has a supported
observation path; it does not create evidence by itself.

Contract proposals from an agent or model are not authoritative. V0.11 accepts
manual/project contracts only; future generated proposals must remain
candidates until explicitly accepted.
