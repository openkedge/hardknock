#!/bin/sh
set -eu
# Refresh only works while the dependency is available.
if [ "$(cat state_refresh.input)" = true ] && [ "$(cat dependency_available.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
