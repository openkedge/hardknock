# Production Implementation Progress

_Started: September 25, 2026_

_Repository implementation completed: September 26, 2026_

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
| 3. Binary installer and transactional setup | Complete locally | `f415a5b` | Hermetic installer and lifecycle fixtures pass; published-release and native-manager runs remain release evidence |
| 4. Portable agent integration | Complete locally | `635a070` | Generic MCP, versioned manifest, bounded conformance fixtures, and compatibility matrix pass locally; external live-host acceptance remains release evidence |
| 5. Production validation | Complete locally | `086dc2e`, `55eadc2` | Atomic Bridge durability, transactional recovery, and sandbox-compatible security, parser, load, and recovery validation pass; hosted and long-running evidence remains external |
| 6. Beta and 1.0 release | Implementation complete; promotion pending | `6b064b7`, `6b9c715` | Trusted release promotion, hardened installer recovery, evidence schema, and deterministic local gates pass; published and supported-host evidence remains external |

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

## Milestone 3 checklist

- [x] Add a version-pinned, Rust-free installer with explicit prefix, PATH,
      dry-run JSON, upgrade, rollback, repair detection, and managed uninstall.
- [x] Reject insecure repositories, unsafe archives, links, traversal,
      oversized payloads, checksum ambiguity, and same-version content drift.
- [x] Add top-level setup, upgrade, repair, and uninstall commands.
- [x] Produce the complete setup plan before mutation and journal every applied
      step without retaining prior user configuration content.
- [x] Roll back managed adapter, service, and manifest files after a failed
      setup.
- [x] Detect Git, Docker, Podman, Claude Code, Codex, Hermes, OpenClaw, and the
      native user service manager.
- [x] Preserve prior agent selection across upgrade and repair.
- [x] Use the stable installed binary path in managed Claude hooks.
- [x] Install exact owner-private systemd user or launchd definitions with an
      on-demand fallback.
- [x] Create managed verified recovery points for setup and upgrade.
- [x] Keep uninstall non-destructive unless a matching manifest authorizes
      explicit data removal.
- [x] Verify GitHub build provenance for official HTTPS downloads and require
      an explicit bypass for custom HTTPS sources.
- [x] Bind official archives to the requested release tag and release workflow,
      with bounded provenance and candidate-binary execution.
- [x] Serialize setup mutations and preserve files changed concurrently during
      rollback.
- [x] Restore a previously running Bridge when a later uninstall step fails,
      and bound service-manager descendants and output capture.
- [x] Make uninstall of an absent managed installation a mutation-free no-op.
- [ ] Run the published installer and native manager lifecycle on each
      supported platform and architecture.

## Milestone 3 completion evidence

Implementation commit: `f415a5b`.

- The POSIX installer passed 33/33 hermetic cases under both `/bin/sh` and
  `/bin/dash`. The suite covers dry-run nonmutation, local and HTTPS mirrors,
  archive and checksum attacks, unsafe prefixes and profiles, lock handling,
  idempotent upgrade, same-version drift, rollback, abandoned transactions,
  PATH ownership, non-destructive uninstall, release-tag mismatch, successful
  and failed workflow-scoped GitHub attestation verification, unavailable and
  timed-out verifiers, timed-out candidate binaries, and explicit provenance
  bypass.
- Managed recovery-point tests expanded the storage suite to 21/21. Recovery
  bundles use the existing maintenance and artifact gates, private unique
  destinations, bounded labels, and full post-publication verification.
- Service planning and application passed 11 focused tests for systemd,
  launchd, on-demand fallback, safe rendering, atomic private writes,
  idempotency, unmanaged conflicts, exact-content removal, bounded manager
  output, timeouts, and descendants that inherit capture streams.
- Setup transaction unit tests passed 20/20 across service, transaction
  locking, compare-before-rollback snapshots, journal, selection, and manifest
  behavior. End-to-end lifecycle fixtures passed 8/8, including a
  post-mutation doctor failure that restored the original Claude settings and
  removed newly managed files, a late failed fresh setup that preserved files
  created concurrently, and a mutation-free absent uninstall.
- An independent follow-up review confirmed that the three setup blockers were
  resolved: concurrent files survive rollback, a previously running Bridge is
  health-checked and restored after failed uninstall, and service-manager
  descendants cannot hold output capture open indefinitely.
- Locked offline all-target/all-feature compiler checks, shell syntax checks,
  formatting, strict Clippy, and the focused suites pass on the combined
  milestone tree.

Published release assets do not yet exist for this development version, so the
real HTTPS release and build-attestation run remains release evidence. Native
systemd and launchd mutation is covered by hermetic manager fixtures locally
and still needs supported-host runs.

## Milestone 4 checklist

- [x] Add a generic MCP stdio command that does not require native agent
      integration.
- [x] Expose only bounded context retrieval, outcome recording, and
      experiment-status tools.
- [x] Keep approvals, external Effect commits, agent execution, generic
      experiment creation, and command/filesystem execution outside the MCP
      surface.
- [x] Bind reused session handles to an active `mcp` session and canonical
      workspace.
- [x] Bound request framing, response framing, concurrency, cancellation, EOF
      drain, and stdio shutdown.
- [x] Add a versioned machine-readable integration manifest and committed JSON
      schema.
- [x] Derive manifest tools and capabilities from the same descriptors as the
      MCP server.
- [x] Add model-free manifest and MCP conformance tests.
- [x] Replace exact Codex-version rejection with generated core-schema and
      initialization checks while retaining tested-version warnings and
      fail-closed approval handling.
- [x] Publish the repository-backed compatibility and authority boundary.
- [ ] Complete live disposable acceptance with current Claude Code and Codex.
- [ ] Complete live acceptance before promoting Hermes or OpenClaw support.

## Milestone 4 completion evidence

Implementation commit: `635a070`.

- `hardknock mcp serve --stdio --workspace PATH` implements MCP protocol
  `2026-07-28` with exactly three tools:
  `hardknock_query_context`, `hardknock_record_outcome`, and
  `hardknock_experiment_status`.
- Modern requests are stateless. Context creation returns an explicit
  `hardknock_session_id`; later stateful calls verify that the handle remains
  active, belongs to the `mcp` adapter, and matches the canonical workspace.
- The stdio server limits requests and responses to Bridge protocol bounds,
  accepts at most 32 concurrent requests, rejects duplicate in-flight IDs,
  aborts and suppresses cancelled requests, drains EOF for at most five
  seconds, and exits cleanly on process interruption without waiting for stdin
  EOF.
- `hardknock integration manifest` emits preview stability, the launch and
  healthcheck commands, declared Linux/macOS `x86_64`/`aarch64` support, the
  exact tool capabilities, and explicit security exclusions. The committed
  JSON schema and runtime validator reject forbidden or unbounded surfaces.
- Codex compatibility now bounds subprocess and pending-event output, verifies
  the generated core App Server schemas used by Hardknock, and performs an
  initialization handshake. The fixture-tested `codex-cli 0.149.1` reports
  `tested`; another conforming version reports
  `core-schema-compatible-untested` with a warning and no approval-schema
  claim. Missing or unsafe fields fail even with `--allow-untested`.
- Formatting, locked offline all-target/all-feature compiler checks, strict
  Clippy, and documentation with warnings denied passed. All ten MCP unit
  tests, all three MCP process tests, all four manifest tests, and the
  standalone conformance script passed. The sandbox-compatible native
  integration selection passed eight tests with two explicit live Codex tests
  ignored and three local-socket fixtures filtered.
- Python fixture compilation, JSON schema parsing, diff checks, and 293 local
  Markdown links passed. An independent review confirmed that session binding,
  authority exclusions, manifest drift prevention, response cancellation, and
  bounded EOF shutdown have no remaining high-priority blocker.

This managed sandbox prohibits Unix-domain socket creation, so the
cross-process Bridge fixture reports a capability-based skip and three native
adapter transport fixtures were not rerun here. Current external agent
versions and model-backed tasks were not invoked. Those supported-host and
live-agent runs remain Milestone 5 release evidence.

## Milestone 5 checklist

- [x] Make action decisions, trajectory updates, forecasts, runtime decisions,
      role views, session revisions, and Bridge events commit atomically.
- [x] Acknowledge lifecycle and action persistence before publishing live
      state, with bounded deadlines and fail-closed in-doubt handling.
- [x] Revalidate session revisions, trajectories, forecasts, runtime
      knowledge, and team authority inside the final write transaction.
- [x] Make session admission, resumption, termination, and run state
      transitions transactional and exactly revision-bound.
- [x] Prevent queue reservations from blocking the persistence writer, and
      propagate sticky writer failures through public barriers.
- [x] Expose a real Bridge shutdown-complete boundary for setup, repair, and
      uninstall.
- [x] Harden setup recovery with descriptor-bound creation, durable identity
      receipts, private quarantine cleanup, and compare-before-rollback.
- [x] Preserve concurrent files and leave an actionable recovery transaction
      whenever safe rollback cannot be proven.
- [x] Pass formatting, strict Clippy, all-target/all-feature compiler checks,
      and focused atomicity, recovery, runtime, predictive, and team tests.
- [ ] Record the hosted Linux/macOS transport and service-manager runs.
- [ ] Record the required 24-hour supported-host soak evidence.

## Milestone 5 completion evidence

Implementation commits: `086dc2e`, `55eadc2`.

- Bridge action persistence now commits the session compare-and-swap,
  predictive trajectory and forecast changes, runtime decision, role knowledge
  view, and emitted events in one SQLite transaction. Commit-time changes to a
  session revision, trajectory, forecast input, or team authority abort every
  side row.
- Session start, resume, end, and queued-run transitions use exact durable
  revision checks. Live session state changes only after the writer
  acknowledges the durable transition; a timeout stops the Bridge and a late
  acknowledgement reconciles live state from the database.
- The bounded writer queue uses nonblocking permits, barriers report prior
  writer failures, and shutdown reports completion only after earlier queued
  writes have finished. Setup and uninstall hold the verified runtime lock
  boundary before mutating integrations or the data home.
- Setup recovery records descriptor-bound file and directory identities before
  mutation. Successful rollback removes transaction-owned recovery state while
  preserving independently created or replaced content; interrupted data
  removal resumes from a private sibling quarantine.
- Independent validation passed formatting and diff checks, a locked offline
  all-target/all-feature compiler check, and strict Clippy with warnings
  denied. Bridge engine tests passed 10/10, Bridge atomicity and capacity tests
  17/17, setup lifecycle tests 9/9, setup transaction tests 32/32, service
  tests 15/15, and integration installer tests 11/11.
- Additional focused validation passed 19 predictive tests, 11 runtime tests,
  20 runtime-knowledge tests with one explicitly ignored manual case, and 17
  team tests. The durable action latency regression measured a debug-build
  P95 of 39.2 ms.
- The follow-up commit preserves the clean-start snapshot used to record the
  first Bridge Experience while independently marking the durable session
  dirty before queued execution. The complete sandbox-compatible Bridge file
  then passed 15/15.

This milestone completes the repository-side, sandbox-compatible production
validation work. Unix-socket and process-inspection fixtures denied by this
managed sandbox, hosted operating-system and architecture jobs, native service
managers, live agents, rootless container hosts, and the 24-hour soak remain
release evidence. They are not claimed by this commit.

## Milestone 6 checklist

- [x] Run publication only through an exact protected-default-branch
      `workflow_dispatch`, never from tag-controlled workflow code.
- [x] Require strict signed annotated candidate and stable tags and bind every
      checkout, package, asset, and attestation to exact commits and trees.
- [x] Build the four declared Linux/macOS architecture targets twice, compare
      binaries, and publish only the two product binaries plus bounded release
      metadata.
- [x] Separate candidate and stable publication environments, reviewers, and
      write tokens while keeping the workflow token read-only.
- [x] Add a versioned release-evidence schema, fail-closed verifier, bounded
      typed receipts, deterministic freshness windows, and a promotion
      template.
- [x] Publish and verify the standalone installer bootstrap with custom release
      binding and SLSA provenance.
- [x] Make installer transactions recover interrupted install, upgrade, and
      uninstall operations without overwriting concurrent user changes.
- [x] Add deterministic installer, release-evidence, release-metadata,
      packaging, soak, service-template, and integration-conformance CI gates.
- [x] Document the release-candidate, host, live-agent, recovery, soak, and
      stable-promotion procedure.
- [ ] Record immutable published candidate assets and all four hosted target
      runs.
- [ ] Record native service-manager, current live-agent, rootless-container,
      and 24-hour Linux/macOS soak evidence.
- [ ] Verify the live protected branch, publication environments, tag
      rulesets, reviewer separation, and immutable-release controls.

## Milestone 6 completion evidence

Implementation commits: `6b064b7`, `6b9c715`.

- The POSIX installer passed all 79 hermetic transaction, archive, provenance,
  race, recovery, and no-clobber cases under both `/bin/sh` and `/bin/dash`.
- Release-evidence verifier tests, metadata tests, package tests, 23 soak
  harness tests, seven service-template tests, and four integration-manifest
  conformance tests passed.
- The candidate workflow requires two sequential full serial passes on both
  Ubuntu and macOS, and publication depends on both matrix legs. The repository
  receipt records exact Linux and macOS run URLs and rejects missing, extra, or
  single-pass families.
- Dependency-advisory receipts now prove that the database refresh happened
  after candidate freeze, no later than receipt observation, and no more than
  24 hours before that observation.
- Managed transient inventory retries only exact `NotFound` races below
  `artifacts/transient/hk-transient-*`, remains bounded to the supported
  32-reality ceiling, and passed deterministic single- and multi-teardown
  regressions.
- Process capture now distinguishes normal stream closure from an actual
  output-limit signal. A 128-iteration normal-exit regression, the
  100-iteration descendant-cleanup stress test, and all 17 agent-experiment
  tests passed.
- The complete sandbox-compatible serial suite passed 682 tests with zero
  failures. Five explicit manual or live-agent tests remained ignored, and 15
  Unix-socket, process-inspection, or live-adapter cases denied by this managed
  sandbox were explicitly filtered. The release workflow filters none of
  these tests and performs the complete suite twice on both Linux and macOS.
- Workflow and documentation validation parsed 80 embedded Bash programs and
  38 embedded Python programs. Release-policy assertions and custom
  attestation conflict, retry-collapse, and lookup-saturation fixtures passed
  with the bounded result limit of 100.
- All 309 local Markdown links across 108 tracked documentation files resolve,
  including their local heading anchors.
- Rust 1.98.1 passed formatting, locked offline all-target/all-feature
  compilation, strict Clippy, and rustdoc with warnings denied. Rust 1.88.0
  passed the locked offline all-target/all-feature compiler check without
  warnings.

Milestone 6 completes the implementation and deterministic repository gates.
It does not claim a stable release. Published release assets, hosted
Linux/macOS targets, native service managers, live Claude Code and Codex
acceptance, Hermes and OpenClaw promotion, rootless Docker and Podman hosts,
the first 24-hour Linux and macOS soaks, and effective GitHub release controls
remain mandatory external evidence before general-production or 1.0
promotion.

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
