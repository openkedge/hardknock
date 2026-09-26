# Compatibility matrix

This matrix records the compatibility evidence present in the repository for
the Milestone 4 generic agent integration. Hardknock remains pre-release. The
MCP contract is preview, and external live acceptance across agent hosts has
not been completed.

## Generic MCP contract

| Item | Repository-backed status |
| --- | --- |
| Launch command | `hardknock mcp serve --stdio --workspace PATH` |
| Installation contract | `hardknock integration manifest` |
| Transport | Newline-delimited MCP JSON-RPC over stdio |
| Protocol | `2026-07-28` |
| Stability | Preview |
| Session model | Stateless requests with an explicit `hardknock_session_id` returned by `hardknock_query_context` and required by later stateful calls |
| Backend | Authenticated local Hardknock Bridge and the local evidence store |
| Excluded authority | Approval grants, external-Effect commits, command execution, filesystem execution, and generic experiment creation |
| External live acceptance | Pending; no general agent-host acceptance claim |

The three advertised tools are:

| Tool | Capability | Effect |
| --- | --- | --- |
| `hardknock_query_context` | `context.query` | Read-only scoped context |
| `hardknock_record_outcome` | `outcome.record` | Records bounded local evidence |
| `hardknock_experiment_status` | `experiment.status` | Read-only progress for separately authorized experiments |

## Operating systems and architectures

The integration manifest and release workflow declare these combinations:

| Operating system | Architecture | Release target | Evidence boundary |
| --- | --- | --- | --- |
| Linux | `x86_64` | `x86_64-unknown-linux-gnu` | Declared manifest support and release build target |
| Linux | `aarch64` | `aarch64-unknown-linux-gnu` | Declared manifest support and release build target |
| macOS | `x86_64` | `x86_64-apple-darwin` | Declared manifest support and release build target |
| macOS | `aarch64` | `aarch64-apple-darwin` | Declared manifest support and release build target |

Windows and other operating-system or architecture combinations are outside
the repository-declared generic integration boundary. A declared target means
the repository contains the manifest entry and release build job; it does not
mean every host/version combination has completed live acceptance.

## Agent adapters

| Integration | Repository evidence | Current restriction |
| --- | --- | --- |
| Generic MCP host | Versioned manifest, bounded tool definitions, and model-free stdio/Bridge conformance tests | Protocol is preview; external live host acceptance pending |
| Claude Code | Deterministic native adapter fixtures | External live acceptance not established by Milestone 4 |
| Codex | Fixture-tested `codex-cli 0.149.1`; generated core-schema and initialization compatibility checks | Other conforming versions run as `core-schema-compatible-untested`; approval-schema compatibility is not claimed and unsupported inbound requests fail closed |
| Hermes | Deterministic native adapter fixtures | External live acceptance not established by Milestone 4 |
| OpenClaw | Deterministic native adapter fixtures | External live acceptance not established by Milestone 4 |

For Codex, `--allow-untested` acknowledges the warning for a
core-schema-compatible untested version. It does not override schema validation
or turn an incompatible App Server into a supported one.

Use `hardknock doctor --strict --json`, `hardknock integrate doctor`, and
`hardknock integration manifest` to inspect a specific installation. Their
results describe that local system and do not expand this published boundary.
