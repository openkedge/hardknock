# Hardknock V0.10 Implementation Report

## 1. Files created and changed

Portable tool models live in `src/tool.rs`, runtime providers and the router in
`src/tool_runtime.rs`, exposure measurement in `src/tool_benchmark.rs`, and
CLI handlers in `src/cli/tools.rs` and `src/cli/attestation.rs`. Persistence is
in `src/store/tools.rs` with migration 013. The V0.10 tool, sandbox, WASI,
attestation, and minimization documents describe the trust boundary.

## 2. Schema migration

Migration 013 adds immutable tool definitions, micro-sandbox records,
execution attestations, and append-only lifecycle events. Migration 012 data
continues to deserialize as cooperative Reality evidence.

## 3. ToolDefinition and manifest

`ToolDefinition` is versioned and named, with native, script, WASI, effect
adapter, and custom invocation variants. `hardknock-tool.toml` supports the
portable native/effect subset and `$WORKSPACE`/`$TMP` virtual paths.

## 4. Tool registry

`ToolRegistry` validates before registration and supports register, lookup,
list, verify, and disable. The SQLite `ToolStore` preserves definitions and
trust state across CLI invocations. Imported and federated definitions are
disabled by default.

## 5. Integrity and trust

Manifest hashes cover identity, invocation, schemas, and capabilities while
excluding the integrity field. Native files are hashed when locally available;
WASI definitions carry their artifact hash. Signatures identify origin but do
not prove safety. Local, built-in, imported, and federated sources remain
distinct.

## 6. Capability intersection

Effective authority is the intersection of Reality, tool declaration, and
active temporary grants. Filesystem scopes, network endpoints, environment
values, credentials, process patterns, Effect scopes, resources, and duration
are narrowed independently. Empty or non-overlapping network intersections
become `none`; no tool can add an endpoint or commit authority.

## 7. MicroSandboxProvider

The async provider abstraction owns create, execute, destroy, and guarantees.
The router resolves the tool, validates input, selects a runtime, executes once,
collects a result, attests it, and destroys the sandbox. Lifecycle failures are
persisted as visible events.

## 8. Container provider

`ContainerMicroSandboxProvider` creates an ephemeral Docker/Podman child with a
read-only root, a read-only Reality workspace plus only declared writable
submounts, temporary storage, dropped capabilities, no-new-privileges, no
ambient credentials, no network by default, and CPU/memory/PID limits. It
refuses silent host fallback.

## 9. Host provider

`HostMicroSandboxProvider` exists only for explicit trusted development. It
clears inherited environment, records `Observed` assurance, and documents that
host filesystem and process isolation are absent.

## 10. WASI status

WASI invocation and provider types are modeled, but Wasmtime is not included in
this pass. A WASI request is rejected when no strong WASI runtime is available;
there is no silent host downgrade. See `docs/wasi.md`.

## 11. Filesystem isolation

Virtual tool scopes are normalized and intersected with absolute Reality
scopes. Traversal and dot components are rejected. Container mounts are chosen
from the effective scopes; designated writing tools may receive workspace RW,
while read-only tools retain a read-only mount.

## 12. Network isolation

Network mode and endpoint allow-lists intersect exactly. A tool requesting an
endpoint absent from the parent Reality is denied. The container provider uses
`none` unless an explicitly supported runtime policy says otherwise; arbitrary
HTTP interception is not claimed.

## 13. Credential lifetime

Tool manifests can name scoped credentials, but credentials are not copied into
attestations. A child receives only explicit environment values and future
credential broker references. Each invocation has a new sandbox ID and no
process state is reused.

## 14. Execution attestation schema

`ExecutionAttestation` records tool identity, invocation, tool/Reality/effective
manifest hashes, input and output hashes, Effect references, result, timing,
runtime guarantees, assurance, and optional artifact references. A separate
`ToolExecutionReceipt` points to outputs and Effects without overloading commit
receipts.

## 15. Attestation verification

Verification recomputes canonical record contents, compares stored record hash,
checks current manifest hashes when available, checks referenced artifact
hashes, and requires basic provenance. It does not prove execution against a
malicious host, and V0.10 is not hardware-backed.

## 16. Lifecycle evidence

The router records requested, resolved, capabilities-computed, sandbox-created,
started, completed, attested, destroyed, and failed events. The SQLite event
table is append-only and can be inspected with `hardknock tool audit`.

## 17. Built-in tools

The catalog includes `read-file`, `write-file`, `run-tests`, `git-diff`,
`package-metadata`, and credentialless `effect-request`. `shell-generic` is
registered separately as an explicit higher-authority development path and is
excluded from the specialized-tool exposure benchmark.

## 18. Effect integration

`effect-request` emits a structured host request. The tool can request propose
or prepare within its intersection, but commit remains outside the sandbox and
must pass the existing Effect Manager authorization gate.

## 19. Agent adapters

`AgentToolAdapter` exposes names and input/output schemas without duplicating
capability semantics. Existing Claude/Codex/Hermes/OpenClaw adapters can adopt
the canonical definitions incrementally; arbitrary shell support remains.

## 20. Input and output validation

JSON object inputs are checked before launch. Declared object/array output
schemas are checked before a successful result is returned; malformed output is
reported as `InvalidOutput`.

## 21. Resource and duration limits

Effective memory, PID, CPU, output, and duration limits use the narrower parent
and tool values. Output truncation is explicit. Timeouts return a typed result
and remain part of the attestation.

## 22. Replay semantics

Input values are stored as hashes by default to avoid persisting secrets. The
CLI exposes replay as an explicit limitation until an input artifact is
provided. `ExecutionAttestation::compare_replay` compares compatible new
attestations as `ReplayMatch` or `ReplayDivergence`; original attestations are
never overwritten.

## 23. Capability minimization

`MinimizeCapability` and `MinimizeCapabilityExposure` provide curriculum and
comparison vocabulary. `hardknock tool benchmark` reports raw authority
dimensions, per-capability configured maximum exposure windows, and resolution
overhead without inventing a scalar security score.

## 24. Security tests

Tests cover portable manifest traversal and schema validation, process/network/
Effect intersection, scoped container arguments, Effect commit denial,
imported-tool disabling, manifest/executable/artifact tampering, replay
comparison, secret-environment noninheritance, sandbox expiration, lifecycle
persistence, credentialless Effect requests, and explicit host fallback. Live
container tests remain optional because this host does not provide Docker/Podman.

## 25. Performance

The benchmark records capability-resolution milliseconds and configured
network/credential/write/Effect exposure windows. These are manifest timeout
ceilings, not observed wall-clock measurements. Container startup timing stays
unset on this host. Per-tool containers may cost more startup time than a
session container; V0.10 optimizes correctness and authority duration first.

## 26. Known limitations and deviations

There is no WASI runtime, syscall mediation, transparent arbitrary HTTP
virtualization, hardware attestation, remote executable marketplace, or
production credential broker. Container allow-list networking still requires a
runtime-specific enforceable policy. Host execution is intentionally labeled
non-isolated.

## 27. Recommended V0.11 direction

Build Certified Experience Packages: a Skill plus behavioral contract,
curriculum, operating envelope, lessons, recoveries, reflexes, attestations,
and an evidence manifest. Do not begin that layer until live container
acceptance and multi-tool authority narrowing are measured in CI.
