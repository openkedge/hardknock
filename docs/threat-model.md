# V0.9 threat model

## Assets and trust

Protected assets are host files and credentials, the container daemon, unrelated network services, authoritative external systems, Effect commit authority, capability signing keys/tokens, and the integrity of Hardknock evidence. Hardknock's host process, local operator, SQLite store, provider daemon, evaluator, Effect Manager, and configured adapter code are trusted. Agent code, repository content, prompts, model output, dependencies, tool output, and external content are untrusted.

V0.9 targets a malicious or misbehaving agent running ordinary code in one container Reality. It is not a hostile multi-tenant hosting boundary and does not defend against a compromised host, daemon, kernel, Hardknock process, local operator, or adapter implementation.

## Threats and controls

| Threat | V0.9 control | Residual risk |
| --- | --- | --- |
| Prompt injection or agent misuse | Untrusted instructions cannot edit the signed manifest; proxy and Effect Manager check requests | The agent can use every capability it legitimately has |
| Host filesystem read/write | Read-only root, one workspace RW bind, narrow control RO bind, canonical path policy | Container/runtime/kernel vulnerability; worktree Git objects remain host-managed |
| `..` path traversal | normalized relative path resolution beneath canonical workspace | Bugs in new proxy operations must repeat the same check |
| Symlink escape | canonical parent/target verification rejects paths outside workspace | Race between validation and host file operation is reduced, not eliminated by openat-style handles |
| Docker socket escape | socket and daemon credentials are never mounted | A daemon or container-runtime vulnerability remains trusted-infrastructure risk |
| Host/network bypass | `--network none` or dedicated `--internal` fixture network; host networking is never requested | Attached fixture containers expose all their network ports to peers; live behavior needs runtime CI |
| DNS bypass | no egress/DNS path in none or internal-only mode | Public-host allow-list is not implemented; unrestricted mode has no isolation |
| Ambient secret exposure | constructed environment; no home/cloud/SSH mounts | Secrets copied into the repository/workspace are in scope |
| Issued credential leakage | exact scope/expiry, private runtime material, redaction, revoke/delete | Agent can transform or exfiltrate a secret during its valid lifetime if network/effect scope permits |
| Capability token theft | short expiry, signature, exact Reality/manifest hash/revision/operations, store revocation | A process in the same Reality that steals the token shares that Reality's authority until expiry/revocation |
| Token tampering/replay | Ed25519 signature, current Reality and revision check, token audit/revocation | Signing key and SQLite are local trusted assets, not HSM/tamper-proof storage |
| Direct Effect bypass | deny network/credentials; expose only scoped host relay for supported targets | Arbitrary syscalls/HTTP are not transparently intercepted; a mistakenly granted network credential can bypass adapters |
| Confused deputy through adapter | token first, then exact Reality capability, kind, target, operation, payload, lifecycle action | Adapter validation bugs remain security-sensitive |
| Unauthorized commit | agent relay and `hk-effect` omit commit; manifest defaults `commit:false`; Manager rejects agent authority | The trusted local user/host can commit with explicit authorization |
| Prepare/commit TOCTOU | prepared fingerprint/version, expiry, commit-time re-read/CAS | External systems without strong adapter primitives may return stale/unknown outcomes |
| PostgreSQL injection/escalation | structured mutation, bounded identifier grammar, parameterized values, configured alias/table | Configured DB role must itself be least-privileged |
| Secret in evidence | output redactor and no raw SQLite credential storage | Encoded/transformed secrets and secrets unknown to broker may survive |
| Resource exhaustion | CPU, memory, PID, timeout, output and tmpfs limits | Host disk consumed by workspace/Git artifacts is not a hard quota in V0.9 |
| Evidence tampering | immutable SQLite triggers and hashes | A host user with database/filesystem access can replace the database or keys |

## Security invariants

1. A Git worktree is never described as an enforced capability boundary.
2. A request denied by the manifest is not dispatched by the proxy or Effect Manager.
3. A valid Reality token cannot authorize another Reality, manifest revision, expired session, or undeclared operation.
4. Agent proposal/preparation never implies commit authority.
5. The Effect Manager validates kind, structured target, operation, payload scope, and lifecycle action after authenticating the caller.
6. Raw brokered secret bytes are not persisted to SQLite and are redacted from known capture paths.
7. Freeze revokes tokens/credentials and prevents new execution/preparation while retaining inspection evidence.

## Validation status

The deterministic security suite proves policy-level rejection for traversal, symlinks, dangerous mounts, token tampering/revision/revocation, effect escalation, secret persistence/redaction, and commit bypass. It inspects the exact container create arguments. The benchmark honestly observes the worktree control and host-level Hardknock policy arm.

No Docker/Podman runtime was present for this pass, so claims that require live namespaces, mounts, daemon networking, or a container escape attempt remain unobserved. No PostgreSQL URL was present, so the real adapter's live transaction sequence remains an optional integration test. See [Security benchmark](security-benchmark.md) for denominators and limitations.
