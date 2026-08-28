# SPDX-License-Identifier: Apache-2.0
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
spec = importlib.util.spec_from_file_location("hardknock_hermes", Path(__file__).with_name("__init__.py"))
plugin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plugin)

class Client:
    def __init__(self):
        self.events = []
        self.decision = {"decision": "continue"}
        self.fail = False
    def request(self, event, data=None, timeout=0.2):
        if self.fail:
            raise TimeoutError("simulated timeout")
        self.events.append((event, data))
        if event in ("session_started", "context_requested"):
            return {"hardknock_session_id": "hk-s-test", "context_document": "Experience, not policy"}
        return self.decision if event == "action_proposed" else {"accepted": True}

class Host:
    def __init__(self):
        self.hooks = {}
    def register_hook(self, name, callback):
        self.hooks[name] = callback

class PluginTests(unittest.TestCase):
    def test_lifecycle_and_advisory_does_not_block(self):
        host, client = Host(), Client()
        with patch.object(plugin, "BridgeClient", return_value=client):
            plugin.register(host)
        h = host.hooks
        h["on_session_start"]("session", model="fixture", platform="cli")
        self.assertIn("Experience", h["pre_llm_call"](session_id="session")["context"])
        client.decision = {"decision": "replan", "reason": "Prefer validated action"}
        self.assertIsNone(h["pre_tool_call"]("terminal", {"command": "npm install"}, session_id="session", tool_call_id="a"))
        h["post_tool_call"]("terminal", {}, '{"exit_code":1}', session_id="session", tool_call_id="a", duration_ms=10)
        self.assertFalse(client.events[-1][1]["result"]["success"])
        self.assertIn("Prefer validated", h["pre_llm_call"](session_id="session")["context"])
        h["on_session_end"]("session", completed=True)
        self.assertEqual(client.events[-1][0], "run_completed")
        h["on_session_finalize"]("session")
        self.assertEqual(client.events[-1][0], "session_ended")
    def test_timeout_and_policy_separation(self):
        host, client = Host(), Client()
        with patch.object(plugin, "BridgeClient", return_value=client):
            plugin.register(host)
        host.hooks["on_session_start"]("session")
        self.assertIsNone(host.hooks["pre_tool_call"]("terminal", None, session_id="session", tool_call_id="malformed"))
        client.fail = True
        self.assertIsNone(host.hooks["pre_tool_call"]("terminal", {"command":"x"},session_id="session",tool_call_id="a"))
        client.fail = False
        client.decision = {"decision":"block","reason":"Explicit policy","authority":"user_policy"}
        self.assertEqual(host.hooks["pre_tool_call"]("terminal", {"command":"x"},session_id="session",tool_call_id="b")["action"],"block")
        result = host.hooks["pre_tool_call"]("terminal", {"command":"x"},session_id="session",tool_call_id="c")
        self.assertTrue(result["message"])
        client.decision = {"decision":"require_approval","reason":"Ask the user"}
        self.assertEqual(host.hooks["pre_tool_call"]("terminal", {"command":"x"},session_id="session",tool_call_id="d")["action"], "approve")
    def test_tool_content_is_not_retained(self):
        self.assertNotIn("secret body", str(plugin.normalize("write_file", {"path":"a", "content":"secret body"}, "/tmp")))
if __name__ == "__main__":
    unittest.main()
