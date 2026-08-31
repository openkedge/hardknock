# Empirical Assurance

Hardknock assurance interprets an existing evidence graph against one named
profile. It does not replace execution, experimentation, chaos, recovery, or
attestation. Those systems produce evidence; assurance makes a bounded claim
about it.

> **Assurance should try to break the claim it certifies.**

## Claim semantics

`Certified` means the specified Skill revision satisfied the specified
Behavioral Contract under the evidence requirements of the specified Assurance
Profile. It does not mean universally correct, bug free, safe in every
condition, or mathematically proven.

V0.11 ships three stable built-in profiles:

| Profile | Evidence emphasis |
| --- | --- |
| `basic-behavior-v1` | observed contract satisfaction, no unresolved Critical contradiction, fresh evidence |
| `resilience-basic-v1` | base behavior, controlled experiments, distinct perturbation coverage, contradictions, freshness |
| `capability-minimal-v1` | base behavior, declared minimal capability profile, minimization evidence, isolated attestation, contradictions |

Profiles expose Behavior, Resilience, Recovery, Capability Discipline, Effect
Discipline, and Evidence Freshness separately. They are not collapsed into a
universal score.

## Evidence Manifest

The manifest references Experiences, Experiments, chaos campaigns,
attestations, Lessons, Reflexes, Recoveries, operating envelopes, capability
manifests, and commit receipts. The local selection policy starts from the
exact Skill revision and its campaigns, then includes relevant negative and
contradictory evidence. Lists are sorted and deduplicated. The BLAKE3 graph
hash excludes the local manifest ID and generation time, so the same selected
graph and policy versions reproduce the same hash.

Every certificate records confidence, validation, freshness, contract
evaluator, capability, evidence-selection, profile, and contract versions.
Manifest insertion checks that every referenced local record exists.

## Eligibility and hard blocks

An unobserved requirement is `inconclusive` and produces
`additional_evidence_required`. A named violation is `violated`. Any in-scope
Critical invariant failure, Critical forbidden outcome, experimental effect
leak, corrupted attestation, or capability maximum violation produces
`blocked`. Counts and success percentages cannot override a hard block.

Missing evidence produces targeted `SatisfyAssuranceRequirement` or
`ChallengeInvariant` curriculum goals. The goals name distinct missing
conditions. Certification does not auto-run them and never commits an effect.

## Lifecycle and freshness

Certification is an explicit `skill certify` action. Dry-run evaluation does
not persist a manifest or certificate. Certificates and revocations are
append-only. A new Skill or contract revision never inherits applicability.
Expiry is profile/policy controlled; tool artifact or runtime changes recommend
review. Integrity failure or explicit revocation invalidates the assertion.

Known unknowns and unsupported conditions remain visible even on an eligible
certificate. An eligible profile is evidence-complete only relative to that
profile's finite requirements.
