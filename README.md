# Hardknock

**Evidence-backed experience for autonomous agents.** Hardknock runs bounded trials, records what happened, and uses tested, scoped lessons to inform later work. The agent still decides what to do; Hardknock preserves the evidence across runs and agents.

<p align="center">
  <img src="hardknock-murph.png" alt="Murph, the Hardknock axolotl, holding a wrench and an experiment checklist" width="200">
</p>

An agent can fail a task, explain the failure convincingly, and still learn the wrong rule. Hardknock treats that explanation as a hypothesis. It compares alternatives from recorded starting conditions, evaluates the results with explicit checks, and retains both supporting and contradictory evidence. Lessons have scope and can be revised or retired.

```text
Task → disposable Reality → execution + checks → immutable Experience
                                                   ↓
                                  hypothesis → controlled trial
                                                   ↓
                                    scoped Lesson → later decision
```

## Install a binary release

Each release publishes a standalone `install-hardknock` bootstrap that does
not require Rust. GitHub CLI 2.97.0 or newer is required for the official
download. Verify the bootstrap before executing it:

```bash
set -euo pipefail

VERSION='<version>'
TAG="v$VERSION"
REPOSITORY='openkedge/hardknock'

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]

GH_VERSION="$(gh --version | sed -n '1s/^gh version \([^ ]*\).*/\1/p')"
[[ "$GH_VERSION" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)([-+].*)?$ ]]
GH_MAJOR="${BASH_REMATCH[1]}"
GH_MINOR="${BASH_REMATCH[2]}"
if (( GH_MAJOR < 2 ||
      (GH_MAJOR == 2 && GH_MINOR < 97) )); then
  echo "GitHub CLI 2.97.0 or newer is required." >&2
  exit 1
fi

gh release download "$TAG" \
  --repo "$REPOSITORY" \
  --pattern 'install-hardknock' \
  --pattern 'install-hardknock.sha256'
gh release verify-asset "$TAG" install-hardknock --repo "$REPOSITORY"
gh release verify-asset "$TAG" install-hardknock.sha256 --repo "$REPOSITORY"

TAG_OBJECT="$(
  gh api "repos/$REPOSITORY/git/ref/tags/$TAG" \
    --jq 'select(.object.type == "tag") | .object.sha'
)"
STABLE_TAG_COMMIT="$(
  gh api "repos/$REPOSITORY/git/tags/$TAG_OBJECT" \
    --jq "select(
      .tag == \"$TAG\" and
      .verification.verified == true and
      .verification.reason == \"valid\" and
      .object.type == \"commit\"
    ) | .object.sha"
)"
DEFAULT_BRANCH="$(gh api "repos/$REPOSITORY" --jq '.default_branch')"
SOURCE_REF="refs/heads/$DEFAULT_BRANCH"
SIGNER_WORKFLOW='openkedge/hardknock/.github/workflows/release.yml'
PREDICATE_TYPE='https://openkedge.dev/hardknock/release-publication/v1'
SLSA_PREDICATE_TYPE='https://slsa.dev/provenance/v1'
ATTESTATION_LIMIT=100

[[ "$TAG_OBJECT" =~ ^[0-9a-f]{40}$ ]]
[[ "$STABLE_TAG_COMMIT" =~ ^[0-9a-f]{40}$ ]]
[[ "$DEFAULT_BRANCH" =~ ^[A-Za-z0-9._/-]+$ ]]

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

verify_publication_attestations() {
  local asset="$1"
  local digest="$2"
  local query row slsa_count strict_row

  [[ "$asset" =~ ^[A-Za-z0-9._-]+$ ]]
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]]
  query="$(
    cat <<EOF
. as \$results
| select((\$results | type) == "array")
| select(
    (\$results | length) > 0 and
    (\$results | length) < $ATTESTATION_LIMIT
  )
| [\$results[]
    | .verificationResult as \$verification
    | \$verification.statement.predicate as \$predicate
    | select(
        (\$predicate | type) == "object" and
        \$predicate.channel == "stable" and
        \$predicate.release_tag == "$TAG"
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
    .predicate.artifact_source_commit == "$STABLE_TAG_COMMIT" and
    .predicate.asset_digests["$asset"] == "$digest" and
    .predicate.workflow_source_ref == "$SOURCE_REF" and
    (.predicate.workflow_source_commit | test("^[0-9a-f]{40}$")) and
    .source_ref == .predicate.workflow_source_ref and
    .source_digest == .predicate.workflow_source_commit and
    .signer_digest == .predicate.workflow_source_commit
  )
| [.source_ref, .source_digest]
| @tsv
EOF
  )"

  # Discover one exact predicate/source tuple; identical retries collapse.
  row="$(
    gh attestation verify "$asset" \
      --repo "$REPOSITORY" \
      --limit "$ATTESTATION_LIMIT" \
      --signer-workflow "$SIGNER_WORKFLOW" \
      --deny-self-hosted-runners \
      --predicate-type "$PREDICATE_TYPE" \
      --format json \
      --jq "$query"
  )"
  [[ "$row" == "$SOURCE_REF"$'\t'* ]]
  VERIFIED_SOURCE_REF="${row%%$'\t'*}"
  VERIFIED_SOURCE_COMMIT="${row#*$'\t'}"
  [[ "$VERIFIED_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]

  # Bind the custom predicate and standard provenance to that exact run.
  strict_row="$(
    gh attestation verify "$asset" \
      --repo "$REPOSITORY" \
      --limit "$ATTESTATION_LIMIT" \
      --signer-workflow "$SIGNER_WORKFLOW" \
      --deny-self-hosted-runners \
      --source-ref "$VERIFIED_SOURCE_REF" \
      --source-digest "$VERIFIED_SOURCE_COMMIT" \
      --signer-digest "$VERIFIED_SOURCE_COMMIT" \
      --predicate-type "$PREDICATE_TYPE" \
      --format json \
      --jq "$query"
  )"
  [[ "$strict_row" == "$row" ]]
  slsa_count="$(
    gh attestation verify "$asset" \
      --repo "$REPOSITORY" \
      --limit "$ATTESTATION_LIMIT" \
      --signer-workflow "$SIGNER_WORKFLOW" \
      --deny-self-hosted-runners \
      --source-ref "$VERIFIED_SOURCE_REF" \
      --source-digest "$VERIFIED_SOURCE_COMMIT" \
      --signer-digest "$VERIFIED_SOURCE_COMMIT" \
      --predicate-type "$SLSA_PREDICATE_TYPE" \
      --format json \
      --jq "select(
        (type == \"array\") and
        (length > 0) and
        (length < $ATTESTATION_LIMIT)
      ) | length"
  )"
  [[ "$slsa_count" =~ ^[1-9][0-9]*$ ]]
}

BOOTSTRAP_SHA256="$(sha256_file install-hardknock)"
CHECKSUM_SHA256="$(sha256_file install-hardknock.sha256)"
verify_publication_attestations install-hardknock "$BOOTSTRAP_SHA256"
BOOTSTRAP_PROMOTION_COMMIT="$VERIFIED_SOURCE_COMMIT"
verify_publication_attestations install-hardknock.sha256 "$CHECKSUM_SHA256"
[[ "$VERIFIED_SOURCE_COMMIT" == "$BOOTSTRAP_PROMOTION_COMMIT" ]]

CURRENT_DEFAULT_BRANCH="$(gh api "repos/$REPOSITORY" --jq '.default_branch')"
[[ "$CURRENT_DEFAULT_BRANCH" == "$DEFAULT_BRANCH" ]]
ENCODED_DEFAULT_BRANCH="$(
  python3 -c \
    'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' \
    "$DEFAULT_BRANCH"
)"
DEFAULT_BRANCH_HEAD="$(
  gh api "repos/$REPOSITORY/branches/$ENCODED_DEFAULT_BRANCH" \
    --jq '.commit.sha'
)"
[[ "$DEFAULT_BRANCH_HEAD" =~ ^[0-9a-f]{40}$ ]]
BRANCH_BINDING="$(
  gh api \
    "repos/$REPOSITORY/compare/$BOOTSTRAP_PROMOTION_COMMIT...$DEFAULT_BRANCH_HEAD" \
    --jq '[.status, .merge_base_commit.sha, .head_commit.sha] | join("|")'
)"
case "$BRANCH_BINDING" in
  "ahead|$BOOTSTRAP_PROMOTION_COMMIT|$DEFAULT_BRANCH_HEAD" | \
    "identical|$BOOTSTRAP_PROMOTION_COMMIT|$DEFAULT_BRANCH_HEAD") ;;
  *)
    echo "Attested promotion commit is not in current default-branch history." >&2
    exit 1
    ;;
esac

EXPECTED="$(awk 'NR == 1 { print $1 }' install-hardknock.sha256)"
[[ "$BOOTSTRAP_SHA256" == "$EXPECTED" ]]

chmod 0755 install-hardknock
```

Install the verified release for a workstation:

```bash
./install-hardknock --version "$VERSION" --dry-run --json
./install-hardknock --version "$VERSION"
"$HOME/.local/bin/hardknock" setup \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --start \
  --json
"$HOME/.local/bin/hardknock" --json doctor --strict
```

The installer accepts the official HTTPS release location or an absolute local
mirror, verifies the selected archive checksum and contents, and installs only
`hardknock` and `hk-effect`. Official downloads also require GitHub CLI to bind
the archive to the requested release tag and verify an attestation from
Hardknock's release workflow. Stable releases additionally require a checked
promotion record covering immutable-release configuration and all production
evidence gates. Attestation lookups request the GitHub CLI 2.97.0 maximum of
100 results and fail closed at saturation before collapsing byte-identical
predicate and signed source-binding tuples.

An agentic installer can keep every step noninteractive and machine-readable:

```bash
VERSION='<version>'
./install-hardknock \
  --version "$VERSION" \
  --prefix "$HOME/.local" \
  --no-modify-path \
  --json

"$HOME/.local/bin/hardknock" setup \
  --agent auto \
  --mode ci \
  --non-interactive \
  --start \
  --json
```

Use `--dry-run --json` on either installer or setup to inspect every planned
change. `hardknock upgrade` creates a verified recovery point and refreshes
managed configuration after the binary is replaced. `hardknock repair`
revalidates owned files. `hardknock uninstall` removes managed adapters and
service files while preserving the data home; `--remove-data` is a separate
explicit action. Offline mirrors report checksum-only provenance, and
`--no-verify-provenance` is the explicit escape hatch for a custom HTTPS
source. See [production operations](docs/operations.md).

## Connect an MCP client

Milestone 4 includes a generic MCP stdio integration for agent hosts that can
launch a local subprocess. Inspect its machine-readable installation contract,
then configure the host to run:

```bash
hardknock integration manifest
hardknock mcp serve --stdio --workspace /absolute/path/to/project
```

The server advertises MCP protocol `2026-07-28` as a preview interface and
routes three bounded tools through the authenticated local Bridge:
`hardknock_query_context`, `hardknock_record_outcome`, and
`hardknock_experiment_status`. The first
context query can create a scoped session and returns a
`hardknock_session_id`; every later stateful call must supply that explicit
handle. The tool surface cannot grant approvals, commit external effects, or
provide command or filesystem execution. Generic experiment creation remains
disabled until Hardknock can enforce an isolated provider for that surface.

The generic MCP integration has local conformance coverage, but external live
acceptance across agent hosts is still pending. See [agent
integrations](docs/integrations.md) and the [compatibility
matrix](docs/compatibility-matrix.md).
Release operators use the [production validation
contract](docs/production-validation.md) and [release-candidate
runbook](docs/release-candidate-runbook.md).

## Try it locally

Hardknock is a pre-release Rust CLI. Build it on Linux or macOS with Rust 1.88
or newer, Git, and a C compiler. This deterministic example needs no model,
package manager, or network service after dependencies are available:

```bash
cargo build --locked
HARDKNOCK_BIN="$PWD/target/debug/hardknock"
DEMO_ROOT="$(mktemp -d)"
cp -R fixtures/strategy-choice "$DEMO_ROOT/project"
git -C "$DEMO_ROOT/project" init -b main
git -C "$DEMO_ROOT/project" config user.name 'Hardknock Demo'
git -C "$DEMO_ROOT/project" config user.email 'demo@example.invalid'
git -C "$DEMO_ROOT/project" add .
git -C "$DEMO_ROOT/project" -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -m 'Strategy fixture'

"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/project" try \
  --agent test-agent \
  --candidate 'direct=direct-upgrade' \
  --candidate 'staged=staged-upgrade' \
  --check './test.sh'
```

The two candidates start from the same committed fixture. The direct upgrade fails its check; the staged upgrade passes. Hardknock reports the comparison and stores two Experiences, but does not apply the winning candidate to the source repository. See the [experiment walkthrough](docs/agent-experiments.md) for the result format, limitations, and a nonfixture example.

For your own clean, committed repository, start with `hardknock --repo /path/to/project run --help` or the [CLI reference](docs/cli.md). Provide a `--check` command when you need a task outcome: a process exit code alone does not establish task success. `HARDKNOCK_HOME` or `--home` selects a dedicated data directory outside the source repository.

## What it covers

Hardknock is one modular Rust crate with SQLite metadata, local artifacts, Git worktree Realities, and an authenticated local Bridge. Its implemented local flows include:

| Area | What it does | Start here |
| --- | --- | --- |
| Experience and experiments | Capture evaluated runs, compare alternatives, retrieve scoped Lessons, and test transfer | [Experience model](docs/experience-model.md) · [Experiments](docs/experiments.md) |
| Resilience and learning | Run controlled chaos and recovery trials; plan bounded curricula and experience budgets | [Chaos](docs/chaos.md) · [Curriculum](docs/curriculum.md) · [Economics](docs/experience-economics.md) |
| Runtime decisions | Use evidence for advice, abstention, prevention, and inspectable decisions | [Runtime control](docs/runtime-control.md) · [Predictive experience](docs/predictive-experience.md) |
| Execution and effects | Declare capabilities, run portable tools, stage supported external effects, and require explicit commit | [Execution boundary](docs/execution-boundary.md) · [Effects](docs/effects.md) |
| Shared knowledge | Integrate agents, federate signed evidence, resolve scoped knowledge, and coordinate bounded teams | [Integrations](docs/integrations.md) · [Federation](docs/federation.md) · [Knowledge resolution](docs/knowledge-resolution.md) · [Team governance](docs/team-governance.md) |

The [documentation index](docs/README.md) groups the full guides, design details, benchmarks, and implementation reports. The [architecture](docs/architecture.md) explains component boundaries; the [roadmap](docs/roadmap.md) describes the longer direction.

## Current status and limits

Hardknock is pre-release. Repository-side productionization is complete for
the scoped single-user Linux/macOS and dedicated-CI boundary, including the
version-pinned binary installer, transactional setup, portable MCP contract,
release workflow, and machine-readable promotion evidence. The tree is ready
to enter controlled release-candidate validation. General-production
promotion remains gated on published artifacts, hosted target and native
service runs, live-agent acceptance, rootless-container validation, the
required 24-hour soaks, and verified repository release controls. The
[V0.22 progress record](docs/v0.22-progress.md) lists the checkpoint boundary.

The [production-readiness and installation plan](docs/production-readiness-plan.md) gives the current overall verdict and the gated path to a supported 1.0 release for general local and CI agent use.
Implementation status and validation evidence are tracked in the [production progress record](docs/production-progress.md).
The [production operations guide](docs/operations.md) documents strict health
checks, backup and restore, retention, Bridge services, and soak validation.

Git worktrees are disposable working states, **not security sandboxes**. They share access to host files, credentials, network, and Git state. Use trusted commands and disposable tasks; choose a documented container or tool capability boundary where isolation matters. Supported external Effects require a separate explicit commit, and Hardknock does not intercept arbitrary external calls. Read the [execution boundary](docs/execution-boundary.md) and [effect security guide](docs/effect-security.md) before using those features.

Native Claude Code, Codex, Hermes, and OpenClaw adapters have deterministic
fixture coverage. The generic MCP stdio integration is also implemented as a
preview. Live cross-agent acceptance is still incomplete. Benchmarks in the
docs are designed local fixtures, not estimates of production reliability or
general agent improvement.

## Repository map

| Path | Purpose |
| --- | --- |
| `src/` | CLI, runtime, evidence, learning, integrations, and storage modules |
| `tests/` | Integration and security tests |
| `fixtures/` | Deterministic, offline scenarios used by demos and tests |
| `integrations/` | Native agent adapter assets |
| `migrations/` | Append-only SQLite schema migrations |
| `docs/` | Guides, design notes, benchmarks, and milestone reports; start at [docs/README.md](docs/README.md) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build checks and contribution guidance. Hardknock is licensed under [Apache-2.0](LICENSE); see [NOTICE](NOTICE).
