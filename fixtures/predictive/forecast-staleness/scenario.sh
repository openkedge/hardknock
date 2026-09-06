#!/bin/sh
set -eu
test "$(cat runtime_version.input)" = v1 && printf 'FAIL\n' > outcome.txt || printf 'PASS\n' > outcome.txt
