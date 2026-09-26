#!/usr/bin/env python3
"""Validate the static systemd and launchd user-service templates."""

from __future__ import annotations

import configparser
from pathlib import Path
import plistlib
import re
import shlex
import sys
import unittest


sys.dont_write_bytecode = True
REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
SYSTEMD_PATH = (
    REPOSITORY_ROOT / "packaging/systemd/hardknock-bridge.service"
)
LAUNCHD_PATH = (
    REPOSITORY_ROOT
    / "packaging/launchd/dev.openkedge.hardknock.bridge.plist"
)
SYSTEMD_ARGUMENTS = [
    "%h/.local/bin/hardknock",
    "--home",
    "%h/.hardknock",
    "bridge",
    "start",
    "--foreground",
]
LAUNCHD_BRIDGE_ARGUMENTS = [
    "__USER_HOME__/.local/bin/hardknock",
    "--home",
    "__USER_HOME__/.hardknock",
    "bridge",
    "start",
    "--foreground",
]
LAUNCHD_SHELL = (
    'set -o pipefail; "$@" 2>&1 | '
    "/usr/bin/logger -p user.notice -t hardknock-bridge"
)
LAUNCHD_ARGUMENTS = [
    "/bin/sh",
    "-c",
    LAUNCHD_SHELL,
    "hardknock-bridge",
    *LAUNCHD_BRIDGE_ARGUMENTS,
]


def systemd_configuration() -> tuple[str, configparser.RawConfigParser]:
    text = SYSTEMD_PATH.read_text(encoding="utf-8")
    parser = configparser.RawConfigParser(interpolation=None, strict=True)
    parser.optionxform = str
    parser.read_string(text)
    return text, parser


def launchd_configuration() -> tuple[str, dict[str, object]]:
    text = LAUNCHD_PATH.read_text(encoding="utf-8")
    configuration = plistlib.loads(text.encode("utf-8"))
    if not isinstance(configuration, dict):
        raise AssertionError("launchd template root must be a dictionary")
    return text, configuration


def seconds(value: str) -> int:
    duration = re.fullmatch(r"([1-9][0-9]*)s", value)
    if duration is None:
        raise AssertionError(f"expected a positive whole-second duration: {value}")
    return int(duration.group(1))


class ServiceTemplateTest(unittest.TestCase):
    def test_systemd_structure_command_and_paths(self) -> None:
        _, configuration = systemd_configuration()
        self.assertEqual(
            configuration.sections(),
            ["Unit", "Service", "Install"],
        )
        service = configuration["Service"]
        self.assertEqual(service["Type"], "simple")
        self.assertEqual(shlex.split(service["ExecStart"]), SYSTEMD_ARGUMENTS)
        self.assertEqual(service["UMask"], "0077")
        self.assertEqual(configuration["Install"]["WantedBy"], "default.target")
        self.assertNotIn("/target/", service["ExecStart"])
        self.assertNotIn("/bin/sh", service["ExecStart"])

    def test_systemd_restart_shutdown_and_output_policy(self) -> None:
        _, configuration = systemd_configuration()
        unit = configuration["Unit"]
        service = configuration["Service"]
        self.assertEqual(service["Restart"], "on-failure")
        self.assertLessEqual(seconds(service["RestartSec"]), 30)
        self.assertLessEqual(seconds(unit["StartLimitIntervalSec"]), 300)
        self.assertGreaterEqual(int(unit["StartLimitBurst"]), 1)
        self.assertLessEqual(int(unit["StartLimitBurst"]), 10)
        self.assertEqual(service["KillSignal"], "SIGTERM")
        self.assertEqual(service["KillMode"], "control-group")
        self.assertLessEqual(seconds(service["TimeoutStopSec"]), 60)
        self.assertEqual(service["StandardOutput"], "journal")
        self.assertEqual(service["StandardError"], "journal")
        self.assertEqual(service["SyslogIdentifier"], "hardknock-bridge")
        self.assertLessEqual(
            seconds(service["LogRateLimitIntervalSec"]),
            300,
        )
        self.assertGreaterEqual(int(service["LogRateLimitBurst"]), 1)
        self.assertLessEqual(int(service["LogRateLimitBurst"]), 10_000)
        self.assertEqual(service["NoNewPrivileges"], "true")

    def test_launchd_xml_structure_command_and_paths(self) -> None:
        _, configuration = launchd_configuration()
        self.assertEqual(
            configuration["Label"],
            "dev.openkedge.hardknock.bridge",
        )
        self.assertEqual(configuration["ProgramArguments"], LAUNCHD_ARGUMENTS)
        self.assertIs(configuration["RunAtLoad"], True)
        self.assertEqual(configuration["Umask"], 0o77)
        self.assertEqual(configuration["ProcessType"], "Background")
        self.assertEqual(configuration["StandardOutPath"], "/dev/null")
        self.assertEqual(configuration["StandardErrorPath"], "/dev/null")

    def test_launchd_restart_and_shutdown_policy(self) -> None:
        _, configuration = launchd_configuration()
        self.assertEqual(
            configuration["KeepAlive"],
            {"SuccessfulExit": False},
        )
        self.assertGreaterEqual(configuration["ThrottleInterval"], 1)
        self.assertLessEqual(configuration["ThrottleInterval"], 30)
        self.assertGreaterEqual(configuration["ExitTimeOut"], 1)
        self.assertLessEqual(configuration["ExitTimeOut"], 60)
        self.assertIs(configuration["AbandonProcessGroup"], False)

    def test_launchd_placeholder_is_documented_without_runtime_expansion(self) -> None:
        text, configuration = launchd_configuration()
        placeholders = set(re.findall(r"__[A-Z][A-Z0-9_]*__", text))
        self.assertEqual(placeholders, {"__USER_HOME__"})
        self.assertIn(
            "Replace every __USER_HOME__ with the user's absolute home directory",
            text,
        )
        arguments = configuration["ProgramArguments"]
        self.assertIsInstance(arguments, list)
        for value in [
            *LAUNCHD_BRIDGE_ARGUMENTS,
            configuration["StandardOutPath"],
            configuration["StandardErrorPath"],
        ]:
            self.assertNotIn("$", value)
            self.assertNotIn("~", value)

    def test_launchd_routes_output_to_platform_logging_safely(self) -> None:
        text, configuration = launchd_configuration()
        arguments = configuration["ProgramArguments"]
        self.assertEqual(arguments[:4], [
            "/bin/sh",
            "-c",
            LAUNCHD_SHELL,
            "hardknock-bridge",
        ])
        self.assertEqual(arguments[4:], LAUNCHD_BRIDGE_ARGUMENTS)
        self.assertIn("set -o pipefail", arguments[2])
        self.assertIn('"$@"', arguments[2])
        self.assertIn("/usr/bin/logger", arguments[2])
        self.assertNotIn("__USER_HOME__", arguments[2])
        self.assertNotIn("eval", arguments[2])
        self.assertNotIn(".hardknock/logs", text)
        self.assertNotRegex(text, r"bridge\.(?:stdout|stderr)\.log")
        self.assertIn("Inspect startup failures in Console.app", text)

    def test_templates_exclude_tcp_and_privilege_escalation(self) -> None:
        systemd_text, systemd = systemd_configuration()
        launchd_text, launchd = launchd_configuration()
        combined = f"{systemd_text}\n{launchd_text}".lower()
        for forbidden in (
            "--tcp",
            "sudo",
            "pkexec",
            "cap_sys_admin",
            "networkstate",
        ):
            self.assertNotIn(forbidden, combined)

        service = systemd["Service"]
        for forbidden_key in (
            "User",
            "Group",
            "SupplementaryGroups",
            "AmbientCapabilities",
            "PermissionsStartOnly",
            "ExecStartPre",
            "ExecStartPost",
        ):
            self.assertNotIn(forbidden_key, service)

        for forbidden_key in (
            "UserName",
            "GroupName",
            "InitGroups",
            "RootDirectory",
            "Sockets",
            "inetdCompatibility",
        ):
            self.assertNotIn(forbidden_key, launchd)


if __name__ == "__main__":
    unittest.main()
