#!/bin/sh
set -eu
test "$(cat state_refresh.input)" = true && printf 'PASS\n' > outcome.txt || printf 'FAIL\n' > outcome.txt
