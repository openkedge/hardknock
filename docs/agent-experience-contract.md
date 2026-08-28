# Portable agent experience contract

**Reflection proposes experience. Experiment promotes it.**

Experience belongs to Hardknock. Vendor adapters translate observable decisions and events; they do not define lesson truth, directly read SQLite, or request hidden chain of thought.

An agent receives its task and current context, compact relevant experience, evidence summaries, and optional reflex/recovery advice. An agent can report observable actions, lesson usage or disagreement, explicit hypotheses, recovery attempts, and run outcomes. Not every output has a callable Bridge endpoint in this pass: arbitrary hypotheses, recovery tools, skill-validation requests, and experiment execution remain future extensions. The implemented input/output boundary is listed in [Bridge v1](bridge-protocol.md).

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
