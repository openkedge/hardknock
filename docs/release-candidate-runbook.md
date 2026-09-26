# Release candidate runbook

This runbook defines the Milestone 6 procedure for producing and evaluating a
Hardknock release candidate. A candidate becomes eligible for 1.0 only after
both the repository gates and the external evidence gates pass.

Repository validation proves that a commit can be packaged. It does not prove
that assets were published, native service managers worked, live agents
conformed, hosted operating-system and architecture jobs passed, or the
required soak completed.

## Roles and evidence directories

Assign a release operator and an independent reviewer by immutable GitHub user
ID and canonical lowercase login. The IDs must be positive and different; the
normalized logins must also be different. Resolve each account through the
GitHub API and retain both values in the evidence record. Keep raw release
evidence outside the source checkout:

```bash
export HK_RELEASE_VERSION='<stable-version>'
export HK_RC_NUMBER='<positive-integer>'
export HK_CANDIDATE_TAG="v$HK_RELEASE_VERSION-rc.$HK_RC_NUMBER"
export HK_STABLE_TAG="v$HK_RELEASE_VERSION"
export HK_RC_EVIDENCE="$HOME/hardknock-release-evidence/$HK_CANDIDATE_TAG"
mkdir -p "$HK_RC_EVIDENCE"
chmod 700 "$HK_RC_EVIDENCE"
```

Do not store agent prompts, model transcripts, credentials, unrestricted logs,
or release-operation scratch files in the repository.

The stable promotion commit contains only bounded, redacted evidence receipts
under `release/evidence/<stable-version>/`. Every receipt referenced by
`record.json` is bound by its SHA-256 digest. Keep the larger source artifacts
in the external directory.

The release tooling requires Python 3.11 or newer and GitHub CLI 2.97.0 or
newer. Verify the CLI before every release operation:

```bash
gh --version
```

Configure these external controls before creating a release tag:

- protect the default branch with required pull-request reviews, force-push
  and deletion blocking, administrator enforcement, and no review-bypass
  actors;
- create `hardknock-candidate-publication` and
  `hardknock-stable-publication` as distinct protected environments;
- configure at least one direct GitHub user account as a required reviewer for
  each environment, reject team reviewers, keep the two environments'
  reviewer user-ID and normalized-login sets disjoint, enable self-review
  prevention, and use a selected-branch deployment policy that permits only
  the exact default branch;
- store `RELEASE_CONTROL_TOKEN` separately in each environment, rather than as
  a repository secret. The workflow uses it only for read API calls. It needs
  environment and branch-protection read access plus sufficient repository
  ruleset access for the API to return `bypass_actors`; GitHub currently
  requires write access to the ruleset for that field. If the field is hidden,
  publication fails closed;
- store a separate `RELEASE_PUBLISH_TOKEN` in each environment with only the
  repository contents permission needed to create and inspect releases. The
  workflow's built-in token keeps `contents: read`, so release creation depends
  on entering the reviewed environment;
- activate no-bypass tag rulesets that match the candidate and stable tag
  forms and block both tag update and tag deletion; and
- enable immutable releases for the repository.

These are mandatory release controls. The workflow checks their effective API
state before either publication job can create a release. Environment approval
does not replace evidence review, and the release operator must not be an
environment reviewer.

## 1. Freeze and identify a prerelease candidate

Start from a clean checkout:

```bash
git status --short
git fetch --tags origin
git rev-parse HEAD
git show -s --format='%H%n%ct%n%T' HEAD
cargo +1.98.1 metadata --locked --no-deps --format-version 1
```

Set the package to its final stable version, such as `1.0.0`, before external
validation. Tag that exact source with a strict candidate tag such as
`v1.0.0-rc.1`. The candidate workflow publishes a distinct immutable
prerelease. Its package version, binary versions, archive names, and checksums
still use `1.0.0`, allowing the exact files to be published later under the
stable `v1.0.0` tag without rebuilding or renaming them.
`Cargo.lock`, migration files, integration schemas, protocol versions,
compatibility policy, changelog, licenses, and support policy are frozen for
the candidate. Any code or contract change creates a new candidate and resets
affected evidence.

Record:

```yaml
stable_version:
candidate_tag:
commit:
tree:
source_date_epoch:
operator:
  type: User
  id: <positive GitHub user ID>
  login: <canonical lowercase login>
independent_reviewer:
  type: User
  id: <different positive GitHub user ID>
  login: <different canonical lowercase login>
frozen_at_utc:
evidence_completed_at_utc:
```

The evidence timeline is reproducible and does not use the verifier's wall
clock. `frozen_at_utc` must be no earlier than the exact
`source_date_epoch` and no more than 72 hours after it. Every receipt's
`observed_at_utc` must be between `frozen_at_utc` and
`evidence_completed_at_utc`, inclusive. Completion must be no earlier than
freeze and no more than 14 days later. A year-2000 receipt for a 2026 source,
or any receipt after the declared completion time, fails closed.

## 2. Pass repository validation

Run all commands in [production validation](production-validation.md).
Complete two sequential full serial test passes on Linux and two on macOS
against the same candidate commit and lock file. At minimum, retain output
from each operating-system family:

```bash
cargo +1.98.1 fmt --all --check
cargo +1.98.1 check --locked --all-targets --all-features
cargo +1.98.1 clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' \
  cargo +1.98.1 doc --locked --no-deps --all-features
cargo +1.98.1 test --locked --all-targets --all-features \
  -- --test-threads=1
cargo +1.98.1 test --locked --all-targets --all-features \
  -- --test-threads=1
cargo +1.98.1 package --locked

sh scripts/test_install.sh
python3 scripts/test_release_metadata.py
python3 scripts/test_package_release.py
python3 scripts/test_release_evidence.py
python3 scripts/test_bridge_soak.py
python3 scripts/test_service_templates.py
sh scripts/test_integration_conformance.sh
```

Also verify Rust 1.88:

```bash
cargo +1.88.0 check --locked --all-targets --all-features
```

Stop the release if a required check fails, flakes, is skipped unexpectedly, or
uses a modified checkout. Fixes require a new commit and candidate.

## 3. Tag and build immutable prerelease artifacts

Create and push the candidate tag only after repository review. Candidate
publication is dispatched from the protected default branch; pushing a tag
does not start a privileged workflow:

```bash
export HK_DEFAULT_BRANCH="$(
  gh repo view openkedge/hardknock \
    --json defaultBranchRef \
    --jq '.defaultBranchRef.name'
)"
export HK_CANDIDATE_COMMIT="$(git rev-parse HEAD)"
export HK_DEFAULT_BRANCH_PATH="$(
  python3 -c \
    'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' \
    "$HK_DEFAULT_BRANCH"
)"
export HK_PROMOTION_COMMIT="$(
  gh api \
    "repos/openkedge/hardknock/branches/$HK_DEFAULT_BRANCH_PATH" \
    --jq '.commit.sha'
)"
test "$HK_CANDIDATE_COMMIT" = "$HK_PROMOTION_COMMIT"

git tag -s "$HK_CANDIDATE_TAG" "$HK_CANDIDATE_COMMIT" \
  -m "Hardknock $HK_CANDIDATE_TAG"
git verify-tag "$HK_CANDIDATE_TAG"
git push origin "$HK_CANDIDATE_TAG"

gh workflow run .github/workflows/release.yml \
  --repo openkedge/hardknock \
  --ref "$HK_DEFAULT_BRANCH" \
  -f "release_tag=$HK_CANDIDATE_TAG" \
  -f "promotion_commit=$HK_PROMOTION_COMMIT"
```

The release workflow accepts candidate tags only in the form
`vX.Y.Z-rc.N`, where `N` is a positive integer without leading zeroes, and
requires the Cargo package version to remain `X.Y.Z`. An exact `vX.Y.Z` tag is
reserved for stable publication after evidence review. The dispatch fails if
the selected workflow ref is not the current default branch, if that branch
does not equal `promotion_commit`, or if the candidate tag does not point to
that same commit. The workflow verifies the annotated tag signature and
dereferenced commit and tree through the GitHub API before any build and again
immediately before publication.

The candidate workflow must:

- verify that tag and package version match;
- run formatting, strict Clippy, package inspection, and two sequential full
  serial-suite passes on both Linux and macOS;
- build `hardknock` and `hk-effect` twice with Rust 1.98.1;
- compare the two binaries byte for byte;
- package only those binaries plus `LICENSE` and `NOTICE`;
- publish SHA-256 files, CycloneDX SBOM, license inventory, and provenance
  attestations;
- run installer transactions under Ubuntu `/bin/sh`, Ubuntu `/bin/dash`, and
  macOS `/bin/sh`;
- enter only the protected candidate publication environment before obtaining
  write or attestation permissions;
- publish an immutable prerelease without replacing or editing an existing
  release; and
- run `gh release verify` and exact asset and attestation checks after
  publication. A failed or transient post-publication check leaves the release
  unchanged and held for investigation.

On a rerun, an existing candidate is accepted only when its signed tag
binding, prerelease and immutable state, complete asset set, byte digests, and
both provenance and release-binding attestations all match. Every other
existing-release state fails closed.

Required targets:

| Target | Exact hosted runner | OS | Architecture |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `linux` | `x86_64` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `aarch64` |
| `x86_64-apple-darwin` | `macos-15-intel` | `macos` | `x86_64` |
| `aarch64-apple-darwin` | `macos-15` | `macos` | `aarch64` |

Hosted matrix receipts must contain exactly the row associated with their
target. Self-hosted runners, Windows, `s390x`, aliases, and mixed
runner/OS/architecture combinations do not satisfy this gate.

Download each published archive, checksum, metadata file, and attestation into
the external evidence directory. Record workflow run identifiers and immutable
asset URLs. A local archive or workflow artifact does not satisfy the
published-artifact gate.

The repository receipt records the serial runs separately from the four build
targets:

```json
{
  "serial_test_passes": {
    "linux": {
      "passes": 2,
      "workflow_run_url": "https://github.com/openkedge/hardknock/actions/runs/123"
    },
    "macos": {
      "passes": 2,
      "workflow_run_url": "https://github.com/openkedge/hardknock/actions/runs/123"
    }
  }
}
```

Use the exact candidate workflow URL. Both families may name the same run ID
because the workflow executes them as separate jobs in one protected dispatch.

The release must also publish the attested `install-hardknock` bootstrap and
its checksum copied from `scripts/install.sh`.

## 4. Verify published candidate assets

On a fresh host for each target, with no Rust toolchain required, download the
candidate release and verify its immutable release record, exact asset digests,
and trusted-dispatch attestations. Candidate publication requires the default
branch commit to equal the candidate source commit, so both the signed tag and
the workflow identity bind the same commit:

```bash
set -euo pipefail

mkdir -p "$HK_RC_EVIDENCE/assets"
cd "$HK_RC_EVIDENCE/assets"
gh release download "$HK_CANDIDATE_TAG" \
  --repo openkedge/hardknock
gh release verify "$HK_CANDIDATE_TAG" --repo openkedge/hardknock

export HK_SOURCE_COMMIT="$(git rev-list -n 1 "$HK_CANDIDATE_TAG")"
export HK_SOURCE_TREE="$(git show -s --format=%T "$HK_SOURCE_COMMIT")"
export HK_ATTESTATION_LIMIT=100

[[ "$HK_DEFAULT_BRANCH" =~ ^[A-Za-z0-9._/-]+$ ]]
[[ "$HK_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]
[[ "$HK_SOURCE_TREE" =~ ^[0-9a-f]{40}$ ]]

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

for asset_path in ./*; do
  test -f "$asset_path"
  asset="${asset_path#./}"
  [[ "$asset" =~ ^[A-Za-z0-9._-]+$ ]]
  test "${#asset}" -le 255
  digest="$(sha256_file "$asset")"
  candidate_query="$(
    cat <<EOF
. as \$results
| select((\$results | type) == "array")
| select(
    (\$results | length) > 0 and
    (\$results | length) < $HK_ATTESTATION_LIMIT
  )
| [\$results[]
    | .verificationResult as \$verification
    | \$verification.statement.predicate as \$predicate
    | select(
        (\$predicate | type) == "object" and
        \$predicate.channel == "candidate" and
        \$predicate.release_tag == "$HK_CANDIDATE_TAG"
      )
    | {
        predicate: \$predicate,
        source_ref: \$verification.signature.certificate.sourceRepositoryRef,
        source_digest:
          \$verification.signature.certificate.sourceRepositoryDigest,
        signer_digest:
          \$verification.signature.certificate.buildSignerDigest
      }]
| unique
| select(length == 1)
| .[0]
| select(
    .predicate.schema == "hardknock-release-publication-v1" and
    .predicate.stable_tag == "$HK_STABLE_TAG" and
    .predicate.artifact_source_commit == "$HK_SOURCE_COMMIT" and
    .predicate.artifact_source_tree == "$HK_SOURCE_TREE" and
    .predicate.asset_digests["$asset"] == "$digest" and
    .predicate.workflow_source_ref == "refs/heads/$HK_DEFAULT_BRANCH" and
    .predicate.workflow_source_commit == "$HK_SOURCE_COMMIT" and
    .source_ref == .predicate.workflow_source_ref and
    .source_digest == .predicate.workflow_source_commit and
    .signer_digest == .predicate.workflow_source_commit
  )
| [.source_ref, .source_digest]
| @tsv
EOF
  )"
  gh release verify-asset "$HK_CANDIDATE_TAG" "$asset" \
    --repo openkedge/hardknock
  slsa_count="$(
    gh attestation verify "$asset" \
      --repo openkedge/hardknock \
      --limit "$HK_ATTESTATION_LIMIT" \
      --signer-workflow openkedge/hardknock/.github/workflows/release.yml \
      --deny-self-hosted-runners \
      --source-ref "refs/heads/$HK_DEFAULT_BRANCH" \
      --source-digest "$HK_SOURCE_COMMIT" \
      --signer-digest "$HK_SOURCE_COMMIT" \
      --predicate-type https://slsa.dev/provenance/v1 \
      --format json \
      --jq 'select(
        (type == "array") and
        (length > 0) and
        (length < 100)
      ) | length'
  )"
  test -n "$slsa_count"
  custom_binding="$(
    gh attestation verify "$asset" \
      --repo openkedge/hardknock \
      --limit "$HK_ATTESTATION_LIMIT" \
      --signer-workflow openkedge/hardknock/.github/workflows/release.yml \
      --deny-self-hosted-runners \
      --predicate-type \
        https://openkedge.dev/hardknock/release-publication/v1 \
      --format json \
      --jq "$candidate_query"
  )"
  expected_binding="refs/heads/$HK_DEFAULT_BRANCH"$'\t'"$HK_SOURCE_COMMIT"
  test "$custom_binding" = "$expected_binding"
  strict_custom_binding="$(
    gh attestation verify "$asset" \
      --repo openkedge/hardknock \
      --limit "$HK_ATTESTATION_LIMIT" \
      --signer-workflow openkedge/hardknock/.github/workflows/release.yml \
      --deny-self-hosted-runners \
      --source-ref "refs/heads/$HK_DEFAULT_BRANCH" \
      --source-digest "$HK_SOURCE_COMMIT" \
      --signer-digest "$HK_SOURCE_COMMIT" \
      --predicate-type \
        https://openkedge.dev/hardknock/release-publication/v1 \
      --format json \
      --jq "$candidate_query"
  )"
  test "$strict_custom_binding" = "$custom_binding"
done

verify_checksum_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$1"
  else
    shasum -a 256 -c "$1"
  fi
}
for checksum in ./*.sha256; do
  verify_checksum_file "$checksum"
done
```

The custom release-publication predicate must name
`$HK_CANDIDATE_TAG`, `$HK_STABLE_TAG`, `$HK_SOURCE_COMMIT`, and
`$HK_SOURCE_TREE`, identify `refs/heads/$HK_DEFAULT_BRANCH` as the trusted
workflow source, and contain the digest of every exact asset. The workflow
performs that field-level comparison; retain its publication run metadata with
the external evidence. Every lookup uses the bounded `--limit 100`. A raw
result set with 100 entries is treated as possibly
truncated and fails closed. Byte-identical custom retry predicates and their
signed certificate source bindings collapse to one; any second predicate or
signed source binding for the same channel and release tag is a conflict.

Native service receipts use only these mappings:

| Gate | Manager | Platform | Optional architecture |
| --- | --- | --- | --- |
| `systemd_user` | `systemd-user` | `linux` | `x86_64` or `aarch64` |
| `launchd` | `launchd` | `macos` | `x86_64` or `aarch64` |

If an architecture is recorded, it must use one of the exact supported values.

Extract the archive for the host and verify both binaries report
`$HK_RELEASE_VERSION`. Test bootstrap transaction behavior against a local
mirror containing these exact verified candidate assets. The bootstrap maps
package version `X.Y.Z` to release tag `vX.Y.Z`, so its official HTTPS path is
not available until the separate stable release exists. Candidate provenance
must therefore be established by the GitHub checks above; the local mirror
test establishes installer behavior only.

Record archive name, SHA-256, provenance result, installed file manifest,
binary version output, operating system, architecture, and whether Rust was
absent.

## 5. Exercise managed setup and native services

Use a fresh dedicated home and a disposable user account or host:

```bash
export HARDKNOCK_HOME="$HOME/.hardknock"

hardknock setup \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --dry-run \
  --json

hardknock setup \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --start \
  --json

hardknock --json doctor --strict
hardknock integration manifest
hardknock --json integrate doctor
hardknock bridge status
```

Repeat the setup command and verify that it creates no duplicate hook, plugin,
service, or configuration entry.

On Linux, retain:

```bash
systemctl --user status hardknock-bridge.service
journalctl --user -u hardknock-bridge.service
```

On macOS, retain:

```bash
launchctl print "gui/$(id -u)/dev.openkedge.hardknock.bridge"
log show --last 30m --predicate 'process == "hardknock-bridge"'
```

For both service managers, test login or reboot activation, stop, start,
bounded shutdown, log destination, failed-start diagnostics, upgrade, repair,
and uninstall. A template parser test does not satisfy this host gate.

CI runners use:

```bash
hardknock setup \
  --agent auto \
  --mode ci \
  --non-interactive \
  --start \
  --json
hardknock --json doctor --strict
```

CI mode validates the on-demand Bridge path and does not count as native
service-manager evidence.

## 6. Run live agent acceptance

Use a disposable committed repository with deterministic checks and no
production secrets. Retain agent and model versions, integration diagnostics,
the evaluated outcome, and bounded Hardknock evidence.

Required before 1.0:

1. A generic MCP host launches
   `hardknock mcp serve --stdio --workspace /absolute/project`, retrieves
   context, records an outcome, and reads a separately authorized experiment's
   status.
2. Current Claude Code completes an evaluated lifecycle through installed
   hooks.
3. Current Codex completes an evaluated App Server lifecycle and safely handles
   approval requests.
4. Claude Code and Codex complete a second-agent transfer case using scoped
   evidence.

The generic MCP surface must still expose exactly:

- `hardknock_query_context`;
- `hardknock_record_outcome`;
- `hardknock_experiment_status`.

It must not grant approval, commit an external Effect, execute commands, expose
arbitrary filesystem access, or create generic experiments.

Hermes and OpenClaw remain preview unless equivalent live gates pass. Fixture
tests and a schema-compatible warning do not establish live support.

## 7. Run security and recovery drills

Run rootless Docker and rootless Podman separately:

```bash
HARDKNOCK_TEST_CONTAINER=1 \
  cargo +1.98.1 test --locked --test security \
  optional_live_container_denies_host_secret_socket_ambient_credentials_and_network \
  -- --nocapture
```

Record runtime name/version, rootless status, host OS/architecture, image
digest, capability manifest, and cleanup result.

Using public commands, rehearse:

- interrupted setup followed by `hardknock repair --non-interactive --json`;
- failed upgrade followed by verified backup restore;
- corrupted database rejection and restore;
- full-disk or quota refusal and bounded transient pruning;
- killed Bridge, stale runtime files, clean restart, and record reconciliation;
- failed managed uninstall and rollback;
- key or signing compromise response, including candidate withdrawal and
  replacement credentials.

Create a backup before destructive drills:

```bash
hardknock --json backup "$HK_RC_EVIDENCE/pre-drill.hkbak"
hardknock --json migration dry-run
```

Verify rollback or restore:

```bash
mv "$HARDKNOCK_HOME" "$HARDKNOCK_HOME.failed"
hardknock --home "$HARDKNOCK_HOME" --json \
  restore --verify "$HK_RC_EVIDENCE/pre-drill.hkbak"
hardknock --home "$HARDKNOCK_HOME" --json doctor --strict
```

The restore target must be the original path and missing or empty. Preserve the
failed home until the reviewer accepts the recovery evidence.

After an uncatchable installer termination, the next invocation takes the
prefix lock and validates the recorded transaction phase and file identities.
It completes a verified rollback before starting a new mutation. If recovery
cannot be proven safe, it fails closed and preserves the transaction directory
for inspection. Validate fresh install, upgrade, profile mutation, and
uninstall interruption under both `/bin/sh` and `/bin/dash`.

## 8. Complete the 24-hour soak

Run the published release binary on one supported Linux host and one supported
macOS host:

```bash
python3 scripts/bridge_soak.py /absolute/path/to/published/hardknock \
  --home /absolute/path/to/dedicated/soak-home \
  --duration-seconds 86400 \
  --poll-interval-seconds 30 \
  --command-timeout-seconds 30 \
  --shutdown-timeout-seconds 15
```

Retain the single JSON result and system resource observations. The gate
requires:

- no leaked daemon or descendant process;
- no stale endpoint, token, socket, or relay;
- no leaked managed Reality, Git worktree, transient evaluator artifact, or
  label-filtered container resource;
- queryable persisted session and run state after restart;
- no new nonterminal record;
- diagnostics within the 5 MiB aggregate bound;
- clean stop, second start, and second stop;
- every claimed host capability reported as `checked`, not `partial` or
  `unavailable`.

A shortened run verifies only the harness.

## 9. Upgrade, rollback, and uninstall

Install the previous supported candidate, create representative evidence, and
upgrade in place:

```bash
hardknock upgrade \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --start \
  --json
hardknock --json doctor --strict
```

Verify that the upgrade creates or confirms a recovery point, migrates once,
preserves configuration and evidence, refreshes exact managed files, and keeps
the native service healthy.

For a forced failed upgrade, restore the pre-migration backup as documented in
[production validation](production-validation.md). Then verify managed
uninstall:

```bash
hardknock uninstall --dry-run --json
hardknock uninstall --non-interactive --json
test -d "$HARDKNOCK_HOME"
```

Finally test explicit data removal only on a disposable installation with a
matching setup manifest and an independently verified backup:

```bash
hardknock uninstall --non-interactive --remove-data --json
```

## 10. Create the stable promotion commit and decision

The reviewer must reject promotion when any required gate is failed, partial,
unavailable, skipped, stale, tied to another commit, or backed only by a local
mirror or fixture.

Promotion also requires no unresolved P0/P1 defect, no critical or high
default-product dependency advisory, public security and support contacts, and
stable compatibility policies. Defaults must continue to prevent agent
self-approval, external-Effect commit through generic integration, automatic
remote-knowledge activation, and autonomous continuous learning.

Choose the stable version, copy `release/evidence-template.json` to
`release/evidence/<stable-version>/record.json`, and add bounded redacted
receipts below the same directory. Set `candidate.tag` to the intended stable
`vX.Y.Z` tag. Record the exact immutable `vX.Y.Z-rc.N` candidate tag, commit,
tree, and commit timestamp in `candidate.source`. Record every exact candidate
release asset name and SHA-256 under `artifacts`. The template's all-zero
object IDs and digests are deliberately invalid placeholders; replace every
one before review.

Each passing gate references a typed receipt:

```json
{
  "path": "receipts/linux-soak.json",
  "sha256": "<64 lowercase hexadecimal digits>",
  "kind": "soak"
}
```

Paths are relative to `release/evidence/<stable-version>/`. The verifier
rejects missing, empty, oversized, linked, escaping, changing, or
digest-mismatched evidence. It also parses every receipt and requires the
receipt's gate, kind, candidate source identity, checks, target, host, agent,
service manager, runtime, recovery result, or soak duration to match the gate.
A generic nonempty file cannot satisfy a gate. It also enforces the 72-hour
source-to-freeze window, the 14-day freeze-to-completion window, and the
inclusive receipt observation interval without reading the current clock.
For dependency advisories, refresh the advisory database after candidate
freeze. Record the successful refresh as `database_updated_at_utc`; it must be
no later than the receipt observation and no more than 24 hours old at that
observation.

The `repository` receipt also records the protected default branch, required
review count, force-push and deletion blocking, administrator enforcement, and
zero bypass actors. The `release_controls` receipt records the effective
candidate and stable tag ruleset IDs and names, update/deletion blocking and
zero bypass actors, plus both exact publication environments, their distinct
nonempty arrays of direct user reviewer objects (`type`, numeric user `id`, and
canonical lowercase `login`), disjoint user-ID and normalized-login sets,
self-review prevention, and exact-default-branch deployment policy. Team
reviewers are invalid. The candidate operator and independent reviewer use the
same typed direct-user shape and must have different IDs and normalized
logins. These receipts preserve the reviewed control state; the publication
jobs query the live API state again. Stable publication also resolves the
candidate operator and independent reviewer logins through the GitHub API,
requires the returned direct-user IDs to equal the committed IDs, and retains
the verified mappings in publication metadata.

The receipt fields are
`candidate_environment_required_reviewers`,
`stable_environment_required_reviewers`, and
`reviewer_user_ids_disjoint: true`. Every reviewer object must use
`type: "User"`; the verifier computes the ID-set intersection instead of
trusting the Boolean alone.

Resolve the immutable candidate source and verify the completed record:

```bash
export HK_SOURCE_COMMIT="$(git rev-list -n 1 "$HK_CANDIDATE_TAG")"
export HK_SOURCE_TREE="$(git show -s --format=%T "$HK_SOURCE_COMMIT")"
export HK_SOURCE_EPOCH="$(git show -s --format=%ct "$HK_SOURCE_COMMIT")"

gh release verify "$HK_CANDIDATE_TAG" --repo openkedge/hardknock

python3 scripts/verify_release_evidence.py \
  "release/evidence/$HK_RELEASE_VERSION/record.json" \
  --evidence-root "release/evidence/$HK_RELEASE_VERSION" \
  --expected-version "$HK_RELEASE_VERSION" \
  --expected-tag "$HK_STABLE_TAG" \
  --expected-source-repository openkedge/hardknock \
  --expected-source-tag "$HK_CANDIDATE_TAG" \
  --expected-source-commit "$HK_SOURCE_COMMIT" \
  --expected-source-tree "$HK_SOURCE_TREE" \
  --expected-source-date-epoch "$HK_SOURCE_EPOCH"

git add -- "release/evidence/$HK_RELEASE_VERSION"
test -n "$(
  git diff --cached --name-only -- \
    "release/evidence/$HK_RELEASE_VERSION"
)"
while IFS= read -r path; do
  case "$path" in
    "release/evidence/$HK_RELEASE_VERSION/"*) ;;
    *)
      echo "Unexpected staged path: $path" >&2
      exit 1
      ;;
  esac
done < <(git diff --cached --name-only)
git diff --cached --check
git commit -S -m "release: promote $HK_RELEASE_VERSION"
export HK_EVIDENCE_COMMIT="$(git rev-parse HEAD)"
git push origin HEAD
export HK_DEFAULT_BRANCH="$(
  gh repo view openkedge/hardknock \
    --json defaultBranchRef \
    --jq '.defaultBranchRef.name'
)"
# Merge HK_EVIDENCE_COMMIT through the protected branch review process first.
git fetch origin "$HK_DEFAULT_BRANCH"
git merge-base --is-ancestor \
  "$HK_EVIDENCE_COMMIT" "origin/$HK_DEFAULT_BRANCH"
export HK_DEFAULT_BRANCH_PATH="$(
  python3 -c \
    'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' \
    "$HK_DEFAULT_BRANCH"
)"
export HK_PROMOTION_COMMIT="$(
  gh api \
    "repos/openkedge/hardknock/branches/$HK_DEFAULT_BRANCH_PATH" \
    --jq '.commit.sha'
)"

if git ls-remote --exit-code --tags origin \
  "refs/tags/$HK_STABLE_TAG" >/dev/null 2>&1; then
  echo "Stable tag already exists: $HK_STABLE_TAG" >&2
  exit 1
fi
git tag -s "$HK_STABLE_TAG" "$HK_SOURCE_COMMIT" \
  -m "Hardknock $HK_STABLE_TAG"
git verify-tag "$HK_STABLE_TAG"
git push origin "$HK_STABLE_TAG"

gh workflow run .github/workflows/release.yml \
  --repo openkedge/hardknock \
  --ref "$HK_DEFAULT_BRANCH" \
  -f "release_tag=$HK_STABLE_TAG" \
  -f "promotion_commit=$HK_PROMOTION_COMMIT"
```

The verifier must return `ok: true`. Pushing the stable tag does not execute
tag-controlled workflow code. The operator dispatches the workflow from the
exact protected default-branch commit containing the reviewed evidence. The
stable publication environment permits only that branch and requires its own
independent reviewer. The workflow records the exact promotion commit and
allows later reviewed default-branch commits only while that commit remains in
the protected branch history. It fails if either annotated tag signature or
object changes, if `vX.Y.Z` does not point to the candidate record's exact
commit and tree, or if the candidate release no longer satisfies its immutable
contract.

Stable publication is rebuild-free. The workflow verifies the candidate
release is immutable, downloads its complete asset set, rejects missing or
extra assets, checks every recorded digest and checksum, and verifies each
release asset and attestation against the trusted candidate workflow commit
and the custom candidate-tag predicate. It then attests those unchanged bytes
from the trusted promotion commit and verifies the stable-tag predicate
locally before creating a new stable release under `vX.Y.Z`. After creation it
verifies repository release immutability, downloads the stable assets again,
and proves their digests and bytes equal the candidate assets. The immutable
candidate prerelease is never edited.

An existing stable release makes a rerun read-only. It succeeds only when the
release is immutable and non-prerelease, both signed tag bindings are exact,
the complete asset set and digests match the evidence, every byte matches the
candidate, and both attestations verify. Any mismatch fails closed. A failure
after release creation leaves the release unchanged and held for
investigation.

After the stable workflow passes, verify the official installation path on a
fresh host. The stable tag identifies the artifact source commit, while the
trusted default-branch dispatch identifies the attestation source commit.
These commits are intentionally different after the evidence record is
committed.

Resolve and verify the signed annotated stable tag, calculate the local asset
digest, and query the repository's current default branch. First verify the
custom predicate using only the exact signer workflow and predicate type. Use
`gh attestation verify --format json --jq` to accept one unique predicate
whose fields match:

- `schema = hardknock-release-publication-v1`
- `channel = stable`
- `release_tag = $HK_STABLE_TAG`
- `artifact_source_commit =` the dereferenced stable-tag commit
- `asset_digests[$HK_ARCHIVE] =` the local archive SHA-256
- `workflow_source_ref = refs/heads/<default-branch>`
- `workflow_source_commit =` one lowercase 40-hex commit

The same unique result must have certificate
`sourceRepositoryRef = workflow_source_ref`,
`sourceRepositoryDigest = workflow_source_commit`, and
`buildSignerDigest = workflow_source_commit`; these signed fields prevent a
predicate from claiming a different source binding.

That first pass discovers the workflow source pair; it is not sufficient on
its own. Rerun custom-predicate verification with the discovered exact
`--source-ref`, `--source-digest`, and `--signer-digest`, with signer digest
equal to the discovered workflow source commit. Then run standard SLSA
provenance verification with the same exact constraints and explicit
`--predicate-type https://slsa.dev/provenance/v1`. Both calls also require
`--signer-workflow openkedge/hardknock/.github/workflows/release.yml` and
`--deny-self-hosted-runners`. Collapse byte-identical duplicate predicates
together with the certificate's `sourceRepositoryRef`,
`sourceRepositoryDigest`, and `buildSignerDigest` before counting: one unique
exact predicate/source binding is accepted so a retry is idempotent. Reject
zero unique matches, any conflicting predicate or signed source binding, a
changed predicate, or different workflow source pairs between assets. Set
`--limit 100` on every lookup and reject a raw result set whose length reaches
100 before collapsing duplicates; saturation means a conflicting attestation
may be outside the returned set.

The full custom predicate must also bind `$HK_CANDIDATE_TAG`,
`$HK_SOURCE_TREE`, the promotion-record digest, and the complete asset-digest
map. The release workflow performs that full comparison. The installer must at
least enforce the selected archive fields above before executing it. The
copy-paste bootstrap verification in `README.md` implements the same two-pass
contract without requiring a separate `jq` or Python installation.

After those checks, invoke the bootstrap with
`--version "$HK_RELEASE_VERSION"` and no repository override. This confirms
the stable URL and trusted-dispatch publication without treating
`refs/tags/$HK_STABLE_TAG` as the attestation source ref.

If a published candidate is rejected, mark it as withdrawn without replacing
its assets. Fix forward with a new `-rc.N` tag. If the stable tag has already
been pushed, stop and investigate rather than moving it. Operators who
installed the rejected candidate should restore the recorded pre-upgrade
backup or install the last accepted release, then run:

```bash
hardknock repair --agent auto --mode workstation --non-interactive --json
hardknock --json doctor --strict
```
