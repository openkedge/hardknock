#!/bin/sh
set -eu
test "$(cat reprepare.input)" = true && printf 'PASS\n' > outcome.txt || printf 'FAIL\n' > outcome.txt
