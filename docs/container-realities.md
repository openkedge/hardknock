# Container Realities

`ContainerRealityProvider` layers Docker- or Podman-managed execution over Hardknock's exact Git snapshot/worktree substrate. The worktree remains the source of diff and artifact evidence; the container limits the agent command's view and authority.

## Creation

```bash
hardknock --repo /path/to/clean/repo reality create \
  --provider container --profile coding-offline \
  --image debian:bookworm-slim

hardknock reality execute <reality-id> -- /bin/sh -lc 'make test'
hardknock reality inspect <reality-id>
hardknock reality diff <reality-id>
hardknock reality freeze <reality-id>
hardknock reality discard <reality-id>
```

`hardknock run --provider container --capabilities coding-offline` performs the same lifecycle for one agent execution. Container runs default to `coding-offline` and `debian:bookworm-slim` when those flags are omitted.

The runtime command applies:

- a read-only container root;
- only `/workspace` as a read/write host bind;
- a narrow, read-only `/run/hardknock` control bind;
- `/tmp` as `nosuid,nodev,noexec` tmpfs;
- the invoking non-root numeric UID/GID so the exact worktree stays writable (or `65532:65532` when Hardknock itself runs as root), all Linux capabilities dropped, and `no-new-privileges`;
- manifest CPU, memory, PID, timeout, and captured-output limits;
- explicitly constructed `HOME`, `PATH`, and locale values;
- `--network none` for none/loopback-only, a dedicated internal network for allow-list, or Docker bridge only for an explicitly unrestricted manifest.

No host home, SSH agent, cloud credential directory, Docker socket, host network, or privileged flag is added. Image metadata stores the resolved digest when the runtime returns one. Tags alone are not treated as reproducible identifiers.

## Network allow-list implementation

The V0.9 implementation is deliberately narrow. It creates a per-Reality `--internal` network and connects only fixture containers whose exact names appear in the manifest. It has no internet gateway. The declared port is policy/audit scope, but Docker attachment itself does not filter ports between attached containers. Public DNS allow-listing, transparent egress interception, and a zero-trust per-port proxy are deferred.

`NetworkMode::LoopbackOnly` currently maps to `--network none`: container loopback remains usable inside the namespace, while host loopback is not. `Unrestricted` maps to the runtime bridge and is visibly reported as no network isolation.

## Process and file proxy

The shell proxy verifies the token, current manifest hash and revision, expiry, revocation state, frozen state, and process capability before `docker exec`. For an active scoped test credential it creates a unique read-only per-action secret file under the control mount and injects only its path; the file is removed after capture. The proxy enforces a timeout by killing the container process, bounds output, redacts issued secret bytes, and records allow/deny events. The file proxy performs host-mediated reads, writes, deletes, and listings only after canonical workspace path validation.

This is not syscall-level mediation. A binary already inside the container executes under the container boundary and network mode, not through a per-syscall policy engine. Container cancellation beyond the command timeout is less complete than the Git `ProcessRunner` process-group path in V0.9.

## Effect relay

While Bridge is running it creates a Unix socket in the private per-Reality control directory and exposes it at `/run/hardknock/bridge.sock`. `hk-effect` reads the Reality token from the same read-only bind. The relay verifies both signature and exact Reality binding before accepting propose, status, or discard. It intentionally exposes no commit command.

Unix socket path limits apply to unusually long `HARDKNOCK_HOME` values. The default home is within common macOS/Linux limits; use a short dedicated path for tests if necessary.

## Failure cleanup

Creation and execution failures attempt to remove the container, internal network, token, relay, credentials, and worktree. Runtime crashes can still leave daemon-side resources; `hardknock reality cleanup` reconciles known orphaned Realities. SQLite audit/history is append-only by trigger but is not a tamper-proof external log.

## Runtime verification status

The exact runtime arguments and failure paths have unit/security coverage. No Docker/Podman executable was available during the committed V0.9 pass, so the project does not claim live observation of root filesystem, network, resource-limit, or cleanup behavior on that machine. CI should run the `integration-container` layer on a rootless-compatible runtime host.
