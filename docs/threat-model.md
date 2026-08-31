# V0.12 threat model

## Adaptive runtime threats

The runtime controller is a trusted local policy component, but its evidence,
agent reports, repository context, external state, and tool outputs may be
wrong or adversarial. A runtime decision is a recommendation or control
classification, not a new capability grant.

| Threat | V0.12 control | Remaining boundary |
| --- | --- | --- |
| Experience overrides hard policy | Security policy, missing capability, isolation, Effect-adapter, and authority checks precede Experience | A misconfigured permissive policy remains operator error |
| Retrieved match presented as knowledge | `KnownSupported` requires local support, scope, freshness, and no contradiction/gap | Unobserved context changes cannot be inferred |
| Stale or out-of-scope certification authorizes action | Exact Skill revision, status, revocation, expiry, profile, action, environment, and risk applicability | Context fingerprints only cover recorded dimensions |
| Signed remote evidence becomes local authority | Federated signals remain advisory and cannot independently authorize act/recover/replan | Operators may explicitly reproduce and promote them locally |
| Agent uncertainty derived from private reasoning | Only explicit reports and observable signals are accepted | An agent can omit or misreport uncertainty |
| One numeric risk score hides dimensions | Severity, reversibility, externality, assurance, Effect risk, authority, and isolation remain separate | Structured adapters can still classify a target incorrectly |
| Unsafe automatic experiment | Reality availability, Effect safety, budget, duration, and isolation requirements are mandatory | Git worktree experiments retain their documented cooperative boundary |
| Approval used to launder missing evidence | Approval is selected only for supported preparation with external authority missing; unsupported actions abstain | A user can still authorize risk outside Hardknock |
| Runtime over-intervenes | Outcome feedback measures unnecessary intervention; false-positive Reflex feedback disables and lowers a new revision | Sparse or dishonest feedback can leave a bad policy uncorrected |
| Decision record forged or rewritten | Stable context hash, deterministic re-evaluation, immutable policy contents and SQLite triggers | A host user controlling SQLite/filesystem can replace the whole store |
| Persistence pressure blocks action path | Bounded hot cache and ordered writer; enqueue fails closed when unavailable/full | Host crash after guidance and before queued commit can lose the record |
| Gap feedback creates runaway autonomous work | Curriculum recommendations set `auto_run: false`; no background daemon | An operator may explicitly start expensive curricula |

The synchronous Bridge path performs deterministic cache lookup and policy
evaluation without an LLM. This reduces latency and prompt-injection surface,
but cache freshness and invalidation become trusted implementation concerns.
CLI/direct-run context synthesis reads current SQLite state. Remote refresh,
cold container creation, and model latency are not part of the reported hot
path benchmark.

## Runtime security invariants

1. `ACT` never bypasses capability, isolation, Effect, approval, or commit enforcement.
2. Hard security denial wins over all Experience and assurance evidence.
3. Remote advisory evidence cannot independently authorize local action.
4. `REPLAN`, security block, `REQUIRE_APPROVAL`, and `ABSTAIN` remain distinct.
5. Replay creates a new record and never edits the original decision.
6. Decision feedback is evidence about control quality, not proof that the original policy was correct.
7. Runtime gaps may recommend curriculum but cannot start it automatically.

## Certification threats

| Attack | Mitigation | Remaining boundary |
| --- | --- | --- |
| Cherry-picked successes | Deterministic Skill/profile selection includes in-scope failures and contradictions | Evidence outside the recorded local graph is unknown |
| Duplicate evidence inflation | Manifest references are sorted and deduplicated; coverage uses distinct configured conditions | Semantically duplicate observations with different IDs need future similarity analysis |
| Stale certificate | Exact Skill/contract/profile revisions plus tool/runtime hashes and freshness policy | Dependency changes absent from evidence cannot be inferred |
| Manifest or artifact mutation | Stable BLAKE3 manifest hash and Ed25519 signature over the whole unsigned artifact | A compromised host identity can sign false assertions |
| Contract weakening/profile downgrade | Immutable revisions, explicit profile versions, and visible contract diff warnings | Legitimate weakening is permitted after disclosure |
| Capability laundering | Contract maximum and profile capability checks remain separate from behavior | Host observations provide `Observed`, not isolated assurance |
| Critical failure averaged away | Any required Critical invariant or forbidden outcome blocks deterministically | A failure cannot block if its state/effect was unobservable |
| Remote trust transitivity | Valid remote `.hkcert` reports authentic but local certification false | Local reproduction remains an explicit later action |
| Executable substitution | Tool artifact/runtime hashes bind the certificate and changes trigger review | Hardware-backed execution proof is deferred |

The signing key authenticates the producer's assertion. It does not prove the
producer was honest, the host was uncompromised, the contract was strong, or
the evidence applies locally.

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

V0.10 adds a per-tool boundary inside a Reality. Tool definitions are hashed
and registry entries can be disabled; imported or federated executables remain
disabled by default. The effective capability set is an intersection, so a
tool cannot add a network endpoint, writable path, credential, or Effect scope
that the parent Reality lacks. Each invocation gets its own sandbox lifecycle
and attestation. Host fallback is explicit and reports no isolation.

The deterministic security suite proves policy-level rejection for traversal, symlinks, dangerous mounts, token tampering/revision/revocation, effect escalation, secret persistence/redaction, and commit bypass. It inspects the exact container create arguments. The benchmark honestly observes the worktree control and host-level Hardknock policy arm.

No Docker/Podman runtime was present for this pass, so claims that require live namespaces, mounts, daemon networking, or a container escape attempt remain unobserved. No PostgreSQL URL was present, so the real adapter's live transaction sequence remains an optional integration test. See [Security benchmark](security-benchmark.md) for denominators and limitations.
