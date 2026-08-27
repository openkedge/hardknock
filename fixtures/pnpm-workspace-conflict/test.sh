#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
test -f pnpm-workspace.yaml
test -f pnpm-lock.yaml
test -f packages/demo/package.json
if [ -e package-lock.json ]; then
    echo 'FAIL package_manager_conflict duplicate_lockfile' >&2
    exit 1
fi
echo 'PASS simulated pnpm workspace state'
