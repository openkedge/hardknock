# Micro-Sandboxes

The parent Reality supplies context. Each named tool receives a fresh
`MicroSandbox` with an effective capability set:

```text
Reality capabilities ∩ Tool declaration ∩ active temporary grant
```

The intersection never adds a network endpoint, writable path, credential, or
Effect scope. Effect commit is always outside the tool boundary.

The container provider creates one disposable Docker/Podman child per
invocation with a read-only root, the Reality workspace mount, an explicit
temporary filesystem, dropped capabilities, no-new-privileges, resource
limits, and no ambient credentials. It removes the child after capture and
refuses to silently fall back to the host runtime.

The workspace bind is read-only unless a tool declares the full workspace
writable. Narrow writable directories are separate bind or temporary mounts.
Plain Docker cannot enforce arbitrary internet endpoint allow-lists, so those
requests are denied and recorded as such until a runtime-specific network
policy provider is configured.

The host provider is an explicit trusted-development escape hatch and reports
`Observed` with no isolation claim. The WASI provider is modeled as an
experimental runtime and currently returns an unavailable-runtime error rather
than claiming controls it cannot enforce.
