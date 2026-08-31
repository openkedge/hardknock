# Hardknock V0.11 Implementation Report

## 1–5. Surface, schema, model, evaluation, observability

V0.11 adds `src/assurance/{model,evaluator,artifact}.rs`,
`src/store/assurance.rs`, `src/cli/assurance.rs`, `tests/assurance.rs`, four
assurance documents, and migration 014. Existing Skill and SkillRevision JSON
gain a backward-compatible optional `BehavioralContractRef`.

`BehavioralContract` supports Skill, Tool, Recovery, and EffectPlan subjects;
deterministic evaluator/state/effect/capability/custom conditions; phased
invariants; forbidden outcomes; capability requirements; and observation
requirements. Evaluation uses satisfied, violated, inconclusive, and
not-applicable. Missing observation is inconclusive. `ContractObservability`
compares each condition with declared evaluator/state/effect/capability/custom
observation support before certification.

Migration 014 adds immutable Behavioral Contract revisions and Skill binding
history, immutable Evidence Manifests and certificates, append-only
revocations, and immutable external artifact observations. Certificate rows
reference exact Skill and contract revisions.

## 6–9. Profiles, manifests, and negative evidence

The three built-ins are `basic-behavior-v1`, `resilience-basic-v1`, and
`capability-minimal-v1`, each with a stable ID and explicit version. Profiles
can require run counts, controlled experiments, distinct profile coverage,
recovery coverage, Reflex false-positive limits, capability profiles,
minimization, Critical contradiction absence, freshness, and attestation
assurance.

The manifest references all V0.11 evidence classes and records policy versions,
contract evaluations, distinct coverage, recovery/reflex observations,
contradictions, capability/effect facts, attestation integrity and assurance,
timestamps, known unknowns, and tool/runtime hashes. Canonical sorting and
deduplication prevent ID-order and duplicate-count instability; IDs and
generation timestamps are excluded from the evidence graph hash. Contradicted
Lessons and failure outcomes are selected with positive evidence rather than
filtered out.

## 10–12. Lifecycle, freshness, and revisions

`skill certify --dry-run` only evaluates. The explicit non-dry command persists
an immutable manifest and `Certified` record only when recommendation is
eligible. Revocation appends a record and never deletes history. Freshness is
current, review-recommended, expired, or invalidated. Skill/contract revision
changes invalidate applicability; tool/runtime hash changes recommend review;
expiry and revocation remain explicit.

## 13–15. Capabilities, attestations, and curriculum

Behavior and authority are evaluated independently. Forbidden, required, and
maximum capability patterns are checked against observed manifests. Ambient
credentials, Effect commit authority, or any capability outside the maximum
block certification even when behavior passes. Strict profiles require
`IsolatedObserved` or stronger attestations; corrupted attestations block.

Evidence gaps generate `SatisfyAssuranceRequirement` and
`ChallengeInvariant` goals. Goals identify missing conditions and require an
explicit isolated curriculum action. Certification itself never runs a trial
or commits an external effect.

## 16–20. Artifacts, federation boundary, and CLI

`.hkcert` embeds the exact certificate, contract, profile, manifest, provenance,
and Ed25519 producer signature. The signature covers every unsigned field;
verification also recomputes the manifest hash and internal references. A valid
remote artifact is authentic external evidence but never local certification.
Referenced tools are identities only and are not installed.

The required `contract list/show/validate/history/diff`, `assurance
show/gaps/history/diff/export/verify/revoke`, and `skill certify` commands are
implemented. `contract register` is the explicit project-file acceptance and
Skill-binding path. JSON and readable output contain real evaluations, gaps,
blockers, dimensions, evidence, and revision data.

## 21–29. Deterministic acceptance results

The local V0.11 test target observes:

| Scenario | Result |
| --- | --- |
| satisfied deterministic contract | satisfied |
| missing condition evidence | inconclusive; additional evidence curriculum |
| 99 successes + 1 Critical invariant failure | blocked |
| behavior success + unrestricted network above registry-only maximum | behavior satisfied; certification blocked |
| tool H1 changed to H2 | review recommended |
| Skill or contract revision changes | invalidated applicability; old record historical |
| signed artifact before mutation | authentic; local certification not established |
| Evidence Manifest mutation | manifest and signature invalid |
| prompt-style project TOML | parsed, structured, observable |
| 1,000 Experiences + 100 Experiments + 120 Attestations | stable hash under the 2-second test gate |

The pure suite is local, deterministic, network-free, and external-model-free.
`UnsupportedClaimRate` and `CriticalViolationCertificationEscapeRate` are zero
for the constructed acceptance cases because ineligible assertions cannot be
issued and Critical violations are hard blocks.

## 30–31. Limitations and deviations

V0.11 is empirical assurance, not theorem proving, a public certification
authority, a global trust root, hardware attestation, a marketplace, or a
multi-tenant service. State predicates require structured observations;
Hardknock cannot infer an unrecorded external fact. Dependency freshness is
limited to recorded revisions and tool/runtime hashes. Remote verification is
advisory; reproduction planning/execution and external certificate comparison
remain future integration work. Package-level certification types are modeled,
while the existing package CLI is not expanded into a second duplicate
certification workflow.

The implementation uses one cohesive completion commit rather than the prompt's
suggested multi-commit sequence because the requested deliverable was to
complete and commit the phase as one reviewable repository state.

## 32. Recommended V0.12 direction

Use Experience plus assurance during live control. The runtime should choose
normal action, bounded experiment, recovery, replan, approval, or abstention
from current context, operating envelope, known gaps, freshness, effect risk,
and authority. Every choice must leave evidence and assurance must never grant
authority by itself.
