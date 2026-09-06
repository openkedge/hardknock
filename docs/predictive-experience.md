# Predictive Experience

Hardknock v0.15 forecasts only from normalized observable trajectories and earned
evidence. It is not a generic anomaly detector, telemetry platform, probability
model, or authority source.

## Concepts

- A `FailureSignature` recognizes a failure that already exists. An
  `EarlyWarningSignature` recognizes an ordered precursor before failure.
- A `RiskIndicator` detects an observable condition. A `Reflex` or
  `PreventiveIntervention` describes a response; prediction itself never executes it.
- A candidate warning never activates. Local validation needs matching failed
  trajectories and successful negative controls. Two of each are required for
  `Validated` status in the initial policy.
- Forecast strength is qualitative. `Strong` means the warning has matching history
  and a locally supported causal basis; it never means a fabricated probability.
- Runtime/tool-version scope is exact. Federated warnings enter as advisory candidates
  and must be reproduced locally.

Trajectories contain bounded operational features, not conversation transcripts,
prompts, hidden reasoning, inherited environment variables, or arbitrary secrets.
The default window is 20 relevant events or five minutes. Fingerprints include event
order and normalized salient values while bucketing millisecond values instead of
hashing timestamps.

## CLI

```bash
hardknock trajectory list
hardknock trajectory show <trajectory-id>
hardknock trajectory compare <left> <right>
hardknock trajectory replay <trajectory-id>

hardknock forecast list
hardknock forecast show <forecast-id>
hardknock forecast explain <forecast-id>
hardknock forecast replay <forecast-id>
hardknock forecast quality
hardknock forecast impact <early-warning-id>
hardknock forecast forecastability <failure-class>
hardknock forecast discover <failure-class>
hardknock forecast curriculum
```

`trajectory replay` replays the normalized pre-failure prefix only; it cannot rerun
commands or effects, and it never treats an observed failure as an early warning. To
populate trajectories explicitly, use `trajectory start --spec`,
`trajectory event <id> --spec`, and `trajectory finish <id> --outcome`. Register a
candidate signature with `forecast register --spec` and validate it with comma-separated
positive and negative trajectory IDs:

```bash
hardknock forecast validate <early-warning-id> \
  --positive <failed-a>,<failed-b> \
  --negative <success-a>,<success-b>
```

`forecast discover` compares at least two matching failure trajectories with at least
two successful controls. Event or exact feature conditions present in every failure and
no more than one quarter of controls become inactive Candidate Risk Indicators. They do
not become warning signatures or affect runtime without separate validation.

The deterministic local benchmark intentionally executes reviewed fixture commands in
cooperative Git worktrees and therefore requires acknowledgment:

```bash
hardknock --repo fixtures-repository forecast benchmark --trusted-local
```

## Feedback and calibration

Every active forecast can resolve as failure occurred, avoided, false alarm, expired,
or inconclusive. Precision, recall, false-positive/negative rates, warning lead, avoided
failure rate, and unnecessary intervention rate remain `null` until their denominator
has enough deterministic samples. A failure with no forecast becomes a forecast miss.
When no precursor is observable, Hardknock records `insufficient_observability` rather
than inventing a predictor.

False alarms degrade predictor health and quarantine that warning from persisted runtime
decisions. Refinement creates an append-only revision candidate; it is not activated
until it passes positive and negative controls. Curriculum emits recommendations only.
Experience Profiles and immutable snapshots include a `PredictiveExperienceSummary`;
growth reports compare validated predictors, quality metrics, warning lead, and validated
preventive actions without filling in missing samples as zero.

Compact Bridge events cover trajectory updates, indicator matches, forecast creation and
resolution, preventive suggestions, and applied interventions. They contain typed IDs and
status only, not raw observation payloads.

## Prevention and authority

Preventive interventions are promoted only when same-start controlled Experiment arms
show control failure and intervention success. One independent Experiment supports the
action; a second validates it. Automatic runtime use requires a local validated warning
and validated intervention. The least disruptive matching action is preferred.

Forecasts grant no capability or effect authority. External mutations without commit
authority produce `REQUIRE APPROVAL`; weak low-severity warnings with costly responses
produce observe/warn behavior. Observe, Advise, Adaptive, and Governed modes retain their
existing semantics and hard governance always wins.
