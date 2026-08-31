# Portable Tools

Hardknock treats a named tool as an executable capability contract, not as an
arbitrary shell string. A `ToolDefinition` records its version, invocation,
input/output schemas, declared capabilities, integrity hashes, provenance,
trust, and disabled state.

The built-in catalog is intentionally small: `read-file`, `write-file`,
`run-tests`, `git-diff`, `package-metadata`, and `effect-request`. The separate
`shell-generic` definition is visibly broader and intended only as an explicit
power tool for trusted development. Imported or federated definitions are
disabled until a local operator approves them.

```bash
hardknock tool list
hardknock tool show run-tests
hardknock tool verify run-tests
hardknock tool audit
hardknock tool benchmark
```

`tool run` validates JSON input, intersects capabilities with the parent
Reality, selects an explicit runtime, stores lifecycle events and an
attestation, and destroys the sandbox. Host execution requires
`--runtime host --allow-host-fallback` and is labeled `Observed`.
