# Production validation

This document defines the production validation contract for Hardknock's
single-user glibc Linux and macOS production boundary. It separates checks that
can be reproduced from a repository checkout from evidence that must be
collected on released binaries and representative hosts.

Passing the local repository gates means the source tree is a release-candidate
input. It does not establish that artifacts were published, native service
managers worked on real hosts, live agents conformed, every declared target ran
in hosted CI, or the 24-hour soak passed.

## Evidence classes

| Class | Meaning | May be satisfied locally? |
| --- | --- | --- |
| Repository gate | Deterministic source, fixture, parser, packaging, and recovery checks | Yes |
| Host gate | Behavior that depends on an operating system, architecture, container runtime, or native service manager | Only on the named host |
| Live-agent gate | Disposable task through an installed agent and its real lifecycle API | No fixture substitution |
| Published-artifact gate | Installation and provenance verification against immutable release assets | No local mirror substitution |
| Soak gate | Time-bounded observation of a release binary on a supported host | No shortened-run substitution |

Every retained result must identify its class. A result marked `partial`,
`unavailable`, skipped, ignored, or run against a debug binary does not satisfy
an external evidence gate.

## Trusted publication controls

Release publication requires GitHub CLI 2.97.0 or newer and a
`workflow_dispatch` run selected from the exact protected default branch.
`release_tag` must be a strict signed annotated `vX.Y.Z-rc.N` or `vX.Y.Z`
tag. `promotion_commit` must be the current default-branch commit from which
GitHub loads the workflow. The workflow checks out the tag source explicitly;
tag pushes never execute publication code.

Both `hardknock-candidate-publication` and
`hardknock-stable-publication` must be protected GitHub environments. They
must each require at least one direct GitHub user account, reject team
reviewers, use disjoint reviewer user-ID and normalized-login sets, prevent
self-review, and use selected deployment branches containing only the exact
default branch. Reviewer records use `type: User`, a positive immutable
numeric GitHub user ID, and the account's canonical lowercase login.
`RELEASE_CONTROL_TOKEN` is an environment secret in each environment, so tag
or feature-branch workflow code cannot read it. The workflow makes only read
API calls. The token must have enough ruleset permission for GitHub to return
`bypass_actors`; publication fails closed when that field is hidden.
Each environment also holds a separate least-privilege
`RELEASE_PUBLISH_TOKEN`. The workflow's built-in token has only
`contents: read`; creating or inspecting the final release with write access
therefore requires the reviewed environment.

The default branch must require reviews, block force pushes and deletion,
enforce protection for administrators, and have no review bypass actors.
Active no-bypass tag rulesets must cover both strict release tag forms and
block update and deletion. Repository immutable releases must be enabled.
These controls are external to the repository and are mandatory even when all
local tests pass.

## Reproducible repository gates

Run from a clean checkout of the candidate commit. Do not use an uncommitted
working tree as release evidence.

```bash
git status --short
git rev-parse HEAD
rustc +1.98.1 --version --verbose
cargo +1.98.1 --version

cargo +1.98.1 fmt --all --check
cargo +1.98.1 check --locked --all-targets --all-features
cargo +1.98.1 clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' \
  cargo +1.98.1 doc --locked --no-deps --all-features

cargo +1.98.1 test --locked --all-targets --all-features \
  -- --test-threads=1
cargo +1.98.1 test --locked --all-targets --all-features \
  -- --test-threads=1
```

Verify the minimum supported compiler separately:

```bash
rustc +1.88.0 --version --verbose
cargo +1.88.0 check --locked --all-targets --all-features
```

Run the deterministic productization checks:

```bash
HARDKNOCK_INSTALL_TEST_SHELL=/bin/sh scripts/test_install.sh
HARDKNOCK_INSTALL_TEST_SHELL=/bin/dash scripts/test_install.sh
python3 scripts/test_release_metadata.py
python3 scripts/test_package_release.py
python3 scripts/test_release_evidence.py
python3 scripts/test_bridge_soak.py
python3 scripts/test_service_templates.py
sh scripts/test_integration_conformance.sh

cargo +1.98.1 test --locked --offline --test setup -- --nocapture
cargo +1.98.1 test --locked --offline --test storage -- --nocapture
cargo +1.98.1 test --locked --offline --test bridge_capacity -- --nocapture
cargo +1.98.1 test --locked --offline --test integrations -- --nocapture
cargo +1.98.1 test --locked --offline --test security -- --nocapture
```

The release decision itself is machine-readable. Copy
`release/evidence-template.json` to
`release/evidence/<stable-version>/record.json`. Put only bounded redacted
receipts below that version directory, and bind every reference by SHA-256.
Record the operator and independent reviewer with the same typed direct-user
shape. Their positive IDs and normalized logins must differ. During stable
publication, the workflow resolves both logins through the GitHub API, checks
that the returned direct-user IDs equal the committed IDs, and retains those
verified mappings in the run metadata.
Verify the record before promotion:

```bash
VERSION='<stable-version>'
TAG="v$VERSION"
RC_NUMBER='<positive-integer>'
SOURCE_TAG="v$VERSION-rc.$RC_NUMBER"
SOURCE_COMMIT="$(git rev-list -n 1 "$SOURCE_TAG")"
SOURCE_TREE="$(git show -s --format=%T "$SOURCE_COMMIT")"
SOURCE_DATE_EPOCH="$(git show -s --format=%ct "$SOURCE_COMMIT")"

python3 scripts/verify_release_evidence.py \
  "release/evidence/$VERSION/record.json" \
  --evidence-root "release/evidence/$VERSION" \
  --expected-version "$VERSION" \
  --expected-tag "$TAG" \
  --expected-source-repository openkedge/hardknock \
  --expected-source-tag "$SOURCE_TAG" \
  --expected-source-commit "$SOURCE_COMMIT" \
  --expected-source-tree "$SOURCE_TREE" \
  --expected-source-date-epoch "$SOURCE_DATE_EPOCH"
```

The verifier exits nonzero for pending, partial, unavailable, or failed gates;
empty passing evidence; unresolved blocking defects; mismatched tag/version;
placeholder or mismatched candidate source identities; a decision other than
`promote`; artifact names or digests that do not match the final stable
version; all-zero placeholder object IDs or digests; or an evidence file that
is missing, empty, oversized, linked, outside the evidence directory, changing
during verification, or different from its recorded digest. Every passing
reference must contain a typed receipt whose
gate, kind, candidate identity, checks, and gate-specific details validate.
The repository receipt types the default-branch review, force-push, deletion,
administrator, and bypass controls. The release-controls receipt types the
effective candidate and stable tag rulesets and the two protected publication
environments, nonempty direct-user reviewer objects, disjoint reviewer user-ID
and normalized-login sets, self-review prevention, and exact-default-branch
deployment policy. Team reviewers are rejected. The typed fields are
`candidate_environment_required_reviewers`,
`stable_environment_required_reviewers`, and
`reviewer_user_ids_disjoint`; the verifier independently checks the ID sets.
The repository receipt must also contain exactly `linux` and `macos` serial
test results. Each result records `passes: 2` or more and the exact
`https://github.com/openkedge/hardknock/actions/runs/<run-id>` URL for the
candidate workflow that ran both sequential full-suite passes on that
operating-system family. A scalar pass count, a missing family, an extra
family, or an unrelated workflow URL fails closed.

Evidence freshness is anchored to the exact candidate commit timestamp, not
the machine running the verifier. `candidate.frozen_at_utc` must be within
zero to 72 hours after `candidate.source.source_date_epoch`. Every typed
receipt's `observed_at_utc` must be at or after freeze and at or before
`candidate.evidence_completed_at_utc`. Completion must be within 14 days of
freeze. This ordered interval rejects stale evidence and timestamps beyond the
declared evidence campaign while keeping repeated verification reproducible.
The dependency-advisory receipt has a stricter freshness rule:
`database_updated_at_utc` records a successful database refresh at or after
candidate freeze, no later than receipt observation, and no more than 24 hours
before that observation.

Package and inspect the install boundary:

```bash
cargo +1.98.1 package --locked

INSTALL_ROOT="$(mktemp -d)"
cargo +1.98.1 install --locked --path . --root "$INSTALL_ROOT"
test -x "$INSTALL_ROOT/bin/hardknock"
test -x "$INSTALL_ROOT/bin/hk-effect"
test ! -e "$INSTALL_ROOT/bin/hardknock-test-adapter"
test "$(find "$INSTALL_ROOT/bin" -type f | wc -l | tr -d ' ')" -eq 2
"$INSTALL_ROOT/bin/hardknock" --version
"$INSTALL_ROOT/bin/hk-effect" --version
```

The two sequential full serial test passes on Linux and the two on macOS,
package inspection, and focused commands must all use the same commit and lock
file. Record every ignored or environment-skipped test; such tests remain
external evidence work.

## Enforced limits

The candidate must retain these fail-closed limits:

| Surface | Limit |
| --- | --- |
| Installer archive | 256 MiB |
| Installer checksum file | 4 KiB |
| Each installed binary | 128 MiB |
| Each packaged legal file | 8 MiB |
| Provenance command | 120 seconds and 1 MiB per captured stream |
| Candidate version command | 30 seconds and 4 KiB per captured stream |
| Bridge configuration | 1 MiB |
| Integration and setup JSON files | 1 MiB each |
| Bridge event | 1 MiB |
| Stored Bridge output summary | 8 KiB |
| Bridge context | 1–32 KiB; default 32 KiB |
| Context lessons | 1–5; default 5 |
| Active Bridge sessions | 1–1,024; default 256 |
| MCP in-flight requests | 32 |
| Tool subprocess output | capability limit; default 8 MiB per stream |
| Tool capture shutdown | 2 seconds after stop |
| Native service-manager command | 10 seconds and 64 KiB per stream |
| Setup rollback file | 4 MiB |
| Setup rollback set | 64 files and 32 MiB total prior content |
| Setup fingerprint input | 64 MiB per file |
| Setup recovery state | 1 MiB |
| Setup transaction lock | 5 seconds |
| Backup manifest | 16 MiB |
| Backup inventory | 100,000 source entries, 64 path components |
| Maintenance lock | 2 seconds |
| Bridge diagnostics | active 1 MiB plus four 1 MiB archives |

Default artifact policy is 2 GiB, 20,000 files, and 1 GiB minimum free space.
Generated Git diffs are limited to 16 MiB. Limits complement operating-system
and container controls; they are not a filesystem or kernel quota.

Validate configured limits and current use:

```bash
hardknock --json storage status
hardknock --json storage check-capacity --bytes 1048576 --files 16
hardknock --json doctor --strict
```

## Recovery drills

Use a dedicated `HARDKNOCK_HOME` and disposable agent configuration for every
drill. Preserve the command output, transaction journal, doctor report, and
before/after file digests.

### Interrupted or failed managed setup

Normal failures roll back managed files before returning. A process crash may
leave a private sibling recovery directory. Other setup mutations then fail
closed. Recover with:

```bash
hardknock repair \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --json
hardknock --json doctor --strict
```

Repair first restores transaction-owned files whose recorded identity still
matches, preserves concurrent changes, archives the recovery journal, and then
applies the requested managed state. Do not delete a
`.hardknock-setup-*.recovery` directory before repair.

### Failed upgrade or corrupted home

Inspect migration state, preserve the failed home, and restore the verified
pre-upgrade bundle to the original home path:

```bash
hardknock --json migration dry-run
mv "$HARDKNOCK_HOME" "$HARDKNOCK_HOME.failed"
hardknock --home "$HARDKNOCK_HOME" --json \
  restore --verify \
  "$HARDKNOCK_HOME.failed/backups/pre-migration-<from>-to-<to>-<time>"
hardknock --home "$HARDKNOCK_HOME" --json doctor --strict
```

Restore requires a missing or empty target and rejects relocation, tampering,
unsafe paths, insecure modes, symlinks, hard links, and an occupied target.
There are no down migrations; downgrade recovery uses the backup made before
the upgrade.

### Full disk or quota exhaustion

Inspect capacity, plan pruning, and apply only the bounded transient plan:

```bash
hardknock --json storage status
hardknock --json storage prune
hardknock --json storage prune --apply
hardknock --json doctor --strict
```

Protected evidence is never an automatic prune candidate. If the plan cannot
recover enough space, move or expand the filesystem; do not remove arbitrary
artifact files.

### Killed Bridge or stale runtime state

```bash
hardknock bridge status
hardknock bridge stop
hardknock bridge start
hardknock --json doctor --strict
```

Startup owns the runtime lock before removing stale endpoint, token, socket, or
relay paths. It reconciles interrupted Bridge-owned records and unlocked
automatic Realities. External effects and descendants that deliberately leave
their process group require operator inspection.

### Failed managed uninstall

Retry the non-destructive operation after inspecting the journal:

```bash
hardknock uninstall --dry-run --json
hardknock uninstall --non-interactive --json
hardknock --json doctor --strict
```

The command removes exact managed files and retains data by default. If a later
step fails, managed files are restored and a previously running Bridge is
restarted. Use `--remove-data` only after a separate verified backup.

### Interrupted bootstrap installer

The installer records a private transaction phase and rollback metadata before
changing the prefix or login profile. After an uncatchable termination, the
next installer invocation must take the prefix lock, reject an active owner,
and either complete a verified rollback or fail closed while preserving the
transaction directory. Validate this with SIGKILL during fresh install,
upgrade, profile mutation, and uninstall. Retain the before/after manifest,
installer result, and recovery output.

## External evidence gates

The following evidence cannot be replaced by repository fixtures:

1. Published archives, checksums, SBOM, license inventory, and provenance
   attestations for all four declared targets from an immutable
   `vX.Y.Z-rc.N` candidate release.
2. Verified repository immutable-release configuration, protected default
   branch and tag rulesets, and distinct reviewed candidate and stable
   publication environments. The trusted default-branch workflow must read a
   passing promotion record without executing workflow code from a tag.
3. Fresh installation from those immutable assets without Rust installed.
4. Managed `systemd --user` operation only with platform `linux`, and managed
   launchd operation only with platform `macos`, including reboot/login
   restart, bounded shutdown, logs, upgrade, and uninstall. An optional
   architecture must be exactly `x86_64` or `aarch64`.
5. Hosted jobs using the exact mappings:
   `x86_64-unknown-linux-gnu` on `ubuntu-24.04`,
   `aarch64-unknown-linux-gnu` on `ubuntu-24.04-arm`,
   `x86_64-apple-darwin` on `macos-15-intel`, and
   `aarch64-apple-darwin` on `macos-15`. Receipts must record matching
   `linux`/`macos` and `x86_64`/`aarch64` values. Self-hosted runners,
   Windows, `s390x`, and mixed combinations fail the gate.
6. Live disposable acceptance for generic MCP, current Claude Code, and current
   Codex. Hermes and OpenClaw must remain preview unless their live gates pass.
7. Rootless Docker and rootless Podman security checks on representative
   supported hosts.
8. One uninterrupted 24-hour Bridge soak on Linux and one on macOS using the
   release binary.

Run the host soak with a dedicated home:

```bash
python3 scripts/bridge_soak.py /absolute/path/to/release/hardknock \
  --home /absolute/path/to/dedicated/soak-home \
  --duration-seconds 86400 \
  --poll-interval-seconds 30 \
  --command-timeout-seconds 30 \
  --shutdown-timeout-seconds 15
```

A five-second run is a harness smoke test only:

```bash
python3 scripts/bridge_soak.py target/release/hardknock \
  --home /tmp/hardknock-bridge-soak \
  --duration-seconds 5 \
  --poll-interval-seconds 0.1
```

Every soak probe must be `checked` and passing for the capability being
claimed. `partial` and `unavailable` results must be carried into the release
decision.

## Evidence record

The record binds promotion to the exact immutable candidate source tag, commit,
tree, commit timestamp, and complete packaged asset manifest. The exact
`vX.Y.Z-rc.N` tag identifies the immutable prerelease while `candidate.tag`
records the intended stable `vX.Y.Z` tag. The package, binaries, and asset names
already use `X.Y.Z`.
Each gate receipt identifies that same candidate, its evidence class, exact
checks, host or agent versions where applicable, recovery result, producer, and
UTC timestamp. The promotion record references each receipt with a canonical
relative path, a 64-character lowercase SHA-256 digest, and its required kind:

```json
{
  "status": "pass",
  "evidence": [
    {
      "path": "receipts/hosted-linux-x86_64.json",
      "sha256": "<digest>",
      "kind": "hosted_matrix"
    }
  ]
}
```

Keep secrets, raw prompts, credentials, unrestricted logs, and complete user
homes outside the repository. Commit only the bounded redacted receipts needed
to make the stable decision independently verifiable. A generic text or JSON
file is not a receipt: the verifier requires the exact typed fields and
gate-specific pass conditions.

After the evidence record is committed, the operator creates `vX.Y.Z` at the
exact candidate commit as a signed annotated tag, then dispatches
`.github/workflows/release.yml` from the exact protected default-branch
promotion commit. The stable environment supplies publication privileges only
after independent review. The workflow verifies both tag signatures and
dereferenced commit/tree objects through the GitHub API, the effective branch,
environment, and tag-ruleset controls, every candidate digest, and every
candidate attestation.

Stable publication re-attests the unchanged bytes from the trusted
default-branch promotion commit. The stable tag instead resolves to the exact
candidate/artifact commit. Consumers must not use the tag ref or tag commit as
the stable attestation source.

The dependency-free consumer contract uses GitHub CLI's built-in JSON query
support. For each downloaded asset, calculate its SHA-256, resolve the signed
annotated stable tag to its commit, and query the repository default branch.
Run custom-predicate verification first with the exact signer workflow and
`https://openkedge.dev/hardknock/release-publication/v1` predicate type, using
`--limit 100 --format json --jq` to require one unique predicate where
`channel` is
`stable`, `release_tag` is the requested stable tag,
`artifact_source_commit` is the stable-tag commit, the asset's entry in
`asset_digests` equals its local SHA-256, `workflow_source_ref` is
`refs/heads/<default-branch>`, and `workflow_source_commit` is lowercase
40-hex. The same result's signed certificate must set
`sourceRepositoryRef` to that ref and both `sourceRepositoryDigest` and
`buildSignerDigest` to that commit. Extract that workflow ref and commit, then
rerun both the custom predicate and standard SLSA provenance verification
with those exact `--source-ref` and `--source-digest` values and with
`--signer-digest` equal to the workflow source commit. All passes require the
exact release workflow and deny self-hosted runners; the standard pass
specifies `--predicate-type https://slsa.dev/provenance/v1`. The first pass
only discovers the source pair; installation trust comes from the exact
second-pass verifications and field comparison.

Before counting first-pass results, collapse byte-identical duplicate
predicates together with the signed certificate's `sourceRepositoryRef`,
`sourceRepositoryDigest`, and `buildSignerDigest`. One unique exact
predicate/source binding is accepted, including after an attestation-only
retry. Zero unique matches or any conflicting predicate or signed source
binding fails closed. No retry deletes an attestation or release. Check the
raw result count before deduplication; 100 results is a saturated lookup and
fails closed because GitHub CLI may have truncated a conflicting attestation.
Every custom and SLSA verification path uses the same explicit limit.

The custom predicate also binds the candidate tag and commit/tree,
promotion-record digest, and complete asset digest map. The workflow verifies
these attestations locally before creating the stable release, then re-resolves
both tags, downloads the stable assets, and proves byte identity with the
candidate manifest.

Reruns never edit an immutable release. An existing release is accepted only
when its tag binding, channel flags, immutability, complete asset set, digests,
bytes, and attestations match exactly. Any mismatch or transient
post-publication verification failure leaves the release unchanged for
investigation. No artifact is rebuilt, and the immutable candidate prerelease
is never edited into stable.
