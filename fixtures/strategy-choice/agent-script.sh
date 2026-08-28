#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
case "$1" in
  direct-upgrade) printf 'v2\n' > api-version ;;
  staged-upgrade) printf 'v2\n' > api-version; printf 'v2\n' > consumer-version ;;
  *) printf 'unknown fixture strategy\n' >&2; exit 2 ;;
esac
