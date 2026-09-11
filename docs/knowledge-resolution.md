# Runtime knowledge resolution

The shared bridge normalizes Claude, Codex, Hermes and shell actions before `DefaultRuntimeKnowledgeResolver` invokes the Pass 1 deterministic resolver. Adapters own no precedence rules. `RuntimeController` receives resolved operational references, never a hierarchy graph.

`DefaultKnowledgeContextBuilder` observes task family, agent runtime, operating system, architecture, normalized action type and proposed effect type. Provider, resource type, API/dependency/tool versions, idempotency, reversibility, consistency and credential state can be supplied as typed observations. Missing values stay unknown. Runtime builders must not infer provider guarantees from URL spelling or agent prose.

Trust precedence is tool attestation, effect adapter, runtime, adapter, explicit user, agent report, imported context. Only observations at adapter trust or above enter applicability. Lower-trust reports remain available for conflict diagnostics. Equal highest-trust disagreement removes the value. Bridge `knowledge_reports` always has AgentReported provenance; callers cannot choose its trust source. The local store observation API is for trusted host integrations, not an agent RPC.

Applicable Constraints and AntiPatterns recommend REPLAN. A matching pinned executable Recovery can recommend RECOVER; abstract recovery remains guidance. Unresolved conflicts recommend EXPERIMENT only when existing capability/risk checks permit, otherwise approval. External policy blocks and approval requirements take precedence over learned exceptions.

The existing development `ExperienceContextBundle` carries an optional `knowledge` section with primary guidance, refinements, exceptions, Constraints, Recovery, AntiPatterns, unknowns, conflicts and provenance. `KnowledgeContextBudget` configures item counts (defaults 3/3/3/3/2/2/3/2). Full resolution and trace remain in local records. Existing native adapter response formats may display the translated runtime intervention rather than every structured field; the bridge JSON carries the bundle.

Every recorded decision binds a snapshot, immutable artifact references, context hash and policy version. Effect commit checks the exact session/action decision, current hierarchy hashes and refreshed observations. Changed guidance requires a new action/resolution. Repeat action IDs cannot reuse cached hierarchy decisions. The check never grants effect authority. It is a pre-commit check, not a transaction spanning a remote service; effect adapters retain responsibility for atomic token validation at execution.

`hardknock why <decision-id>` includes snapshot and resolution trace. JSON includes structured applied, suppressed, unknown and conflicting knowledge. Application outcomes remain unclassified until an explicit evidence-backed attribution is supplied; stored completed controlled experiments are required. A comparison's existence does not by itself prove a human attribution is semantically correct.
