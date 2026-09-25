# Documentation

Start with the [root README](../README.md) for a runnable example and current status. This index groups the deeper material by task. `hardknock --help` and subcommand `--help` show the command syntax for the checked-out build; [CLI reference](cli.md) adds examples and semantics.

## Start and understand

- [Architecture](architecture.md): components, persistence, and safety boundaries.
- [Experience model](experience-model.md): observations, hypotheses, Lessons, scope, and provenance.
- [Controlled experiments](experiments.md) and [agent experiments](agent-experiments.md): paired trials and explicit strategy comparisons.
- [Generic agent contract](agent-integration.md) and [native integrations](integrations.md): connect an agent to the runner or Bridge.
- [Roadmap](roadmap.md): milestone goals and longer-term direction.

## Learn, retrieve, and improve

| Topic | Guides |
| --- | --- |
| Evidence and transfer | [Retrieval](retrieval.md) · [Experiment quality](experiment-quality.md) · [Experience abstraction](experience-abstraction.md) · [Knowledge hierarchy](knowledge-hierarchy.md) |
| Resilience | [Chaos campaigns](chaos.md) · [Operating envelopes](operating-envelopes.md) · [Reflexes](reflexes.md) · [Recovery](recovery.md) |
| Development | [Curriculum](curriculum.md) · [Persistent development](development.md) · [Experience economics](experience-economics.md) · [Experience budgets](experience-budget.md) |
| Decisions | [Runtime control](runtime-control.md) · [Runtime policies](runtime-policies.md) · [Abstention](abstention.md) · [Decision records](decision-records.md) · [Predictive experience](predictive-experience.md) · [Causal experience](causal-experience.md) |
| Evidence quality | [Epistemic evidence](epistemic-evidence.md) · [Behavioral contracts](behavioral-contracts.md) · [Assurance](assurance.md) · [Certification artifacts](certification-artifacts.md) |
| Knowledge lifecycle | [Knowledge resolution](knowledge-resolution.md) · [Snapshots](knowledge-snapshots.md) · [Conflicts](knowledge-conflicts.md) · [Guard revision candidates](guard-revision-candidates.md) |

## Execute and change external state

| Topic | Guides |
| --- | --- |
| Execution | [Execution boundary](execution-boundary.md) · [Container Realities](container-realities.md) · [Capabilities](capabilities.md) · [Capability minimization](capability-minimization.md) · [Credential broker](credential-broker.md) · [Micro-sandboxes](micro-sandboxes.md) · [WASI status](wasi.md) |
| Tools and attestations | [Portable tools](tools.md) · [Tool manifests](tool-manifests.md) · [Execution attestation](execution-attestation.md) · [Security benchmark](security-benchmark.md) · [Threat model](threat-model.md) |
| Effects | [Governed effects](effects.md) · [Transactional Realities](transactional-realities.md) · [Effect adapters](effect-adapters.md) · [Effect security](effect-security.md) · [Commit semantics](commit-semantics.md) · [Reconciliation](reconciliation.md) · [Compensation](compensation.md) · [PostgreSQL adapter](postgres-effect-adapter.md) |

## Compose and share work

| Topic | Guides |
| --- | --- |
| Composition | [Overview](composition.md) · [Contracts](composition-contracts.md) · [Capabilities](composition-capabilities.md) · [Effects](composition-effects.md) · [Recovery](composition-recovery.md) · [Sequence invariants](sequence-invariants.md) · [Plan validity](plan-validity.md) |
| Agents and Bridge | [Portable agent contract](agent-experience-contract.md) · [Bridge protocol](bridge-protocol.md) · [Claude](integrations/claude.md) · [Codex](integrations/codex.md) · [Hermes](integrations/hermes.md) · [OpenClaw](integrations/openclaw.md) |
| Sharing | [Federation](federation.md) · [Team governance](team-governance.md) · [Handoffs](team-handoffs.md) · [Review gates](team-review.md) |

## Progress and evidence

The [V0.22 distributed sync checkpoint](v0.22-progress.md) states what works now and what remains. Reports are historical snapshots, so prefer the current feature guide and CLI help for usage. `benchmarks/` contains machine-readable summaries.

| Milestones | Reports |
| --- | --- |
| Foundation and transfer | [Phases 3–6](implementation-phase-3-6.md) · [Transfer](implementation-transfer.md) |
| V0.2–V0.7 | [V0.2](implementation-v02.md) · [V0.3](implementation-v03.md) · [V0.4](implementation-v04.md) · [V0.5](implementation-v05.md) · [V0.6](implementation-v06.md) · [V0.7](implementation-v07.md) |
| V0.8–V0.12 | [V0.8](implementation-v08.md) · [V0.9](implementation-v09.md) · [V0.10](implementation-v010.md) · [V0.11](implementation-v011.md) · [V0.12](implementation-v012.md) |
| V0.14–V0.17 | [V0.14](implementation-v014.md) · [V0.15](implementation-v015.md) · [V0.16](implementation-v016.md) · [V0.17](implementation-v017.md) |
| V0.18–V0.21 | [V0.18 pass 1](v0.18-pass1-report.md) · [V0.18 pass 2](v0.18-pass2-report.md) · [V0.19](v0.19-report.md) · [V0.20](v0.20-report.md) · [V0.21 progress](v0.21-progress.md) · [V0.21 report](v0.21-report.md) |
