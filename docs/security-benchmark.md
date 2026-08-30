# Execution-boundary security benchmark

Run the deterministic report with a fresh dedicated home:

```bash
export HARDKNOCK_HOME="$(mktemp -d "${TMPDIR:-/tmp}/hardknock-v09.XXXXXX")"
hardknock capability benchmark --output ./v09-security.json
```

The benchmark compares three arms and reports only attempted actions. A zero numerator with a zero denominator is `unobserved`, never a security success.

| Arm | Runtime observed | Denied-capability access | External mutation bypass | Known credential exposure |
| --- | --- | ---: | ---: | ---: |
| Git worktree | yes | 1/1 succeeded | 1/1 succeeded | 1 |
| Container baseline | no | 0/0 | 0/0 | 0 |
| Hardknock capability policy | no container runtime; policy requests observed | 0/4 succeeded | 0/1 succeeded | 0 |

The checked-in report is [v09-execution-boundary-summary.json](benchmarks/v09-execution-boundary-summary.json). The worktree arm uses a synthetic secret outside its workspace and a controlled external mock mutation, demonstrating that cooperative Git separation does not stop either action. The Hardknock arm submits exact forbidden filesystem, network, credential, and commit requests through policy/manager checks; all are denied and the mock external state remains unchanged.

The container-baseline arm was not run because neither Docker nor Podman was available. The report therefore does not claim observed mount, network, credential, or mutation isolation for a container. Provider startup overhead, execution latency, and memory overhead also were not measured; a truthful performance comparison is unavailable from this host.

## Metrics

`CapabilityEscapeRate = successful denied-capability accesses / denied-capability attempts`.

`EffectBypassRate = unauthorized authoritative mutations / bypass attempts`.

`CredentialExposureRate` counts known raw brokered secrets in persisted benchmark captures. These metrics cover only the explicit benchmark attempts. They are not probabilities of escape under arbitrary hostile code.

## CI layers

| Layer | Requirements | Purpose |
| --- | --- | --- |
| `unit` | Rust toolchain | model, policy, persistence, command construction |
| `integration-local` | Git and local shell | Bridge, worktree, effects, learning, security controls |
| `integration-container` | Docker/Podman daemon | live mounts, network none/allow-list, non-root process, cleanup, credential demo |
| `optional-real-adapters` | disposable PostgreSQL URL | invariant, stale CAS, commit receipt, idempotency |

The default pure suite must pass without Docker or PostgreSQL. Runtime-equipped CI should publish its own benchmark artifact rather than replacing an unobserved denominator with an inferred result.
