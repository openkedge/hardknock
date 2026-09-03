#!/bin/sh
set -eu
# Tool version is an explicit known confounder.
if [ "$(cat tool_version.input)" = v2 ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
