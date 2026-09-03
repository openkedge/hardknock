#!/bin/sh
set -eu
# Dependency loss contradicts previously useful refresh guidance.
if [ "$(cat state_refresh.input)" = true ] && [ "$(cat dependency_available.input)" = true ]; then printf 'PASS\n' > outcome.txt; else printf 'FAIL\n' > outcome.txt; fi
