#!/bin/sh
set -eu
# Trusted deterministic fixture: latency/retries co-occur but do not affect this mechanism.
if [ "$(cat state_refresh.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
