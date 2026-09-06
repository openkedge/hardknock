#!/bin/sh
set -eu
test "$(cat specialized_tool.input)" = true && printf 'PASS\n' > outcome.txt || printf 'FAIL\n' > outcome.txt
