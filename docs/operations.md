# Production Operations

The implemented controlled-candidate boundary is one local user on glibc-based
Linux or macOS. Keep `HARDKNOCK_HOME` outside source repositories and use one
dedicated data home per installation. The examples below use:

```bash
export HARDKNOCK_HOME="$HOME/.hardknock"
```

## Binary installation and managed setup

Follow the canonical
[fail-closed binary-release verification](../README.md#install-a-published-release)
before executing the downloaded `install-hardknock` bootstrap. That procedure
keeps the signed stable tag that identifies the candidate artifact separate
from the protected default-branch commit that signed the release attestations.
GitHub CLI 2.97.0 or newer is required for the official provenance path.

The canonical block leaves a verified executable in the current directory.
Preview and apply the installation and managed setup:

```bash
set -euo pipefail

VERSION='<version>'
./install-hardknock --version "$VERSION" --dry-run --json
./install-hardknock --version "$VERSION"
"$HOME/.local/bin/hardknock" setup \
  --agent auto \
  --mode workstation \
  --non-interactive \
  --start \
  --json
```

For an autonomous CI installer, use `--no-modify-path --json` and invoke the
installed binary by its explicit prefix path. The same setup command and
machine-readable result are used by interactive and agentic systems.

The installer accepts the official HTTPS release base, an absolute local
mirror, or a `file://` mirror. Official downloads fail closed unless GitHub CLI
binds the archive to the requested `openkedge/hardknock` release tag and
verifies an attestation from the repository's release workflow. Provenance and
candidate-binary checks have wall-clock and output limits. A custom HTTPS
source requires the explicit `--no-verify-provenance` bypass. Local mirrors
remain available for offline agents and report checksum-only provenance. The
installer rejects insecure HTTP, archive traversal, links, oversized members,
checksum ambiguity, changed same-version payloads, unsafe prefixes, concurrent
transactions, and unmanaged destination collisions. Interrupted installer
transactions are recovered under the installation lock before a new mutation.
It installs two binaries plus license, notice, and an integrity manifest. PATH
changes use one bounded managed block in the selected login profile and can be
disabled with `--no-modify-path`.

Setup emits its full plan before applying it. Applied steps are recorded in
owner-private JSONL under `$HARDKNOCK_HOME/setup/transactions/`. Mutations take
a bounded transaction lock. Rollback snapshots contain only bounded managed
files; journal output records paths and results, not prior user file contents.
Rollback compares each current file with the fingerprint recorded immediately
after the setup step, preserving concurrent changes instead of overwriting
them. Workstation mode installs an exact managed `systemd --user` or launchd
definition. CI mode uses the documented on-demand Bridge command.

After replacing the binaries with a newer pinned release:

```bash
hardknock upgrade --agent auto --mode workstation --non-interactive --start --json
hardknock repair --agent auto --mode workstation --non-interactive --json
```

Upgrade preserves recorded agent choices, creates a verified recovery
boundary, performs any database migration once, refreshes exact managed files,
and runs strict doctor. Repair performs the same ownership and health checks
without forcing an extra backup when a managed recovery point already exists.

Managed removal is non-destructive by default:

```bash
hardknock uninstall --dry-run --json
hardknock uninstall --non-interactive --json
```

This stops the Bridge when reachable and removes only exact managed adapter,
service, and setup-manifest files. If a later uninstall step fails, managed
files are restored and a Bridge that was running before the attempt is
restarted. An absent managed installation is a true no-op. Successful removal
retains the database, artifacts, backups, and transaction history.
`--remove-data` additionally removes the exact owner-private home only when its
setup manifest matches that path. The home first moves atomically to a private
sibling quarantine. If deletion is interrupted after that commit point,
ordinary setup remains fail-closed and `hardknock repair --non-interactive`
resumes the deletion before applying a new managed state.

## Readiness checks

Run the strict doctor after installation, configuration changes, upgrades, and
recovery:

```bash
hardknock --json doctor --strict
```

The report checks the applied schema, private path ownership and modes, disk
capacity, stale runtime resources, backup status, managed release integrity,
Bridge health, configured adapter compatibility, and artifact limits.
Strict readiness requires at least one verified bundle under
`$HARDKNOCK_HOME/backups/`; setup and upgrades create managed recovery points.

Strict mode uses stable process exit codes:

| Exit | Meaning |
| --- | --- |
| `0` | Ready: every required production check passed |
| `1` | Degraded: at least one warning needs attention |
| `2` | Not ready: a required check failed or is unavailable |

Without `--strict`, doctor still reports the same findings but keeps the normal
management-command exit behavior.

## Migration inspection and recovery backups

Inspect an existing home before opening it with a newer binary:

```bash
hardknock --json migration dry-run
```

The command does not create the home or modify its database. When `Store::open`
finds an older nonempty schema, Hardknock takes the home maintenance lock,
waits for artifact producers to finish, reserves SQLite's write transaction,
creates and verifies a pre-migration backup under
`$HARDKNOCK_HOME/backups/`, and then applies all migrations in one SQLite
transaction. A failed migration rolls back the schema change and leaves the
verified backup available for recovery.

Hardknock has no down migrations. To return to an older binary, restore its
pre-upgrade backup.

## Explicit backup and restore

Create a backup in a new directory outside the managed data home:

```bash
hardknock --json backup "$HOME/hardknock-backups/pre-change"
```

Backup creation uses SQLite's online backup API, copies the artifact tree,
records BLAKE3 digests and sizes in a manifest, checks database integrity and
foreign keys, and verifies the completed bundle before publishing it. The
destination must not already exist. Backup, restore, and migration operations
share a bounded maintenance lock. Backup and migration also close the
artifact-reservation gate and refuse to start while an artifact producer is
active, so the database snapshot and copied evidence represent one idle
boundary. Backup traversal is iterative and deterministic, with a maximum of
100,000 source entries and 64 path components. Verification applies the same
nesting limit and a bounded inventory that also counts empty directories.

Restore always verifies the bundle before changing the target:

```bash
mv "$HOME/.hardknock" "$HOME/.hardknock.failed"
hardknock --home "$HOME/.hardknock" --json \
  restore --verify \
  "$HOME/.hardknock.failed/backups/pre-migration-<schema>-to-<schema>-<time>"
```

The target home must be missing or empty and must exactly match the source-home
path recorded in the backup. Current database artifact references contain
absolute paths, so relocation during restore is not supported. Move or rename
a damaged home first; do not point restore at a populated directory. Restore
does not overwrite an existing installation.

Treat a backup as sensitive local data. It can contain task text, command
output, diffs, and other recorded evidence.

## Artifact limits and retention

Default policy:

```toml
[storage]
max_bytes = 2147483648
max_files = 20000
min_free_bytes = 1073741824
max_scan_entries = 100000
max_prune_items = 10000
```

Inspect current use and configured limits:

```bash
hardknock --json storage status
hardknock --json storage check-capacity --bytes 1048576 --files 16
```

All artifact files count toward the byte and file limits. Only regular,
single-link files below `artifacts/transient/` are reclaimable. Experience
evidence and every artifact outside that subtree are protected. Hardknock
refuses automatic deletion when capacity is low.

Pruning is a separate, explicit operation:

```bash
# Plan only; this is the default.
hardknock --json storage prune

# Apply the bounded oldest-first plan.
hardknock --json storage prune --apply
```

Apply mode revalidates each candidate and stops on symlinks, special files,
hard links, changed entries, scan limits, or an insufficient reclaimable set.
It never deletes protected evidence.

Artifact-producing paths perform a bounded capacity reservation before work
starts. Reservations are recorded in private, locked files and counted
together, so parallel experiment candidates can run while their aggregate
worst-case output remains within the configured limits. A crashed process
leaves an unlocked reservation that the next capacity check removes.

Each captured subprocess stream is limited to 8 MiB. Crossing the limit stops
the process group and retains the bounded output with an explicit marker.
Container actions use the capability manifest's per-stream output limit, with
an 8 MiB default, and stop the disposable container on timeout, capture
failure, or overflow. Generated Git diffs are limited to 16 MiB. These
application limits complement the storage reservation; they are not a
kernel-enforced filesystem quota.

## Bridge restart and diagnostics

Bridge startup takes an exclusive runtime lock before publishing its Unix
socket. It removes stale endpoint, token, socket, and relay paths only after it
has proved that no Bridge owns the home. It also cleans unlocked automatic
Realities and records interrupted Bridge-owned runs, strategy experiments, and
curricula with terminal or partial status before accepting new traffic.

Detached startup writes structured diagnostics to:

```text
$HARDKNOCK_HOME/logs/bridge.jsonl
$HARDKNOCK_HOME/logs/bridge.1.jsonl
...
$HARDKNOCK_HOME/logs/bridge.4.jsonl
```

The active file and each of four archives are limited to 1 MiB, for a maximum
of 5 MiB. Unsafe log paths, owners, modes, link counts, and file changes are
rejected. Foreground mode continues to write diagnostics to stderr:

```bash
hardknock bridge start --foreground
```

Startup reconciliation cannot undo external effects or find descendants that
deliberately escaped their process group. Inspect preserved evidence before
retrying interrupted work.

## User service templates

The templates expect `hardknock` at `$HOME/.local/bin/hardknock` and use
`$HOME/.hardknock` as the data home.

On a systemd user session:

```bash
mkdir -p "$HOME/.config/systemd/user"
install -m 600 packaging/systemd/hardknock-bridge.service \
  "$HOME/.config/systemd/user/hardknock-bridge.service"
systemctl --user daemon-reload
systemctl --user enable --now hardknock-bridge.service
systemctl --user status hardknock-bridge.service
journalctl --user -u hardknock-bridge.service
```

On macOS, copy the launchd template and replace every `__USER_HOME__` token
with the absolute home path before loading it:

```bash
mkdir -p "$HOME/Library/LaunchAgents"
sed "s|__USER_HOME__|$HOME|g" \
  packaging/launchd/dev.openkedge.hardknock.bridge.plist \
  > "$HOME/Library/LaunchAgents/dev.openkedge.hardknock.bridge.plist"
chmod 600 "$HOME/Library/LaunchAgents/dev.openkedge.hardknock.bridge.plist"
plutil -lint \
  "$HOME/Library/LaunchAgents/dev.openkedge.hardknock.bridge.plist"
launchctl bootstrap "gui/$(id -u)" \
  "$HOME/Library/LaunchAgents/dev.openkedge.hardknock.bridge.plist"
launchctl kickstart -k \
  "gui/$(id -u)/dev.openkedge.hardknock.bridge"
```

The systemd unit sends output to the user journal. The launchd job sends output
to the macOS unified log with the `hardknock-bridge` tag. Both use restart
backoff, owner-only creation modes, SIGTERM, and a bounded shutdown interval.
Edit the executable and home arguments before installation if those paths
differ.

## Bridge soak

Build the release binary and run the standalone harness against a dedicated
home:

```bash
cargo build --release --locked --offline
python3 scripts/bridge_soak.py target/release/hardknock \
  --home /tmp/hardknock-bridge-soak
```

The default duration is 86,400 seconds. During the run, the harness repeatedly
checks Bridge status, persistence health, endpoint identity, daemon and
descendant liveness, database record counts, managed Reality directories and
Git worktrees, label-filtered Docker/Podman resources, and diagnostic bounds.
It also runs an offline lifecycle workload, verifies that its session and run
survive restart, then checks shutdown cleanup and a second clean start and
stop. Every probe reports `checked`, `partial`, or `unavailable`; the result
does not claim complete coverage when a host capability is missing. Successful
output is one JSON object suitable for retention as release evidence.

Use `--duration-seconds 5 --poll-interval-seconds 0.1` for a local smoke run.
The harness's own deterministic tests are:

```bash
python3 scripts/test_bridge_soak.py
python3 scripts/test_service_templates.py
```

The required 24-hour Linux and macOS results remain release evidence until
those host runs have completed; the presence of the harness is not a claim
that they passed.
