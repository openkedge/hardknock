# SPDX-License-Identifier: Apache-2.0
"""Hermes plugin API; no SQLite, transcripts, environment dumps, or hidden reasoning."""
import json
import logging
import os
from pathlib import Path
import socket
import stat
import subprocess
import time
import uuid

PROTOCOL = "hardknock.bridge.v1"
LOG = logging.getLogger("hardknock")


class BridgeClient:
    def __init__(self, home=None):
        self.home = Path(home or os.environ.get("HARDKNOCK_HOME", "~/.hardknock")).expanduser()

    def _private(self, name):
        path = self.home / "run" / name
        meta = path.lstat()
        if not stat.S_ISREG(meta.st_mode) or meta.st_mode & 0o077 or meta.st_size > 8192:
            raise ValueError("Bridge runtime file must be a private regular file")
        return path.read_text()

    def request(self, event, data=None, timeout=0.2):
        started = time.monotonic()
        self.home = self.home.resolve()
        endpoint = json.loads(self._private("bridge-endpoint.json"))
        token = self._private("bridge-token")
        request_id = str(uuid.uuid4())
        payload = {"event": event}
        if data is not None:
            payload["data"] = data
        body = (json.dumps({"protocol_version": PROTOCOL, "request_id": request_id,
                            "token": token, "payload": payload}) + "\n").encode()
        if len(body) > 1048576:
            raise ValueError("Bridge request exceeds 1 MiB")
        if endpoint["transport"] == "unix":
            if Path(endpoint["path"]) != self.home / "run" / "hardknock.sock":
                raise ValueError("Unexpected Bridge socket")
            family, address = socket.AF_UNIX, endpoint["path"]
        else:
            host, port = endpoint["address"].rsplit(":", 1)
            if host != "127.0.0.1":
                raise ValueError("Bridge must be local")
            family, address = socket.AF_INET, (host, int(port))
        with socket.socket(family, socket.SOCK_STREAM) as connection:
            connection.settimeout(timeout)
            connection.connect(address)
            connection.sendall(body)
            response = bytearray()
            while b"\n" not in response:
                remaining = timeout - (time.monotonic() - started)
                if remaining <= 0:
                    raise TimeoutError("Bridge deadline exceeded")
                connection.settimeout(remaining)
                chunk = connection.recv(16384)
                if not chunk:
                    raise ConnectionError("Bridge disconnected")
                response.extend(chunk)
                if len(response) > 1048576:
                    raise ValueError("Bridge response too large")
        response = json.loads(response.partition(b"\n")[0])
        if response.get("protocol_version") != PROTOCOL or response.get("request_id") != request_id:
            raise ValueError("Bridge response mismatch")
        if not response.get("ok"):
            raise ValueError("Bridge rejected lifecycle event")
        return response["payload"]


def normalize(tool_name, args, cwd):
    if not isinstance(tool_name, str) or not tool_name or not isinstance(args, dict):
        raise ValueError("Malformed native tool proposal")
    if tool_name == "terminal" and isinstance(args.get("command"), str):
        return {"type": "shell", "command": args["command"], "cwd": args.get("cwd", cwd)}
    if tool_name in ("write_file", "read_file", "patch") and isinstance(args.get("path"), str):
        return {"type": "file_read" if tool_name == "read_file" else "file_write", "path": args["path"]}
    return {"type": "tool_call", "tool": tool_name, "arguments": {"arguments_omitted": True}}


def register(ctx):
    client = BridgeClient()
    sessions = {}
    # This switch is independent user policy, never learned from an experience.
    policy_required = os.environ.get("HARDKNOCK_POLICY_REQUIRED") == "1"

    def unavailable():
        LOG.warning("Hardknock advisory unavailable (payload omitted)")

    def on_start(session_id, model=None, **kwargs):
        if not session_id:
            return
        try:
            cwd = str(Path(kwargs.get("cwd") or os.getcwd()).resolve())
            data = {"session_id": session_id, "agent": {"name": "hermes", "adapter_version": "0.3.0", "model": model},
                    "cwd": cwd, "environment": {}}
            try:
                result = client.request("session_started", data, timeout=5)
            except (OSError, ValueError):
                import tomllib
                config_path = client.home / "config.toml"
                config = tomllib.loads(config_path.read_text()) if config_path.is_file() else {}
                if not config.get("bridge", {}).get("autostart", True):
                    raise
                subprocess.run([os.environ.get("HARDKNOCK_BIN", "hardknock"), "--home", str(client.home), "bridge", "start"],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=6, check=True)
                result = client.request("session_started", data, timeout=5)
            sessions[session_id] = {"id": result["hardknock_session_id"], "cwd": cwd,
                                    "context": result.get("context_document") or "", "advice": [], "actions": {},
                                    "run": str(uuid.uuid4()), "started": time.monotonic()}
        except (OSError, ValueError, subprocess.SubprocessError):
            unavailable()

    def pre_llm_call(session_id=None, **kwargs):
        if session_id not in sessions:
            on_start(session_id, **kwargs)
        session = sessions.get(session_id)
        if session:
            try:
                fresh = client.request("context_requested", {"hardknock_session_id": session["id"]}, timeout=2)
                session["context"] = fresh.get("context_document") or ""
            except (OSError, ValueError):
                unavailable()
            session["run"] = str(uuid.uuid4())
            session["started"] = time.monotonic()
            text = "\n".join([session["context"], *session["advice"]])[:32768]
            session["advice"].clear()
            return {"context": text}
        return None

    def pre_tool_call(tool_name, args, session_id=None, tool_call_id=None, **kwargs):
        session = sessions.get(session_id)
        if not session or not tool_call_id:
            return {"action": "block", "message": "Required Hardknock policy unavailable"} if policy_required else None
        try:
            action = normalize(tool_name, args, session["cwd"])
            decision = client.request("action_proposed", {"hardknock_session_id": session["id"], "action_id": tool_call_id,
                "action": action, "context": {"can_intercept": True}}, timeout=0.02)
            session["actions"][tool_call_id] = action
            if decision["decision"] == "block" and decision.get("authority") in ("user_policy", "external_policy"):
                return {"action": "block", "message": decision["reason"]}
            if decision["decision"] == "require_approval":
                # Hermes "approve" requests human approval; it does not grant permission.
                return {"action": "approve", "message": decision["reason"]}
            message = decision.get("message") or decision.get("reason")
            if message:
                session["advice"] = (session["advice"] + [message])[-5:]
        except (OSError, ValueError):
            unavailable()
            if policy_required:
                return {"action": "block", "message": "Required Hardknock policy unavailable"}
        return None

    def post_tool_call(tool_name, args, result, session_id=None, tool_call_id=None, duration_ms=0, **kwargs):
        session = sessions.get(session_id)
        if not session or tool_call_id not in session["actions"]:
            return
        try:
            parsed = json.loads(result) if isinstance(result, str) else result
            parsed = parsed if isinstance(parsed, dict) else {}
            exit_code = parsed.get("exit_code")
            success = not parsed.get("error") and parsed.get("success", True) and exit_code in (None, 0)
            client.request("action_completed", {"hardknock_session_id": session["id"], "action_id": tool_call_id,
                "action": session["actions"].pop(tool_call_id), "duration_ms": max(0, int(duration_ms)),
                "result": {"success": bool(success), "exit_code": exit_code, "error_class": None if success else "tool_failure"}})
        except (OSError, ValueError, TypeError):
            unavailable()

    def on_end(session_id, completed=False, interrupted=False, **kwargs):
        session = sessions.get(session_id)
        if not session:
            return
        try:
            client.request("run_completed", {"hardknock_session_id": session["id"], "run_id": session["run"],
                "success": bool(completed and not interrupted), "termination": "interrupted" if interrupted else "completed",
                "duration_ms": int((time.monotonic()-session["started"])*1000)})
        except (OSError, ValueError):
            unavailable()

    def on_finalize(session_id=None, **kwargs):
        session = sessions.pop(session_id, None)
        if session:
            try:
                client.request("session_ended", {"hardknock_session_id": session["id"]})
            except (OSError, ValueError):
                unavailable()

    for event, callback in (("on_session_start", on_start), ("pre_llm_call", pre_llm_call),
                            ("pre_tool_call", pre_tool_call), ("post_tool_call", post_tool_call),
                            ("on_session_end", on_end), ("on_session_finalize", on_finalize)):
        ctx.register_hook(event, callback)
