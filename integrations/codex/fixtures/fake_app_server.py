#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministic bidirectional protocol fixture; never calls a model/network."""
import json
from pathlib import Path
import sys

if sys.argv[1:] == ["--version"]:
    print("codex-cli 0.149.1")
    raise SystemExit(0)
if "generate-json-schema" in sys.argv:
    target = Path(sys.argv[sys.argv.index("--out") + 1])
    for filename, fields in {
        "v1/InitializeParams.json": ["clientInfo"],
        "v2/ThreadStartParams.json": ["cwd", "developerInstructions"],
        "v2/TurnStartParams.json": ["threadId", "input"],
        "v2/ItemStartedNotification.json": ["item", "threadId", "turnId"],
    }.items():
        path = target / filename
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps({"properties": {field: {} for field in fields}}))
    raise SystemExit(0)

def send(value):
    print(json.dumps(value), flush=True)

initialized = False
cwd = None
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        assert not initialized
        send({"id": request["id"], "result": {"userAgent": "codex-cli/0.149.1"}})
    elif method == "initialized":
        initialized = True
    elif method in ("thread/start", "thread/resume"):
        assert initialized
        assert "approvalPolicy" not in request["params"] and "sandbox" not in request["params"]
        assert "developerInstructions" not in request["params"] and "baseInstructions" not in request["params"]
        cwd = request["params"]["cwd"]
        send({"id": request["id"], "result": {"thread": {"id": "thread-fixture"}}})
    elif method == "turn/start":
        assert initialized and request["params"]["input"][0]["type"] == "text"
        send({"id": request["id"], "result": {"turn": {"id": "turn-fixture", "status": "inProgress"}}})
        if request["params"]["input"][0]["text"] == "fixture-stall":
            Path(cwd, "fixture-server.pid").write_text(str(__import__("os").getpid()))
            continue
        events = [json.loads(line) for line in Path(__file__).with_name("lifecycle.jsonl").read_text().splitlines()]
        for event in events[4:]:
            if event.get("id") == 900:
                continue  # Approval mapping has a separate explicit test.
            if "item" in event.get("params", {}) and "cwd" in event["params"]["item"]:
                event["params"]["item"]["cwd"] = cwd
            send(event)
