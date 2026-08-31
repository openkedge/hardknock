# Certification Artifacts (`.hkcert`)

`.hkcert` is a portable JSON artifact independent of SQLite. It contains the
certificate, exact contract revision, exact profile version, Evidence
Manifest, provenance graph, and optional Ed25519 signature.

The signature covers the complete unsigned artifact, thereby binding the Skill
revision, contract revision, profile, Evidence Manifest hash, policy versions,
tool/runtime hashes, and issue time. Manifest verification separately
recomputes the stable evidence hash. Mutation of either layer fails
verification.

```bash
hardknock assurance export deploy \
  --profile basic-behavior-v1 \
  --output deploy.hkcert
hardknock assurance verify deploy.hkcert
```

Export refuses to replace an existing file. Input must be a regular,
non-symlink file no larger than 16 MiB. Verification checks schema, manifest
hash, internal revision/profile/policy consistency, producer/key identity, and
signature.

A valid signature means the named Hardknock node produced this assertion from
the embedded data. It does not establish that the producer was honest, the
contract was adequate, the host was uncompromised, or the evidence applies in
the receiver's context. Therefore verification always reports:

```text
remote certification  authentic (when valid)
local certification   not established
local reproduction    not performed
```

There is no certification transitivity. Importing or verifying an artifact
does not activate a Skill, install a Tool, grant a capability, or execute
anything. Local reproduction creates new local evidence and, if eligible, a
new local certificate; it never edits the source artifact.
