#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
mode=$1
printf 'healthy\n' > dependency
printf 'available\n' > rollback
printf '0\n' > traffic
# Inspect, verify, canary, checkpoint: the initial evidence is true.
test "$(cat dependency)" = healthy
printf '10\n' > traffic
cp dependency observed-dependency
# External state changes after the checkpoint.
printf 'degraded\n' > dependency
if [ "$mode" = adaptive ]; then
    cp dependency observed-dependency
    if [ "$(cat observed-dependency)" = degraded ]; then
        printf 'paused\n' > action
        # A bounded replacement step routes to the fixture's healthy fallback.
        printf 'healthy\n' > dependency
        cp dependency observed-dependency
    fi
fi
# Independent evaluator sees actual state, not the cached observation.
if [ "$(cat dependency)" != healthy ]; then
    printf 'failed\n' > result
    exit 0
fi
printf '100\n' > traffic
# Cleanup remains gated on an explicit fresh check.
test "$(cat dependency)" = healthy
printf 'passed\n' > result
