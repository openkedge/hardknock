# WASI Status

`ToolInvocation::WasiComponent`, `WasiMicroSandboxProvider`, and explicit
runtime selection are modeled so a WASI provider can be added without changing
tool manifests or attestations. The current development build does not include
Wasmtime and refuses a silent host or container downgrade when a WASI tool is
requested.

When enabled in a future pass, a provider must preopen only declared virtual
directories, construct an explicit environment, bound stdin/stdout, and report
which network controls it truly enforces. A component without a preopened
directory must not see that path. Host home directories, SSH material, and cloud
credentials are never implicit preopens.
