#!/bin/sh
set -eu
# Memory pressure does not cause this fixture's stale-state failure.
if [ "$(cat state_refresh.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
