#!/bin/sh
set -eu
test "$(cat reconcile.input)" = true && printf 'PASS\n' > outcome.txt || printf 'FAIL\n' > outcome.txt
