#!/bin/sh
set -eu
# More retries cannot repair stale state.
if [ "$(cat state_refresh.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
