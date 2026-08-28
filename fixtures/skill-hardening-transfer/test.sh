#!/bin/sh
set -eu
test "$(cat result 2>/dev/null)" = success
test "$(cat generation)" = "$(cat plan-generation)"
test "$(cat generation)" = "$(cat input-generation)"
