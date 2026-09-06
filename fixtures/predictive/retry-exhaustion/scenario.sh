#!/bin/sh
set -eu
# Same deterministic mechanism used by the causal and predictive counterfactuals.
if [ -f reprepare.input ]; then
  test "$(cat reprepare.input)" = true && printf 'PASS\n' > outcome.txt || printf 'FAIL\n' > outcome.txt
elif [ "$(cat state_refresh.input)" = true ]; then
  printf 'PASS\n' > outcome.txt
else
  printf 'FAIL\n' > outcome.txt
fi
