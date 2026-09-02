# Epistemic evidence and common-mode risk

Hardknock V0.13 coordinates evidence generation. It is not a distributed
consensus protocol, leader-election system, or Byzantine quorum. The subject is
empirical support for a scoped Claim, not replicated machine state.

## Evidence paths, not heads

An `EvidencePath` records a conclusion, its source category, its observable
context, and known dependencies. Agent opinion, a controlled experiment, chaos,
recovery, a static check, a human observation, and federated evidence remain
distinct source kinds. Agreement never increments a synthetic probability.

The deterministic policy reports:

- `LOW` when paths repeat effectively identical contexts or share a dominant
  experience, tool, evaluator, or external source with little countervailing
  diversity;
- `MODERATE` when multiple known dimensions differ but important overlap
  remains;
- `HIGH` when multiple source types include controlled empirical support,
  evaluator diversity, several other known differences, and no dominant
  common dependency;
- `UNKNOWN` when missing metadata prevents positive diversity credit.

These are policy categories, not calibrated probabilities.

## Epistemic fault domains

An epistemic fault domain is a set of paths which may share a source of error:
the same model family, runtime, prompt family, retrieved Lesson, external
document, tool, evaluator, environment, or federated root origin. Hardknock can
only expose known dependencies. The graph does not establish statistical
dependence or prove causality.

A fingerprint summarizes model family, active experience, retrieval sources,
toolset, and evaluators. Equal fingerprints flag high correlation risk but do
not destructively collapse observations.

## Challenge planning

The acquisition planner prioritizes the dominant missing dimension within the
existing `ExperienceBudget`. It may recommend withholding a selected Hardknock
Lesson during an explicit evidence-generation run, independent retrieval,
alternative tooling/evaluation, a different available agent, or a controlled
counterfactual. Withholding a Lesson only varies selected Hardknock experience;
it does not prove complete independence.

Normal production execution still maximizes useful validated experience
application. Blind replication belongs only to explicit epistemic challenges.

> **The best next evidence is often the evidence most likely to prove us wrong.**

## Fusion and runtime use

Fusion preserves support, contradiction, and inconclusive paths separately.
Support plus contradiction is `DISPUTED` regardless of vote count. Correlated
agreement can be `SUPPORTED` with `LOW` diversity. Controlled corroboration
across known fault domains can become `DIVERSE_SUPPORT`.

Configured high-consequence actions may require a minimum diversity class. A
`KnownSupported` action below that requirement becomes `EXPERIMENT`,
`REQUIRE_APPROVAL`, or `ABSTAIN`; it does not `ACT` merely because five agents
repeat one Lesson. Low-risk high-diversity support does not trigger additional
agent work.

## Federation echoes

Immediate node count and root evidence origins are separate. Reexports through
four nodes may still represent one empirical origin. Missing root lineage does
not earn origin diversity. Genuine local reproductions introduce new root
empirical origins.

## Hardknock's own fallibility

Lessons, Reflexes, Recoveries, and Certifications can become stale,
overgeneralized, or wrong. Lesson applications and explicit influence records
reconstruct blast radius across sessions, agents, repositories, decisions, and
outcomes. Diverse contradiction marks shared experience for revalidation.
Quarantine retains the artifact and its history while excluding it from
automatic retrieval.

