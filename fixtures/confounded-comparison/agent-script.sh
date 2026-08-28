#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
case "$2:$1" in
  fake-agent-A:strategy-A) printf 'v2\n' > api-version ;;
  fake-agent-B:strategy-B) printf 'v2\n' > api-version; printf 'v2\n' > consumer-version ;;
  *) printf 'unknown fixture agent/strategy\n' >&2; exit 2 ;;
esac
