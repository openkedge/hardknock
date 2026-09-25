# Changelog

This file records user-visible Hardknock changes. Historical milestone reports
under `docs/` remain the detailed evidence record for pre-release development.

## Unreleased

### Added

- Production-readiness plan, implementation milestones, and release gates.
- Pinned Rust 1.88 minimum support and Rust 1.98.1 release builds.
- Linux and macOS CI with repeated serial tests, package/install checks, and
  dependency policy enforcement.
- Four-platform tag builds with byte-reproducibility checks, deterministic
  archives, checksums, CycloneDX SBOMs, license inventories, and build
  provenance attestations.
- A feature-gated conformance test adapter that is excluded from normal
  end-user installation.

### Changed

- V0.23+ feature development is frozen while the existing V0.22 checkpoint is
  productized for a scoped single-user and CI 1.0 boundary.
- Package and runtime version metadata now identify the V0.22 development
  line.
- Database migration and doctor output share one latest-schema source.
- Process cancellation performs bounded post-reap process-group cleanup to
  close the ordinary descendant fork race.

## 0.22.0-checkpoint.1 - 2026-09-23

### Added

- Signed, cursor-based filesystem synchronization between configured nodes.
- Conservative remote knowledge admission, relay-origin verification,
  revocation propagation, and deterministic distributed fixtures.
- Bounded multi-agent governance, plan validity, and composition foundations
  completed through the V0.21 milestone.

See [V0.22 progress](docs/v0.22-progress.md) and the historical implementation
reports for exact behavior, evidence, and limitations.
