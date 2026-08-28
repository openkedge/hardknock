#!/bin/sh
set -eu
test "$(cat result 2>/dev/null)" = success
if [ "$(cat fixture-kind)" = config-drift ]; then
  test "$(cat generation)" = "$(cat plan-generation)"
fi
