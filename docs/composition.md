# Bounded operational composition

V0.19 adds explicit compositions of pinned Skills, Tools, Recoveries and EffectPlans. A composition is a 1–50-step directed acyclic ordering graph. It does not choose goals, infer an arbitrary workflow or grant production authority. Repeated use of a component requires separate step identities.

`Before`, `RequiresSuccessOf`, `ConsumesOutputOf` and `EstablishesPreconditionFor` impose ordering. Other relations describe invalidation, recovery, compensation or alternatives; they do not synthesize control flow. Declare the selected executable steps explicitly. `RequiresSuccessOf` stops downstream execution after failure.

Definitions start Candidate or Testable. Static compatibility never promotes them. Controlled successful trials support a revision; contradictory trials degrade it. Changed or unavailable components require revalidation. Promotion to a CompositeSkill additionally checks the composition assurance profile and persists an EvidenceManifest. No component maturity is changed by composition testing.

Migration 024 stores current definitions, immutable revisions/component bodies/evidence, handoffs, interactions, composite Skills and events. Historical inspection reads archived bodies; current replay rechecks current dependencies.

## CLI

```
hardknock compose list
hardknock compose pin --tool <tool-id>
hardknock compose import composition.json
hardknock compose show <id-or-name>
hardknock compose inspect <id-or-name> --context context.json
hardknock compose proof <id-or-name> --repo /path/to/clean/repository
hardknock compose test <id-or-name> --request trial.json --trusted-host
hardknock compose validate <id-or-name> --request control.json --request failure.json --trusted-host
hardknock compose why <id-or-name>
hardknock compose gaps <id-or-name>
hardknock compose recovery <id-or-name>
hardknock compose history <id-or-name>
hardknock compose diff <id-or-name> --from 1 --to 2
```

Request JSON uses `CompositionExperimentRequest`: exact composition/revision, the proof returned by `compose proof`, initial typed facts, per-step JSON inputs, explicit failure injections, behavioral checks and an ExperienceBudget. A proof binds the commit/tree, pinned manifests and controlled environment. The repository must have a committed clean starting state.

Without `--trusted-host`, execution requires the configured container tool provider (currently Docker with alpine:3.20); no host fallback occurs. Host mode is an explicit development facility with Observed assurance. Tools must be available in the chosen runtime. Tests use a local deterministic Python fixture, no models or network.

Each step uses the existing ToolRouter and a fresh Git Reality. A RuntimeController decision is recorded before every test step; the laboratory records unsafe behavior intentionally to test hypotheses. This is not a production dispatcher. Live runtime recommendations remain separate from external enforcement.

See [contracts](composition-contracts.md), [invariants](sequence-invariants.md), [recovery](composition-recovery.md), [capabilities](composition-capabilities.md), [effects](composition-effects.md) and the [implementation report](v0.19-report.md).
