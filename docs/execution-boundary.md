# Agent execution boundary

Hardknock V0.9 separates the trusted host control plane from agent-controlled execution. The host may reason broadly, select experience, evaluate results, and hold external credentials. Commands and file operations for a container Reality pass through a capability-aware execution proxy.

```text
trusted host: reasoning, policy, evaluation, credentials, Effect Manager
                              |
                    signed Reality token
                              |
                   execution/file proxy
                              |
agent process -> capability-isolated container -> scoped Effect relay
```

The boundary exists to permit useful failure without granting the agent all of the host's authority. It does not replace the empirical loop: a container run still creates an Execution, Evaluation, Experience, filesystem diff, and execution-assurance record.

## Trust boundary

The Hardknock process, SQLite store, container daemon, host Git client, evaluator, Effect Manager, and configured effect adapters are trusted. Agent commands and content derived from a task, repository, model, or tool response are untrusted. A prompt injection has the same authority as the agent process and must not be able to expand the manifest.

Container Realities use a read-only root filesystem, a single read/write workspace bind, a read-only Reality control bind, a bounded `/tmp`, a non-root UID, dropped Linux capabilities, `no-new-privileges`, process and memory limits, and an explicit Docker/Podman network mode. The Docker socket and host home are not mounted. Environment variables are constructed from the manifest rather than inherited.

Capability policy is deny by default. A request must match its filesystem, process, network, credential, or Effect scope exactly. A valid token proves only that the request came through the named Reality under a particular manifest revision; it does not authorize a broader target or commit.

## Reasoning and execution

V0.9's robust integration mode keeps agent/model reasoning on the host and runs the requested command in the Reality. This avoids placing model credentials in the container. `hardknock run --provider container` creates the container, issues a short-lived capability token, invokes the shell proxy, evaluates the resulting workspace from the trusted host, records evidence, and discards the container unless retention is requested.

Automatic retry/reflection orchestration is rejected for container runs in V0.9 because those secondary executions are not yet routed through the same proxy. The trusted evaluator also runs on the host against the controlled worktree. These facts appear in the execution assurance and are material when interpreting evidence.

## Security levels

Git worktrees report cooperative filesystem separation and no process, network, or credential isolation. Container Realities report container-level filesystem, process, network, and credential isolation, plus gated supported effects. They are not reported as `strong_sandbox`: containers share the host kernel and the daemon remains privileged infrastructure.

The provider selector rejects a provider whose declared isolation cannot satisfy a `RealityRequirements` value. It never silently labels a Git worktree as capability-isolated. `reality show` and `reality inspect` expose the provider, manifest identifier/hash/revision, image digest when observed, frozen state, credentials, violations, pending effects, and diff.

## Freeze, revocation, and disposal

`hardknock reality freeze` stops the container, revokes issued capability tokens and credentials, prevents new execution or Effect preparation, and leaves metadata/workspace state available for inspection. Capability revocation creates a new immutable manifest revision and removes the old published token. Disposal removes the container, dedicated network, per-Reality relay, token, and credentials, then disposes the underlying worktree. Selected logs, patches, evaluations, Experiences, and audit events survive.

## Observed limits

The pure test suite exercises policy decisions, path and symlink rejection, token tampering, credential redaction, Effect scope/commit denial, container command construction, and Bridge relay authentication. The V0.9 development machine did not have Docker or Podman, so container filesystem and network behavior was not observed live. Likewise, the optional PostgreSQL integration test compiled but was skipped because no test server URL was configured. See [Security benchmark](security-benchmark.md), [Threat model](threat-model.md), and [Container Realities](container-realities.md).
