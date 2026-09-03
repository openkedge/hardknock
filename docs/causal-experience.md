# Causal experience (V0.14)

Agents reason. Hardknock gives them experience. A hypothesis becomes useful only
when an intervention can test it. No hidden reasoning, agent vote, or remote support
label can promote a local causal hypothesis.

## Local demo

Copy `fixtures/causal/stale-state/` into a disposable Git repository and commit it.
Use a dedicated Hardknock data directory. The source checkout remains unchanged.
The fixture has no network or model dependency and executes through the normal
Experiment Engine. `--trusted-local` acknowledges that Git worktrees are not a
security sandbox: host filesystem, credentials, and network are not isolated.

```sh
hardknock --repo /path/to/fixture --home /path/to/data causal demo
hardknock --home /path/to/data causal list
hardknock --home /path/to/data causal plan investigation-<uuid>
hardknock --home /path/to/data causal test causal-<uuid> --trusted-local
hardknock --home /path/to/data causal explain causal-<uuid>
hardknock --home /path/to/data causal refine causal-<uuid>
hardknock --home /path/to/data causal model list
hardknock --home /path/to/data causal model show causal-model-<uuid>
hardknock --repo /path/to/fixture --home /path/to/data causal benchmark --trusted-local
```

Run each of the three hypotheses. The planner puts the state-refresh intervention
first: it splits predictions into latency/retries → FAIL and stale state → PASS.
The next two pairs retain stale state while removing latency or increasing retries.
Only the refresh intervention changes failure to success in this fixture.

`causal compare H1 H2`, `impact H`, `provenance H`, `model history MODEL`, and
`model diff MODEL --from N --to M` expose the evidence and changes.
`causal envelope INVESTIGATION` returns exact multidimensional tested points;
`causal curriculum INVESTIGATION` returns unexecuted discrimination or contradiction
goals. `causal replay INTERVENTION --trusted-local` creates a fresh investigation and
new evidence against the recorded starting proof, failing closed on fingerprint drift.
If a hypothesis belongs to multiple investigations, use `test` or `refine` with
`--investigation INVESTIGATION` to select the intended adapter/context.

## Custom investigations

`causal investigate --spec input.json` accepts the serialized
`causal::CausalInvestigationInput` (see the Rust model and `benchmark::stale_state_input`
for a complete constructor). It registers, but never executes, a trusted fixture.

- `variables`: typed IDs, names, kinds, domains, observability and intervenability.
- `hypotheses`: explicit cause, outcome variable, claim kind, scope, conditions,
  baseline prediction and intervention prediction. Status is reset to Candidate.
- `spec`: committed starting state, input values, distinct root-level `*.input`
  bindings, one trusted command, evaluator, available intervention values,
  Reality requirements, known confounders and budgets.
- `source_experiences`: optional existing Experience IDs. Only declared observable
  environment facts are extracted. Observations are shown separately from trials.

Boolean, categorical, integer-range and float-range values are validated. Custom
domains are descriptive only and cannot be intervened on. Input files cannot be
hidden, nested, absolute or symlinks. Setup failure is inconclusive, not evidence
against the hypothesis. The task adapter must finish successfully and expose its
task outcome to the shared evaluator; evaluator failure represents the tested effect.

One intervention changes one literal. All other supplied inputs must be held
constant. Missing control of a declared confounder, more than one changed variable,
or an already-confounded Experiment gives no support. Non-equivalent starts or
inconsistent evaluators invalidate the pair. Unknown confounders can still exist.

The default causal budget permits three interventions, one changed variable per
intervention, and 300 seconds total reserved execution duration. Per-pair duration
is conservatively allocated from that ceiling and clamped by the existing runner.
Started attempts count even if rejected or interrupted. `ExperienceBudget` applies
per paired experiment; `CausalBudget` limits the investigation. Registration and
planning never launch work. Zero budget does not imply the hypothesis is untestable.

## Interpretation and lifecycle

Only a completed, controlled, locally recorded pair can provide support. The baseline
must match the explicit baseline prediction, and the intervention must change the
outcome as predicted. Failure to change it contradicts that specific hypothesis.
Unrelated hypotheses are not disproved merely because another intervention worked.
One controlled counterexample is retained rather than outvoted by prior support.

Default support policy: one valid pair → Supported; at least three distinct local
experiment replications plus Moderate V0.13 diversity → StronglySupported. Repeated
identical evaluator/fixture fingerprints remain low-diversity. Evaluator identity
and committed fixture/environment fingerprints are derived from execution records.
No status means “proven.” Necessary/sufficient, mediation, and risk claims do not
gain support from a single Boolean pair; those remain deliberately inconclusive.

Interactions are represented by explicit conditions and tested using separate
single-variable pairs. No combinatorial search or inference over unobserved values
is performed. Contextual contradictions preserve both observations and mark the
hypothesis disputed/Contradicted; `explain` reports differing variables for explicit
scope refinement. No scope is silently broadened or narrowed.

## Learning, runtime, federation and assurance

`causal refine` stores a Lesson revision candidate with scoped Lesson guidance,
conditional Reflex guidance and mechanism-targeted Recovery guidance. It does not
edit existing artifacts, activate Reflexes or skip their existing validation tests.
`causal link --spec dependency.json` records an explicit dependency between a locally
supported hypothesis and an existing Lesson, Reflex, Recovery, Skill, certification
or runtime decision. An intervention link must reference actual support. Such a link
is an explicit author-specified relationship, not automatic semantic verification
of arbitrary Recovery code.

Runtime uses only exact tested input combinations, matching repository versions and
failure signatures. No cross-version freshness is assumed. A fresh available Recovery
linked to the supported intervention is preferred; otherwise guidance is REPLAN.
Unresolved high-risk mechanisms lead to safe experiment recommendations or abstention.
Hard security policy is checked first. Decision reasons and `why --decision` retain
the hypothesis/intervention basis; used mechanisms are linked for impact analysis.

Controlled contradiction appends review/revalidation records for linked artifacts.
Runtime and retrieval exclude quarantined guidance without rewriting artifact history.
Bridge cache reload excludes those entries; persisted runtime decisions additionally
check current quarantine state, including for an already-running Bridge.

Optional assurance `CausalFailureCoverage { severity, minimum_supported_mechanisms }`
counts only explicitly linked locally supported mechanisms for the selected Skill.
Basic certification does not require causal coverage. Unknown coverage is inconclusive.

Remote origin and its reported status can be carried with a candidate. They are
preserved as advisory metadata, never copied into local support or counted as a
replication. Local reproduction establishes local support/contradiction independently.
Automatic embedding into signed federation packages and agent MCP proposal tools
are deferred; the explicit local investigation API is the current ingestion boundary.

This release is explicit local testing, not general causal discovery, production
destructive experimentation, statistical identification, or a universal causal graph.
