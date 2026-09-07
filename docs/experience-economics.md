# Experience economics

V0.16 answers a narrower question than “what can Hardknock test?”:

> Given many unresolved evidence gaps and a finite acquisition budget, which
> experiments are worth spending on now?

The answer is deterministic and inspectable. It is not a learned utility model,
an LLM judgment, a continuous scheduler, or a claim that unrelated scientific
questions share one natural score.

## Domain model

An `ExperienceOpportunity` identifies one testable gap, a typed target, structured
reasons, dependencies, estimated cost, experiment risk, current saturation, and a
multidimensional value vector. The vector retains six distinct bands:

- risk reduction;
- learning value;
- reuse potential;
- decision relevance;
- evidence gap;
- novelty.

Risk reduction is explained by failure severity, observed runtime exposure, and
the current mitigation gap. Decision relevance records affected decisions and task
families. Cost separately records trials, external agent runs, expected duration,
compute, human attention, staging resources, and external-effect risk.

The persisted status lifecycle is `Candidate → Eligible → Selected → Running →
Completed`. `Deferred`, `Saturated`, `Blocked`, and `Invalidated` make non-selection
first class instead of silently dropping work.

## Opportunity generation

`Store::experience_planning_context` projects existing Hardknock records without an
LLM. The deterministic generator currently maps:

| Existing evidence | Opportunity |
| --- | --- |
| repeated runtime abstention or unknown | `ResolveRuntimeUnknown` |
| forecast miss | `InvestigateForecastMiss` |
| candidate, stale, noisy, or contradicted warning | validate, revalidate, or reduce false positives |
| unresolved causal hypothesis | `DiscriminateCausalHypotheses` |
| contradicted Lesson or Recovery | `ResolveContradiction` |
| candidate Lesson | `ValidateLesson` |
| unvalidated Recovery or Reflex | validate the artifact |
| low-diversity Claim | `IncreaseEvidenceDiversity` |
| supported Skill | harden, map its envelope, or close its assurance gap |
| broad network grant | `MinimizeCapability` |
| locally relevant external object | `ReproduceFederatedExperience` |

Stable opportunity IDs are hashes of kind, target, and structured reasons. Repeated
planning updates eligible records but does not overwrite a running or completed
opportunity.

## Saturation and marginal value

Saturation is contextual. The policy examines observations, equivalent
replications, distinct contexts, counterfactuals, evidence domains,
contradictions, and freshness. A contradiction resets a mature target to
`Contradicted` and gives resolution Critical marginal value. Saturation requires
multiple contexts, counterfactuals, diversity domains, and replications; a raw
test count alone is insufficient.

The marginal reason remains visible: first evidence, first counterfactual, context
extension, contradiction resolution, diversity, replication, repeated equivalent
replication, or saturation. Fresh saturated low-risk work receives no allocation.

## Eligibility and allocation

Before ranking, the allocator rejects unsupported trial safety, disallowed effect
risk, unmet dependencies, unavailable approval budget, duplicate targets, stale
opportunities, and infeasible costs. Within the same priority class, a Pareto-
dominated opportunity is deferred when another candidate is no worse on every
value dimension and no more expensive, and is strictly better somewhere.

The default allocation is lexicographic:

1. explicit priority class, beginning with Critical unmitigated failures;
2. risk-reduction band;
3. number of affected runtime decisions;
4. runtime exposure;
5. learning-value band;
6. reuse potential;
7. novelty;
8. trial and agent-run cost;
9. stable opportunity ID.

High-blast-radius contradictions, repeated unmitigated failures, stale active
evidence, abstention gaps, assurance blockers, causal discrimination, epistemic
diversity, and capability minimization occupy documented classes. Objective modes
(`Balanced`, `Resilience`, `Assurance`, `Research`, and `Efficiency`) change a small
number of explicit class choices. There is no hidden weighted sum.

The policy ranks risk before cheapness. A four-trial Critical investigation can
reserve all four available trials before four one-trial low-risk candidates are
considered.

## Budget ledger

`ExperienceBudget` retains the older Reality and command fields and adds explicit
curriculum trials, parallel trials, human approvals, and allowed effect risk. A
ledger maintains separate `reserved`, `consumed`, and `remaining` counts for trials,
agent runs, duration, and approvals.

Reservation happens before work begins. Completion releases the estimate and
charges actual consumption. An early stop therefore returns unused trials. An
overrun is never hidden: consumed may exceed the original estimate and remaining
saturates at zero. Dimensions are never added into one synthetic utilization
number.

## Compilation, outcomes, and replanning

The opportunity compiler routes to the existing Curriculum, Experiment, Causal,
Federation Reproduction, or Capability Curriculum type. It deliberately does not
contain another experiment engine. `explore run` activates the reservation and
emits these typed delegation plans. The owning engine executes the concrete trial
and records an `ExperienceOpportunityResult` through the store API.

Results distinguish material learning, strengthened evidence, contradiction, no
material change, inconclusive science, execution failure, and cancellation. Actual
cost and evidence references are append-only. After each result, remaining
reservations are released and a new portfolio revision is calculated from the
remaining budget. Newly revealed Critical work can displace earlier selections.

The stop policy ends work when the objective is satisfied, evidence is saturated
or contradicted, the experiment becomes unsafe, or budget is exhausted. An
infrastructure failure is not mislabeled `NoMaterialChange`.

## CLI

```bash
hardknock explore plan --budget-trials 8 --max-agent-runs 3
hardknock explore run [portfolio-<uuid>]
hardknock explore status
hardknock explore show opportunity-<uuid>
hardknock explore why opportunity-<uuid>
hardknock explore report
hardknock explore history [portfolio-<uuid>]
hardknock explore replay portfolio-<uuid>
hardknock explore benchmark
hardknock experience debt
```

Planning and replay execute no trials. `show` and `why` include the full value
vector, cost, risk, evidence status, and latest selection or deferral reasons.
History includes immutable portfolio revisions and append-only events. Experience
debt means unresolved Medium-or-higher gaps with meaningful exposure and incomplete
mitigation; it is not ordinary source-code debt.

Development profiles include an `Experience Acquisition` summary: open Critical
and High opportunities, the latest portfolio, actual trials, material outcomes,
Critical closures, and early-stop savings.

## Persistence and concurrency

Migration 020 adds opportunities and reasons, portfolios and revisions, selections,
deferrals, ledgers, results, cost estimates and actuals, saturation observations,
debt, and economics events. Results and portfolio revisions are append-only. A
revision compare-and-swap prevents concurrent replans from overwriting each other.
Bridge events expose lifecycle changes without federating the private local backlog.

## Deterministic benchmark

`hardknock explore benchmark` compares round robin, fixed type priority, and the
adaptive portfolio with the same 20-trial / 5-agent-run limits over 22 mixed gaps.
The fixture includes two Critical gaps, three High gaps, five Medium opportunities,
five saturated Low opportunities, two contradictions, two causal investigations,
two forecast gaps, and one capability-minimization opportunity.

| Arm | Critical closed | High mitigated | Material rate | Saturated spend | Wasted rate | Held-out success |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Round robin | 0 | 0 | 0.30 | 4 | 0.70 | 0.62 |
| Static type priority | 2 | 4 | 0.75 | 5 | 0.25 | 0.84 |
| Adaptive portfolio | 2 | 6 | 1.00 | 0 | 0.00 | 0.87 |

These are deterministic fixture outcomes, not measurements of general agent
intelligence and not proof of global optimality. They support only the local claim
that this policy closed more high-impact gaps and avoided saturated spend relative
to round robin under the specified backlog and budget.

## Boundaries

- Estimates are explicit heuristics and are recorded for later audit; V0.16 does
  not train its allocation policy from outcomes.
- Observed runtime counts and artifact evidence can be incomplete. Unknown remains
  visible rather than being treated as zero risk.
- Capability minimization is a candidate test, never automatic authority removal.
- Federated evidence remains advisory and is locally reproduced; portfolio priority
  is not federated by default.
- A typed delegation plan still needs the target engine's concrete fixture or input
  configuration. The economics layer does not invent missing experimental inputs.
- Held-out benchmark rates are fixture calculations, not external validation.

