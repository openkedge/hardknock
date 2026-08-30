# V0.9 implementation report

Hardknock V0.9 adds an explicit agent execution boundary around the existing empirical-learning and transactional-Effect loops. The implementation is complete at the model, persistence, CLI, proxy, Bridge, adapter, and pure-test layers. Live Docker/Podman and PostgreSQL acceptance was not available on the development machine; those results are identified as unobserved rather than inferred.

## 1. Files created and changed

New capability modules cover the domain model, profiles, policy, provider selection, container runtime, signed tokens, credentials, execution/file proxy, and benchmark. New storage, CLI, `hk-effect`, PostgreSQL adapter, migration, dedicated security tests, benchmark artifact, and execution/security documentation accompany them. Existing Reality, Experience, workflow, Bridge, Effect Manager/registry, CLI, Store, roadmap, architecture, experiment, curriculum, federation, and development code now carry the boundary and assurance metadata.

The final committed file list is available from Git; the principal new paths are `src/capability/`, `src/store/capabilities.rs`, `src/cli/capability.rs`, `src/bin/hk-effect.rs`, `src/effects/postgres.rs`, `migrations/012_capabilities.sql`, `tests/security/`, and the V0.9 documents.

## 2. Schema migration

Migration 012 creates capability manifests, Reality manifest history, provider runtime metadata, append-only capability events, issued-credential metadata, capability-token audit/revocation, and security-benchmark reports. Manifest and event rows have immutable update/delete triggers. The application schema maximum advances from 11 to 12. SQLite and local keys remain trusted local storage, not a tamper-proof audit service.

## 3. Capability Manifest model

`CapabilityManifest` has a typed ID, profile, revision, filesystem/process/network/environment/credential/Effect capabilities, resource limits, creation time, bounded validation, and a canonical BLAKE3 hash. `ExecutionBoundary` records provider truth, provider security levels/limitations, manifest ID/hash/revision, resolved image digest, and frozen state on every Reality. `ExecutionAssurance` copies the relevant facts into evidence.

## 4. Capability policy semantics

The policy denies by default and records allow/deny/approval-required reasons. Filesystem roots are normalized and canonicalized; network entries match exact host/port; credential grants match provider/name/resource/permissions/expiry; Effect grants match kind/target/operation and lifecycle action. Provider selection refuses insufficient isolation rather than silently downgrading. A denial proves the request was rejected at this policy point, not that every equivalent kernel action was observed and blocked.

## 5. Built-in profiles

`coding-offline`, `coding-networked`, `effect-test`, `staging-agent`, and `coding-effect-test` are built in. All default to no credentials and `effects.commit=false`. Offline coding has no network. Networked coding declares package/API endpoints, while V0.9's implementation supports only named local fixture containers on an internal network. The flagship coding/effect profile scopes database effects to configured test targets.

## 6. Container Reality design

`ContainerRealityProvider` composes a Docker/Podman container with the exact Git worktree provider. It supports create, start/execute, fork from the original snapshot, diff, freeze, discard, and cleanup. Runtime metadata records executable, container ID/name, requested image, resolved digest, dedicated network, attached fixtures, and creation time. Disposal preserves selected evidence and removes ephemeral provider resources.

## 7. Filesystem isolation guarantees

The runtime command uses a read-only root, one read/write `/workspace` bind, a narrow read-only `/run/hardknock` bind, and a bounded `nosuid,nodev,noexec` `/tmp` tmpfs. Dangerous home/root/Docker/cloud mounts are rejected. Host-mediated file operations use canonical workspace resolution and reject traversal or symlink escape. Exact arguments and proxy checks are tested; no live mount namespace was available in this pass.

## 8. Process isolation guarantees

The container uses the invoking non-root numeric UID/GID so its exact worktree stays writable, falling back to UID/GID 65532 when Hardknock itself is root. It drops all Linux capabilities, sets `no-new-privileges`, and applies CPU/memory/PID/time/output bounds. The proxy verifies manifest/token/frozen state before `docker exec`, kills on timeout, and records the decision. Containers share the host kernel, and V0.9 does not add seccomp/AppArmor profiles or syscall mediation. Live PID/resource behavior was unobserved.

## 9. Network policy implementation

None and loopback-only use Docker `--network none`. Allow-list creates a dedicated `--internal` network and connects only exact named fixture containers. Unrestricted explicitly uses bridge and reports no network isolation. Host networking is never selected. Attachment does not enforce declared ports between attached containers, public hostname egress is not implemented, and no live network attempts ran on this host.

## 10. Environment sanitization

The provider constructs the environment from manifest values, with bounded `HOME`, `PATH`, and locale defaults. It does not inherit host AWS, Kubernetes, SSH, proxy, Git, shell, or arbitrary environment variables. The workspace/control mounts do not include corresponding host credential directories. Repository files remain agent-visible by design.

## 11. Credential broker design

`CredentialBroker` issues an exact scoped grant with expiry and lifecycle metadata. The local static broker is a test implementation: it keeps a private host copy, creates a unique read-only per-action file in the Reality control mount, injects only a path reference, registers raw bytes for output redaction, persists only identifiers/scope/lifecycle/opaque reference, and removes/revokes material on action/lifecycle end. It is not a production AWS/Kubernetes/OIDC broker. Host-side PostgreSQL credentials stay entirely in the adapter.

## 12. Capability token design

An Ed25519 authority stores a private mode-0600 key under the private Hardknock identity directory. A short-lived signed token binds token ID, Reality ID, manifest ID/hash/revision, issue/expiry time, and allowed operations. Verification checks signature, expiry, exact Reality/current manifest, operation, token audit presence, and revocation. Published token/relay files live in a per-Reality control directory mounted read-only in the container. Tokens never enter Experience payloads.

## 13. Execution proxy architecture

The shell proxy authenticates and authorizes before dispatch, enforces time/output limits, redacts known credentials, and emits immutable capability events. The file proxy performs read/write/delete/list through safe host workspace resolution. Bridge creates one Reality-bound Unix relay, and `hk-effect` offers propose/status/discard without commit. This is tool-level mediation plus the container boundary, not transparent syscall or HTTP interception.

## 14. Effect Manager enforcement changes

Agent proposals/preparations now require the Reality's exact manifest scope. The Manager checks kind, target pattern, operation, lifecycle permission, current token/revision, and frozen state. Agent commit requests remain denied even if the Effect is otherwise scoped; external local authority can supply a separate exact authorization. The Bridge overwrites caller-supplied Reality/session binding after authenticating the per-Reality relay to avoid confused-deputy substitution.

## 15. PostgreSQL adapter design

The host adapter loads private mode-0600 alias configuration, validates lowercase identifiers, accepts only structured insert/update/delete, parameterizes values, and rejects arbitrary SQL/table/operation escalation. Prepare reads and fingerprints state without a long lock. Commit rechecks expected version in a database transaction and writes an idempotency receipt in the same transaction. Reconcile reads receipts; discard is no-op; compensation requires a new reviewed Effect. Guarantees do not cross the one database boundary.

## 16. Agent integration mode

`hardknock run --provider container --capabilities PROFILE` keeps reasoning/model credentials on the host and executes the requested command through the container proxy. It captures diff/evaluation/Experience and assurance, then disposes the container. The trusted evaluator stays on the host. Automatic retry/reflection is rejected for container runs because those later commands are not yet routed through the proxy. Native V0.3 adapters remain observational host integrations.

## 17. Security tests

`tests/security.rs` and `tests/security/` cover deny-by-default/provider adequacy, traversal/symlink escape, dangerous mounts/container arguments, token tampering/revision/revocation, credential persistence/redaction/revocation, Effect scope and commit bypass, CLI audit/diff/benchmark, per-Reality Bridge token authentication, structured PostgreSQL scope, and an optional live PostgreSQL sequence. The container provider is pure-tested without requiring Docker.

## 18. Credential leakage results

The local security test issued a known synthetic secret, confirmed the raw bytes did not appear in SQLite or serialized issued-credential data, confirmed captured text was replaced by `[REDACTED]`, and observed revocation events/removal. The benchmark policy arm recorded zero known persisted exposures. A live container `env` capture was not run; transformed/encoded secret exfiltration is outside exact-byte redaction.

## 19. Network isolation results

Policy tests denied the forbidden network request and inspected `--network none`; container argument tests confirmed there was no host network flag. The checked-in benchmark has zero successful network-policy requests in the Hardknock arm. Because Docker/Podman was unavailable, attempts to reach host/external fixtures and the positive internal allow-list case are unobserved, not passing results.

## 20. Filesystem escape results

Pure tests rejected `/workspace/../...`, a symlink from the workspace to an outside synthetic secret, and dangerous root/home/Docker/cloud mounts while allowing a normal workspace file. The worktree benchmark control read its outside synthetic secret successfully. The container arm made no live attempt because no runtime was available.

## 21. Effect bypass results

The worktree control performed 1/1 direct synthetic external mutations. The Hardknock arm attempted one unauthorized scoped mutation/commit path and produced 0 authoritative mutations; a separately authorized external-user commit path is tested. Per-Reality relay token tampering is rejected and `hk-effect` has no commit. This result covers registered test adapters and exact attempts, not arbitrary network effects.

## 22. PostgreSQL transactional test results

Structured classification tests passed for valid scope and rejected raw-SQL-shaped payload, table escalation, and operation mismatch. The optional live test compiles and, when configured, creates isolated inventory/receipt tables to exercise invariant rejection, stale version conflict, reprepare, commit/version increment, persisted receipt, and idempotent retry. `HARDKNOCK_TEST_POSTGRES_URL` was unset in this pass, so the live sequence skipped and has no observed result here.

## 23. Provider performance comparison

No truthful runtime performance comparison is available. The Git worktree benchmark ran locally; the container provider never started because the host lacked Docker/Podman. Creation latency, steady-state command latency, disposal latency, CPU, memory, image-pull cost, and Podman/Docker differences must be measured in runtime-equipped CI with image digest and warm/cold state recorded.

## 24. Known security limitations

Containers share the host kernel and trust the daemon. There is no microVM, seccomp policy generator, syscall proxy, per-port internal-network enforcement, public hostname allow-list proxy, arbitrary HTTP interception, hard workspace disk quota, HSM, remote attestation, or tamper-proof event service. File validation does not use directory-handle/openat traversal throughout. Secret redaction covers known exact bytes. Unix socket path length can constrain unusually long homes. Cancellation/retry/evaluator paths are not uniformly proxied. See the complete [threat model](threat-model.md).

## 25. Deviations and rationale

The built-in public-host `coding-networked` declarations cannot receive public egress under the narrow internal-fixture allow-list implementation; failing closed was chosen over general bridge access. Kubernetes and AWS adapters, unsafe mounts, resume, transparent HTTP interception, and production credential exchange were deferred as allowed by the scope. The test broker uses synthetic local secrets. PostgreSQL uses version CAS plus transactional receipts instead of distributed prepared transactions. The security benchmark preserves unobserved container denominators rather than simulating a successful runtime.

## 26. Recommended V0.10 direction

After live V0.9 acceptance, proceed to **Hardknock V0.10 — Micro-Sandboxes, Portable Capability Tools, and Execution Attestation**. Move toward per-tool WASM/WASI or microVM execution, portable signed tool manifests, short-lived grants, stronger path/network primitives, and execution/artifact/effect attestations. Do not begin by broadening provider claims: first demonstrate on an actual runtime that an integrated agent can learn through useful experimentation while possessing substantially less authority than the Hardknock host.

## Verification

The required local gates are:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

The pure suite is intended to pass without Docker or PostgreSQL. Runtime-equipped CI must add live container and optional adapter layers and publish those results separately. The checked-in deterministic benchmark is [v09-execution-boundary-summary.json](benchmarks/v09-execution-boundary-summary.json).
