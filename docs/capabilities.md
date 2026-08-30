# Execution capabilities

A `CapabilityManifest` is an immutable, revisioned statement of what one Reality may request. It contains:

| Domain | Scope |
| --- | --- |
| Filesystem | normalized absolute readable and writable roots |
| Process | execution enabled/disabled, optional executable patterns, process limit |
| Network | none, loopback-only, allow-list, or unrestricted; host and port entries |
| Environment | explicit names and constructed values |
| Credentials | provider, name, resource, permissions, and optional expiry |
| Effects | propose/prepare/commit booleans plus kind, target, and operation scope |
| Resources | CPU, memory, PID, timeout, and captured-output bounds |

The canonical JSON hash, manifest identifier, and revision are stored on the Reality, copied into execution assurance, and signed into its capability token. History is append-only. Escalation never edits an old manifest in place.

## Policy semantics

`DenyByDefaultCapabilityPolicy` returns `allow`, `deny`, or `approval_required` with a reason. Empty Effect scope means no authority; every enabled Effect grant must explicitly list kinds, target patterns, and operations. Filesystem requests are resolved beneath the canonical workspace root; parent components, an absolute user-supplied path, and symlinks escaping the workspace are rejected. Network allow-list entries match an exact host and port. Credentials match provider, name, resource, and requested permissions. Effects match all of kind, structured target pattern, operation, and lifecycle action.

A provider check happens before execution. Container isolation is required for a capability-isolated request; Git worktrees cannot accept a capability profile. Policy decisions and violations are immutable events. Denial is evidence of an attempted action, not evidence that the underlying container stopped every equivalent syscall.

## Built-in profiles

| Profile | Network | Credentials | Effects |
| --- | --- | --- | --- |
| `coding-offline` | none | none | none |
| `coding-networked` | allow-list for declared package/API endpoints | none | none |
| `effect-test` | none | none | scoped mock HTTP/database/message/shadow targets; no commit |
| `staging-agent` | empty allow-list until configured | none | database/deployment with scoped shadow targets; no commit |
| `coding-effect-test` | none until a local DB fixture is explicitly configured | none | scoped test database targets; propose/prepare, no commit |

Profiles are starting points. `coding-networked` names public hosts, but the V0.9 container provider implements allow-list mode only by attaching explicitly named local fixture containers to a dedicated internal network. It therefore rejects/unavailable public-host use rather than silently granting internet access.

## CLI

```bash
hardknock capability list
hardknock capability show coding-offline
hardknock capability validate ./manifest.json
hardknock capability diff coding-offline coding-networked
hardknock capability explain <reality-id> --request '<CapabilityRequest JSON>'
hardknock capability audit --reality <reality-id>
hardknock capability revoke --reality <reality-id> network
hardknock capability benchmark --output ./security-report.json
```

`revoke` supports network, process, credentials, and effects. It creates a new revision, revokes existing tokens, removes the published token, and requires a newly issued token before another operation. V0.9 does not let the agent approve its own escalation and does not implement resume after freeze.

## Capability events as experience

Allow, deny, approval-required, token, credential, manifest revision, and freeze events are recorded separately from agent stdout. Experience records carry execution assurance: provider truth, manifest hash/revision, image digest if the runtime supplied one, and whether the Reality was frozen. Federation can preserve this provenance without treating the manifest as a guarantee stronger than its provider.
