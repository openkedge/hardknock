#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
# A leftover lockfile means the runner did not reconstruct a clean starting state.
if [ -e package-lock.json ]; then
    echo 'ERROR dirty fixture starting state' >&2
    exit 9
fi
case "${1:-}" in
    baseline)
        echo 'ACTION shell npm install'
        printf '{"simulated":true}\n' > package-lock.json
        echo 'RESULT simulated dependency conflict'
        ;;
    alternative)
        echo 'ACTION shell pnpm install'
        test -f pnpm-lock.yaml
        echo 'RESULT simulated workspace state preserved'
        ;;
    *) echo 'usage: agent-script.sh baseline|alternative' >&2; exit 2 ;;
esac
