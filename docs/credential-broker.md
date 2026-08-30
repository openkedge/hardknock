# Credential broker

The V0.9 broker models credentials as short-lived, scoped grants rather than ambient environment. A `CredentialCapability` names a provider, credential, resource, allowed permissions, and optional expiry. Issuance requires an exact matching capability and creates an `IssuedCredential` audit record containing identifiers, scope, lifecycle times, and an opaque local reference. It never stores raw secret bytes or a guessable-secret digest in SQLite.

The local `StaticTestCredentialBroker` is an integration fixture, not a production cloud broker. It keeps the issued secret in a private host mode-0600 runtime file. Immediately before one proxied action it materializes a mode-0444 file in a unique per-action directory under the Reality's read-only control mount, injects an environment variable whose value is only that file path, registers the secret bytes with the output redactor, and deletes the materialized file when the action ends. The durable host copy is removed on revoke, expiry-at-next-use, freeze, or disposal. The enclosing Hardknock home is host mode `0700`.

```text
host credential source -> scoped issuance -> one Reality
                                      |-> exact permissions/expiry
                                      |-> output redactor
                                      `-> revoke and delete
```

The container environment is constructed from manifest values plus path-only `HARDKNOCK_CREDENTIAL_<PROVIDER>_<NAME>` references for active grants. Host AWS, Kubernetes, Git, SSH, proxy, and shell variables are not inherited. A credential is visible to the agent during the operation for which it was issued; it is not therefore claimed to be secret from that agent. The safety properties are limited scope, short lifetime, no ambient inheritance, no raw database storage, redacted captured output, and revocation.

Exact byte sequences of known issued secrets are replaced with `[REDACTED]` before stdout, stderr, event payloads, or Experience artifacts are persisted. This cannot redact unknown encodings, hashes, character-by-character exfiltration, or a secret transformed before capture. The safest integration remains a credentialless agent calling a host-side structured Effect adapter that owns its own scoped connection.

Security tests verify that issued raw bytes are absent from SQLite and serialized records, are redacted from captured output, and cause credential-issued/revoked audit events. Because no live container runtime was available, an actual container `env` command with an injected secret was not observed in this pass.
