# Bounded team governance

V0.21 adds a local control plane for role-separated teams. It evaluates a proposed
team and constrains runtime actions; it does not spawn agents, schedule a swarm,
share credentials, or replace OpenKedge authorization.

## Knowledge and epistemic profiles

`TeamGovernance` binds one immutable policy to one team revision. Every member has
an experience/dependency profile and a capability envelope. Every role has a
capability envelope and `RoleKnowledgePolicy`. A later team revision must record a
new governance document before execution can continue.

Knowledge modes are `full`, `role_scoped`, `blind_challenge`, and `minimal`.
Blind challenges can suppress named lessons and selected experience in the Bridge
response. Constraints and anti-pattern warnings remain visible. The delivered view
is recorded, so challenge completion can show that the challenger did not receive
the hidden artifact. Quarantined artifacts are never delivered as active guidance.

`TeamEpistemicProfile` combines declared member dependencies with canonical
evidence paths. Declared profiles identify common-mode risk but create no evidence
and earn no diversity credit. Reports name shared model, prompt, retrieval,
experience, tool, evaluator, environment, and external-evidence dependencies. They
do not claim statistical independence.

## Challenges and bounded cost

A challenge names an existing review, challenger assignment, strategy, knowledge
policy, evidence requirement, expiry, token ceiling, and logical latency ceiling.
Completion requires a contribution from the authenticated challenger and new
canonical evidence created after assignment. Alternative strategies must change the
requested dependency dimension. Controlled strategies require a recorded
multi-candidate experiment result. Relayed evidence and opinions do not count.

Team formation only evaluates candidates. When evidence already satisfies the
requested diversity it recommends zero additional runs. Economics exposes four team
opportunity kinds and curriculum exposes five bounded team goals. Both reuse existing
budgets and the isolated experiment executor.

## Responsibility, reassignment, recovery, and effects

Responsibilities cover plans, plan steps, composition steps, claims, experiments,
effects, recoveries, and reviews. One subject has one owner at a team revision.
Plan and composition execution compare the acting member/assignment with that owner.

Reassignment requires a current source, distinct target, evidence, a matching
knowledge snapshot, and a target capability envelope covering the role. It advances
the team revision and governance together, invalidating old delegations, reviews,
handoffs, and responsibilities without erasing them.

Recovery requires a structured handoff referencing failure evidence, a validated
Recovery, a plan run, and receipts for referenced committed effects. Runtime accepts
a recovery action only for the current Recovery role and exact handoff/action hash.

At effect commit and reconciliation, Hardknock re-resolves the runtime decision,
team revision, role, delegation chain, review, plan step, and knowledge snapshot.
The verified `EffectActorContext` is committed atomically with the receipt and
embedded in receipt metadata. Explicit external commit authorization remains
mandatory. Agent role authority cannot create an approval.

## Assurance and guard boundary

`team-assurance-basic-v1` checks assignments, capability envelopes, high-risk
review separation, handoff integrity, and dependency profiles.
`team-epistemic-diversity-v1` additionally requires moderate evidence diversity and
a completed challenge. Assessments apply only to the named action and team revision;
they do not certify that a team is safe.

Repeated role violations can yield a `TeamGuardRecommendation` with
`automatic_promotion: false`. External governance must independently validate and
adopt any guard.

## CLI

```text
hardknock team governance-import governance.json
hardknock team governance-show <team-id> <revision>
hardknock team epistemic <team-id>
hardknock team formation <team-id> roles.json --minimum moderate
hardknock team challenge assign challenge.json
hardknock team challenge complete <challenge-id> <contribution-id> --context context.json --tokens 800 --latency-ms 1000
hardknock team responsibility responsibility.json
hardknock team reassign reassignment.json
hardknock team recovery-handoff handoff.json --context context.json
hardknock team assurance --context context.json --profile basic
hardknock team why <team-id>
hardknock team benchmark
```

Administrative mutations are absent from the agent Bridge. The Bridge accepts team
decision context and returns filtered structured context; local code re-resolves IDs.

## Limits

The local SQLite store and authenticated session adapter are the trust boundary.
Dependency profiles are declared metadata until supported by evidence paths.
Sensitive/restricted handoffs are rejected rather than passed to a general DLP
system. Logical latency in the comparison counts stages, not wall-clock latency.
