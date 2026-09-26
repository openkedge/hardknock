#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministic bidirectional protocol fixture; never calls a model/network."""
import json
import os
from pathlib import Path
import sys

MODE = os.environ.get("HARDKNOCK_CODEX_FIXTURE_MODE", "tested")
VERSION = "codex-cli 0.149.1" if MODE == "tested" else "codex-cli 0.999.0"

if sys.argv[1:] == ["--version"]:
    print(VERSION)
    raise SystemExit(0)
if "generate-json-schema" in sys.argv:
    target = Path(sys.argv[sys.argv.index("--out") + 1])

    def object_schema(properties, required=()):
        return {
            "type": "object",
            "properties": properties,
            "required": list(required),
            "additionalProperties": False,
        }

    string = {"type": "string"}
    schemas = {
        "v1/InitializeParams.json": object_schema(
            {
                "clientInfo": object_schema(
                    {"name": string, "title": string, "version": string},
                    ("name", "title", "version"),
                ),
                "capabilities": object_schema(
                    {"experimentalApi": {"type": "boolean"}},
                    ("experimentalApi",),
                ),
            },
            ("clientInfo", "capabilities"),
        ),
        "v2/ThreadStartParams.json": object_schema(
            {
                "cwd": {"type": ["string", "null"]},
                "developerInstructions": {"type": ["string", "null"]},
                "model": {"type": ["string", "null"]},
            }
        ),
        "v2/ThreadStartResponse.json": object_schema(
            {"thread": object_schema({"id": string}, ("id",))},
            ("thread",),
        ),
        "v2/ThreadResumeParams.json": object_schema(
            {
                "threadId": string,
                "cwd": {"type": ["string", "null"]},
                "model": {"type": ["string", "null"]},
            },
            ("threadId",),
        ),
        "v2/ThreadResumeResponse.json": object_schema(
            {"thread": object_schema({"id": string}, ("id",))},
            ("thread",),
        ),
        "v2/TurnStartParams.json": object_schema(
            {
                "threadId": string,
                "input": {
                    "type": "array",
                    "items": object_schema(
                        {
                            "type": {"const": "text"},
                            "text": string,
                            "text_elements": {"type": "array"},
                        },
                        ("type", "text", "text_elements"),
                    ),
                },
            },
            ("threadId", "input"),
        ),
        "v2/TurnStartResponse.json": object_schema(
            {"turn": object_schema({"id": string}, ("id",))},
            ("turn",),
        ),
        "v2/ItemStartedNotification.json": object_schema(
            {
                "threadId": string,
                "turnId": string,
                "item": object_schema({"id": string, "type": string}, ("id", "type")),
            },
            ("threadId", "turnId", "item"),
        ),
        "v2/ItemCompletedNotification.json": object_schema(
            {
                "threadId": string,
                "turnId": string,
                "item": object_schema({"id": string, "type": string}, ("id", "type")),
            },
            ("threadId", "turnId", "item"),
        ),
        "v2/TurnCompletedNotification.json": object_schema(
            {
                "threadId": string,
                "turn": object_schema(
                    {
                        "id": string,
                        "status": {
                            "type": "string",
                            "enum": ["completed", "interrupted", "failed"],
                        },
                    },
                    ("id", "status"),
                ),
            },
            ("threadId", "turn"),
        ),
    }
    if MODE == "incompatible":
        schemas["v2/TurnStartParams.json"]["properties"]["input"] = {
            "type": "string"
        }
    if MODE == "missing-required":
        schemas["v2/TurnCompletedNotification.json"]["properties"]["turn"][
            "required"
        ].remove("status")
    for filename, schema in schemas.items():
        path = target / filename
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps(schema))
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
        send({"id": request["id"], "result": {"userAgent": VERSION.replace(" ", "/")}})
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
        if any(item.get("type") == "text" and item.get("text") == "fixture-stall"
               for item in request["params"]["input"]):
            Path(cwd, "fixture-server.pid").write_text(str(__import__("os").getpid()))
            continue
        events = [json.loads(line) for line in Path(__file__).with_name("lifecycle.jsonl").read_text().splitlines()]
        for event in events[4:]:
            if event.get("id") == 900:
                continue  # Approval mapping has a separate explicit test.
            if "item" in event.get("params", {}) and "cwd" in event["params"]["item"]:
                event["params"]["item"]["cwd"] = cwd
            send(event)
