# Support and Compatibility Policy

Hardknock is currently pre-release. This policy defines the support model being
implemented for the scoped 1.0 release.

## Maturity levels

| Level | Meaning |
| --- | --- |
| Stable | Covered by the published platform and compatibility matrix, upgrade policy, release tests, and security support |
| Beta | Intended for real pilots; compatibility changes are documented and migration paths are supplied |
| Experimental | Available for evaluation with explicit opt-in; interfaces and stored representations may change |
| Unsupported | Outside the published operating boundary or no longer maintained |

Until 1.0, the repository and checkpoint tags are experimental unless a guide
explicitly says otherwise.

## Planned 1.0 operating boundary

Stable support is limited to:

- single-user developer workstations and dedicated CI runners;
- Linux and macOS on supported x86-64 and ARM64 targets;
- the bundled local SQLite store;
- local Unix-socket Bridge operation;
- published binary installation and managed user-level service operation;
- the generic CLI and MCP stdio contract;
- native agent versions listed in the generated compatibility matrix.

Hosted multi-user operation, remote Bridge access, production Effect adapters,
automatic continuous learning, Windows, and network service discovery remain
outside the initial stable boundary.

## Release support

After 1.0:

- the latest minor release receives fixes;
- the immediately preceding minor release receives critical security and
  migration fixes for at least 90 days after its successor;
- patch releases preserve stable command, configuration, wire, and database
  compatibility;
- removals require prior deprecation in a minor release;
- experimental interfaces may change in any release and must identify their
  maturity in human and JSON output.

Pre-1.0 checkpoints receive best-effort fixes on the current development
branch. Older checkpoints are not maintained.

## Data and upgrade compatibility

- Every schema change uses a new append-only migration.
- Upgrades create and verify a recoverable backup before migration.
- The supported upgrade path is from the previous stable minor release to the
  current release.
- A newer database is rejected by an older binary.
- Downgrade recovery uses the pre-upgrade backup; in-place down migrations are
  not promised.
- Stable exports and integration manifests carry explicit schema versions.

## Agent compatibility

Native agent support requires both protocol/schema conformance and a tested
version result. A compatible but untested version may run with an explicit
warning when required fields and authority semantics remain valid. A missing or
unsafe capability fails closed.

The compatibility matrix records:

- agent and version;
- operating system and architecture;
- detected lifecycle capabilities;
- model-free conformance result;
- live disposable acceptance result;
- known restrictions and last verification date.

## Support requests

A useful support report includes:

- `hardknock --version`;
- `hardknock doctor --strict --json` output with secrets removed;
- operating system, architecture, provider, and agent version;
- the failing command and exit code;
- whether the problem reproduces with a fresh dedicated Hardknock home;
- relevant bounded logs or artifact IDs.

Do not attach credentials, raw private prompts, production data, or an entire
Hardknock home to a public issue.
