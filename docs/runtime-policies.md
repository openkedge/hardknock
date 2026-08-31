# Runtime policy profiles

Runtime policy describes how evidence influences a control decision. It is distinct from Experience, which records what happened; assurance, which evaluates evidence against a contract; and capability policy, which controls authority.

The controller is deterministic and inspectable. It does not invoke an LLM or inspect private chain of thought. Explicit uncertainty, missing evidence, alternative count, stale predictions, contradictions, operating-envelope position, and known gaps are normal inputs.

## Precedence

The policy evaluates these constraints in order:

1. hard external security policy;
2. capability availability, isolation, and Effect-adapter support;
3. a fresh Recovery matching the observed failure;
4. active Reflexes and current local Lessons identifying a failure precursor;
5. operating-envelope failure or degradation;
6. knowledge state, assurance, risk, and authority;
7. the agent's preferred strategy.

Experience never overrides a hard policy denial, creates a missing capability, upgrades isolation, or grants Effect commit authority. `ACT` means the action may proceed to those ordinary enforcement layers.

## Transparent matrix

| Knowledge | Low risk | Medium risk | High or critical risk |
| --- | --- | --- | --- |
| Supported | Act | Act | Act only with applicable current assurance and authority; otherwise experiment, approval, or abstention |
| Unknown | Act with warning | Experiment when safe | Experiment under balanced policy when safe; conservative policy abstains |
| Contradicted | Experiment | Experiment | Abstain unless evidence and external approval resolve the conflict |
| Stale | Act with warning | Experiment | Revalidate, request approval, or abstain |
| Out of scope | Experiment when safe | Experiment when safe | Abstain when no safe bounded test exists |

Envelope, Reflex, Recovery, capability, and security precedence can produce a decision before this matrix is consulted.

## Profiles

`developer` allows low-cost exploration more readily, but still cannot bypass hard security, capability, isolation, Effect, or authority requirements.

`balanced` is the default policy profile. It permits low-risk reversible unknown work with a warning, experiments on medium-risk uncertainty, and uses a safe bounded experiment for high-risk unknowns when available.

`conservative` requires experiments for medium-risk unknowns and abstains on high-risk unknowns unless current applicable assurance or an approval path is present.

Every policy configuration receives a content-derived version such as `hardknock.runtime-policy.v1:<hash>`. Immutable decision records retain the exact version used. A version cannot later name different configuration contents.

## Experiment selection

An experiment is available only when the configured mode is not `off`, a safe Reality exists, the proposed Effect can be tested without committing it, and budget remains. The decision contains explicit Reality requirements and the bounded Experience budget.

Expected learning value is categorical (`low`, `medium`, or `high`). It uses declared uncertainty, severity, task-family reuse, evidence gaps, and coarse experiment cost. It is an ordering aid, not a calibrated probability or universal score.

## Tool selection

When several tools satisfy an `ACT` task, the controller prefers current assurance and then the narrower declared capability width. A broader tool is not automatically safer because it is familiar. Tool selection never expands the parent Reality or grant.

## Simulating policy

```bash
hardknock decision compare \
  --scenario fixtures/runtime-scenarios/unknown-high-risk.json \
  --policies balanced,conservative

hardknock decision simulate \
  --action 'deploy service-a' --risk high --testable \
  --policy conservative --no-record

hardknock runtime policy
```

Policy comparison is read-only and executes no proposed action. Decision replay creates a new immutable record using current evidence and policy, which makes policy and Experience evolution auditable.
