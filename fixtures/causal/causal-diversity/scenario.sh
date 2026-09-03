#!/bin/sh
set -eu
# Same mechanism, alternate deterministic checker can inspect this output.
if [ "$(cat state_refresh.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
