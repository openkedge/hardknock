#!/bin/sh
set -eu
# Local cache implementation differs from the remote report: refresh alone does not repair failure.
printf 'FAIL\n' > outcome.txt
