#!/bin/sh
set -eu
test "$(cat state_stale.input)" = true && printf 'FAIL\n' > outcome.txt || printf 'PASS\n' > outcome.txt
