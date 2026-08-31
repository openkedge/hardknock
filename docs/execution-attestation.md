# Execution Attestation

Every routed tool execution records an `ExecutionAttestation` and a separate
`ToolExecutionReceipt`. The attestation includes tool identity and artifact or
manifest hashes, Reality and sandbox IDs, normalized invocation hash, effective
capability hash, input/output hashes, Effect references, result, timestamps,
and runtime guarantees.

```bash
hardknock attestation list
hardknock attestation show attestation-<uuid>
hardknock attestation verify attestation-<uuid>
hardknock attestation replay attestation-<uuid>
```

Verification checks canonical record contents, current manifest hashes when
available, referenced artifact hashes, and required provenance. It does not
prove that a malicious host executed exactly what was recorded and is neither
hardware-backed nor a cryptographic proof of execution. V0.10 produces
`Observed` for explicit host runs and `IsolatedObserved` for a container
provider.

Input values are represented by hashes by default, so credentials and private
arguments are not copied into the attestation. Replay reports this limitation
when an explicit input artifact is unavailable.
