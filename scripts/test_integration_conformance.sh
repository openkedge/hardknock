#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu

cd "$(dirname "$0")/.."
exec cargo test --locked --offline --test integration_manifest
