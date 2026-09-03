#!/bin/sh
set -eu
# Neither high latency nor retry pressure alone is sufficient.
if [ "$(cat latency.input)" -ge 1000 ] && [ "$(cat retry_count.input)" -ge 3 ]; then printf 'FAIL\n' > outcome.txt; else printf 'PASS\n' > outcome.txt; fi
