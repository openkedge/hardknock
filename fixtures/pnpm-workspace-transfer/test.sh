#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
test -f packages/service/package.json
test -f packages/worker/package.json
test -f dependencies-updated.txt
if [ -e package-lock.json ]; then
    echo 'FAIL package_manager_conflict duplicate_lockfile' >&2
    exit 1
fi
echo 'PASS distinct service/worker dependency task'
