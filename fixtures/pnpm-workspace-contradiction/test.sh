#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
if [ ! -f package-lock.json ] || [ -f pnpm-attempted ]; then
    echo 'FAIL this task requires simulated npm-compatible output' >&2
    exit 1
fi
echo 'PASS simulated npm-compatible dependency task'
