#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Deterministic, offline trial of Hardknock's `try` command. It builds the CLI
# if needed, stages the committed `strategy-choice` fixture in a throwaway Git
# repository, and compares two upgrade strategies under a required check. No
# model, package manager, network service, or installed agent is involved.
#
# Usage: scripts/demo.sh
# Override the binary with HARDKNOCK_BIN=/path/to/hardknock scripts/demo.sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture="$repo_root/fixtures/strategy-choice"

if [ -n "${HARDKNOCK_BIN:-}" ]; then
  hardknock_bin="$HARDKNOCK_BIN"
elif [ -x "$repo_root/target/debug/hardknock" ]; then
  hardknock_bin="$repo_root/target/debug/hardknock"
else
  echo "Building hardknock (cargo build --locked)..." >&2
  ( cd "$repo_root" && cargo build --locked )
  hardknock_bin="$repo_root/target/debug/hardknock"
fi

demo_root=$(mktemp -d)
trap 'rm -rf "$demo_root"' EXIT

project="$demo_root/project"
cp -R "$fixture" "$project"
git -C "$project" init -q -b main
git -C "$project" config user.name 'Hardknock Demo'
git -C "$project" config user.email 'demo@example.invalid'
git -C "$project" add .
git -C "$project" -c core.hooksPath=/dev/null -c commit.gpgsign=false \
  commit -q -m 'Strategy fixture'

echo "Comparing a direct vs. staged upgrade from one committed state..." >&2
"$hardknock_bin" --home "$demo_root/data" --repo "$project" try \
  --agent test-agent \
  --candidate 'direct=direct-upgrade' \
  --candidate 'staged=staged-upgrade' \
  --check './test.sh'

echo >&2
echo "Done. Expected: CONTROLLED, 'staged' recommended, two immutable" >&2
echo "Experiences recorded, and both disposable Realities discarded. The" >&2
echo "winning strategy is NOT applied to your source repository." >&2
