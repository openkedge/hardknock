#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
test "$(cat api-version)" = v2
test "$(cat consumer-version)" = v2
