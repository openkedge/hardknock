#!/bin/sh
set -eu
cp generation plan-generation
cp generation input-generation
# An explicit fixture fallback uses the local cached dependency.
printf '%s\n' cached > dependency
HK_FAILURES=0 /bin/sh ./operation.sh
