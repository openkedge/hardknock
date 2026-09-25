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
| 2. Storage and Bridge operations | In progress | — | Backup/restore, upgrade safety, retention, reconciliation, durable daemon logs and service lifecycle |
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
