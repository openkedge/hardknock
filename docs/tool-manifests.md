# `hardknock-tool.toml`

Portable manifests use `schema = "hardknock.tool.v1"`. Paths are virtual and
may use `$WORKSPACE` or `$TMP`; traversal, malformed endpoints, unbounded
resources, unknown invocation types, and Effect commit authority are rejected
before registration.

```toml
schema = "hardknock.tool.v1"
name = "run-tests"
version = "1.0.0"

[invocation]
type = "native"
executable = "pnpm"
args = ["test"]

[capabilities.filesystem]
read = ["$WORKSPACE/**"]
write = ["$WORKSPACE/.cache/**", "$TMP/**"]

[capabilities.network]
mode = "none"

[capabilities.effects]
propose = false
prepare = false
commit = false

[resources]
memory_mb = 1024
pids = 64
timeout_seconds = 300
```

Manifests are hashed independently of their integrity field. Native binaries
and scripts are hashed when their local path is available; WASI artifacts carry
their artifact hash. A hash proves content identity, not safety.
