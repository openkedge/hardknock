# Production Implementation Progress

_Started: September 25, 2026_

This record tracks implementation of the
[production-readiness plan](production-readiness-plan.md). Each milestone ends
with its own reviewed commit and validation summary.

## Working rules

- Keep the supported 1.0 boundary limited to single-user workstation and CI
  operation on Linux and macOS.
- Preserve Hardknock's evidence, authority, and fail-closed trust semantics.
- Commit each completed milestone separately.
- Keep coding-agent prompts, transcripts, and scratch logs outside the
  repository.
- Preserve unrelated user workspace files and changes.
- Record exact validation results and unresolved limitations before marking a
  milestone complete.

## Milestones

| Milestone | Status | Commit | Exit evidence |
| --- | --- | --- | --- |
| 0. Verdict and implementation plan | Complete | `17a79d2` | Plan reviewed; documentation links added |
| 1. Build, package, version, schema, and lifecycle truth | Complete locally | `1998244` | Local release gates pass; hosted Linux/macOS workflows await their first remote run |
| 2. Storage and Bridge operations | Complete locally | `1f6c077` | Local implementation and sandbox-compatible gates pass; 24-hour and host-facility runs remain release evidence |
| 3. Binary installer and transactional setup | Pending | — | Fresh host setup, dry-run JSON, repair, upgrade, and non-destructive uninstall |
| 4. Portable agent integration | Pending | — | MCP stdio, integration manifest, conformance harness, and live adapter matrix |
| 5. Production validation | Pending | — | Cross-platform security, parser, load, soak, and recovery evidence |
| 6. Beta and 1.0 release | Pending | — | Published compatibility policy and all 1.0 release gates complete |

## Milestone 1 checklist

- [x] Align the package version with the V0.22 development line.
- [x] Make the documented formatting and Clippy gates pass on the selected
      toolchain.
- [x] Restore Linux/macOS CI and add a tag-driven release workflow.
- [x] Package only `hardknock` and `hk-effect` for end users.
- [x] Keep `hardknock-test-adapter` available to explicit test builds.
- [x] Replace hard-coded migration diagnostics with one latest-schema source.
- [x] Make `hardknock doctor` report the actually applied schema.
- [x] Stabilize ordinary descendant cleanup on cancellation and test it
      repeatedly.
- [x] Align current-status documentation with package and implementation state.
- [x] Pass formatting, Clippy, targeted tests, full serial tests, release build,
      and installation smoke checks.

## Milestone 1 completion evidence

Implementation commit: `1998244`.

- Rust 1.88.0 passed the locked, offline, all-target, all-feature compiler
  check. Rust 1.98.1 passed formatting, strict Clippy, and documentation with
  warnings denied.
- The full serial all-target/all-feature suite passed 464 tests with zero
  failures; five explicit manual or live-agent tests remained ignored.
- The cancellation race regression passed 32 fork-burst iterations, and the
  CLI and experiment tests verified that child and grandchild processes stop.
- A clean package verification passed in a fresh target directory. A normal
  `cargo install --path .` installed exactly `hardknock` and `hk-effect`;
  `hardknock-test-adapter` remained available only through the explicit
  `test-adapter` feature.
- Two clean `aarch64-apple-darwin` release builds in separate target
  directories produced byte-identical stripped `hardknock` and `hk-effect`
  binaries. The release workflow repeats this comparison on all four supported
  target triples and creates deterministic archives.
- Release metadata generation produced a deterministic CycloneDX 1.7 SBOM and
  license inventory for the 200-package resolved graph. Its focused tests and
  the deterministic archive tests passed.
- Cargo-deny 0.20.2 passed advisory, license, and source policy checks. Eight
  duplicate-version groups remain visible as warnings.
- Actionlint 1.7.12 accepted both workflows. Release actions are pinned to
  immutable commits; tagged assets receive checksums and GitHub build
  provenance attestations.
- Fresh-home, reopen, newer-schema rejection, and doctor tests all reported
  schema 30 through `LATEST_SCHEMA_VERSION`.

The hosted Ubuntu/macOS matrix and tag publication flow cannot be executed
locally. Milestone 1 is complete in the repository, but those first hosted runs
remain release evidence rather than assumed results.

## Milestone 2 checklist

- [x] Add verified database and artifact backup plus guarded restore.
- [x] Add migration inspection and verified pre-upgrade recovery snapshots.
- [x] Add artifact inventory, quotas, protected-evidence rules, and dry-run
      pruning.
- [x] Reconcile abandoned automatic Realities and stale Bridge runtime files.
- [x] Persist interrupted Bridge and experiment state truthfully after restart.
- [x] Retain bounded structured Bridge diagnostics and validate clean shutdown.
- [x] Add validated `systemd --user` and `launchd` service templates.
- [x] Extend doctor with strict schema, filesystem, disk, runtime, backup,
      release-integrity, Bridge, and adapter checks.
- [x] Pass 100 consecutive process-tree cancellation iterations locally.
- [x] Provide a repeatable 24-hour Bridge soak harness.
- [ ] Record the first 24-hour Linux and macOS runs as release evidence.
- [x] Pass formatting, strict Clippy, compiler checks, focused recovery tests,
      and the complete sandbox-compatible suite before the milestone commit.
- [ ] Repeat the complete serial suite with host Unix sockets and process
      inspection enabled.

## Milestone 2 completion evidence

Implementation commit: `1f6c077`.

- Backup uses SQLite's online backup API, copies and hashes referenced
  artifacts, verifies integrity and foreign keys, and publishes only a
  complete new bundle. Restore verifies into a private staging directory and
  refuses existing, relocated, linked, or insecure targets.
- Migration dry-run is non-mutating. A real schema upgrade takes the
  maintenance and artifact gates, waits for active producers, starts the
  SQLite write transaction, creates a verified recovery bundle, and applies
  migrations atomically.
- Storage policy inventories all artifacts while protecting retained evidence.
  Only bounded regular files under `artifacts/transient/` can be pruned.
  Private reservation ledgers permit concurrent producers while enforcing
  aggregate byte and file limits.
- Backup creation and verification use deterministic iterative traversal,
  enforce entry and nesting limits, and count empty directories as well as
  files. Subprocess stdout and stderr are each bounded to 8 MiB, container
  output uses the manifest limit per stream, and Git diff capture is bounded
  to 16 MiB. Capacity reservations cover retries, both diffs, direct command
  paths, and metadata. The cancellation stress regression completed 100
  process-tree iterations.
- Startup reconciliation closes abandoned automatic Realities, marks
  interrupted experiments and Bridge work truthfully, and cleans only runtime
  paths whose ownership, type, mode, link count, identity, and inactive lease
  have been verified. Container intent is persisted before runtime creation,
  and reconciliation reloads state after acquiring its lease so it cannot
  discard a Reality completed between scanning and cleanup.
- Detached Bridge diagnostics retain one active 1 MiB JSONL file and four
  bounded archives. Static `systemd --user` and `launchd` templates use
  owner-only modes, bounded restart behavior, process-group shutdown, and
  platform logging.
- Strict doctor now checks the shared schema version, filesystem safety, disk
  and artifact capacity, stale resources, release integrity, Bridge and
  adapter health, active artifact reservations, and a verified backup with a
  staged restore.
- Locked offline compiler checks, formatting, and strict all-target/all-feature
  Clippy passed. Storage integration tests passed 17/17; four container-proxy
  tests, two reservation tests, both reconciliation race regressions,
  storage-policy, doctor, diagnostics, and process tests passed. One broader
  reconciliation fixture still requires creating a Unix socket.
- An independent follow-up review reran seven focused regressions and confirmed
  that all four previously reported high-severity findings were resolved:
  container completion races, pre-marker container crashes, incomplete
  artifact reservations, and unbounded backup traversal.
- The soak harness passed 23 deterministic tests and the service templates
  passed seven. Corrected historical migration fixtures passed individually
  while preserving immutable evidence JSON and artifact references.
- A complete serial no-fail-fast run exercised every target. After the three
  corrected migration fixtures were rerun successfully, the remaining
  unavailable cases were confined to eight targets whose fixtures require
  Unix-domain socket creation or `ps`; this managed sandbox denies those host
  facilities before product code runs. Two attempts to run the suite with host
  access were stopped because the automatic permission review timed out.

The first 24-hour Linux and macOS soak results, hosted service-manager checks,
and a complete host-facility serial run remain release evidence. They are not
claimed by this local milestone.

## Baseline evidence

Before Milestone 1 changes:

- `cargo fmt --all --check` passed.
- `cargo build --release --locked --offline` passed.
- A full serial retry passed 459 tests with zero failures and five ignored
  manual or optional tests.
- A preceding full serial run failed once because a background descendant
  survived Ctrl-C cleanup; isolated and full-suite retries passed.
- The documented Clippy command failed on an unknown suppressed lint.
- `hardknock doctor` reported schema 20 after applying schema 30.
- `cargo install --path .` installed the internal
  `hardknock-test-adapter` alongside the product binaries.
- Native integration fixtures passed, while live host acceptance remained
  incomplete and the installed Codex version was outside the pinned fixture
  version.
