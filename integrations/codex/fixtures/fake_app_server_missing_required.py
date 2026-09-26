#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Unknown-version fixture that makes a consumed notification field optional."""
import os
from pathlib import Path
import runpy

os.environ["HARDKNOCK_CODEX_FIXTURE_MODE"] = "missing-required"
runpy.run_path(
    Path(__file__).with_name("fake_app_server.py"),
    run_name="__main__",
)
