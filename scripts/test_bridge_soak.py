#!/usr/bin/env python3
"""Fast tests for the standalone detached-Bridge soak harness."""

from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock
from typing import Dict, List, Optional


sys.dont_write_bytecode = True
SCRIPT_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIRECTORY))
import bridge_soak  # noqa: E402


HARNESS = SCRIPT_DIRECTORY / "bridge_soak.py"
FAKE_CONTAINER_RUNTIME = r"""#!/bin/sh
mkdir -p "${FAKE_HOME}"
echo "$*" >> "${FAKE_HOME}/fake-container-commands.log"
if [ "$1" = "version" ]; then
    echo "1.0"
elif [ "$1" = "ps" ] && [ "${FAKE_CONTAINER_LEAK}" = "1" ] \
    && [ -f "${FAKE_HOME}/fake-workload-complete" ]; then
    echo "fake-container-leak"
elif [ "$1" = "network" ] \
    && [ "${FAKE_CONTAINER_NETWORK_LEAK}" = "1" ] \
    && [ -f "${FAKE_HOME}/fake-workload-complete" ]; then
    echo "fake-network-leak"
fi
"""
FAKE_PS = r"""#!/bin/sh
run="${FAKE_HOME}/run"
daemon=""
descendant=""
echo "1 0 Sat Sep 26 00:00:00 2026"
if [ -f "${run}/fake-daemon.pid" ]; then
    daemon="$(cat "${run}/fake-daemon.pid")"
    echo "${daemon} 1 Sat Sep 26 00:00:00 2026"
fi
if [ -f "${run}/fake-descendant.pid" ]; then
    descendant="$(cat "${run}/fake-descendant.pid")"
    echo "${descendant} ${daemon:-1} Sat Sep 26 00:00:00 2026"
fi
"""
FAKE_HARDKNOCK = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import time


def alive(pid):
    if pid <= 1:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def load_json(path, default):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return default


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def records(home, name):
    return load_json(home / ("fake-%s.json" % name), [])


def save_records(home, name, values):
    write_json(home / ("fake-%s.json" % name), values)


def sessions(home):
    return load_json(home / "fake-sessions.json", {})


def save_sessions(home, values):
    write_json(home / "fake-sessions.json", values)


def rotate_logs(logs):
    for generation in range(4, 0, -1):
        source = (
            logs / "bridge.jsonl"
            if generation == 1
            else logs / ("bridge.%d.jsonl" % (generation - 1))
        )
        target = logs / ("bridge.%d.jsonl" % generation)
        if target.exists():
            target.unlink()
        if source.exists():
            source.replace(target)


def report_startup(fd, status, message=None):
    payload = {"status": status}
    if message is not None:
        payload["message"] = message
    try:
        os.write(fd, (json.dumps(payload) + "\n").encode("utf-8"))
    finally:
        os.close(fd)


def daemon(home, startup_fd):
    listener = None
    startup_reported = False
    try:
        if os.environ.get("FAKE_DAEMON_STARTUP_ERROR"):
            raise RuntimeError(os.environ["FAKE_DAEMON_STARTUP_ERROR"])
        return run_daemon(home, startup_fd)
    except BaseException as error:
        try:
            report_startup(
                startup_fd,
                "error",
                "%s: %s" % (type(error).__name__, error),
            )
            startup_reported = True
        except OSError:
            pass
        if not startup_reported:
            try:
                os.close(startup_fd)
            except OSError:
                pass
        if listener is not None:
            listener.close()
        return 2


def run_daemon(home, startup_fd):
    run = home / "run"
    logs = home / "logs"
    run.mkdir(parents=True, exist_ok=True)
    logs.mkdir(parents=True, exist_ok=True)
    socket_path = run / "hardknock.sock"
    if socket_path.exists() or socket_path.is_symlink():
        socket_path.unlink()
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(str(socket_path))
        listener.listen(1)
    except PermissionError:
        socket_target = os.environ.get("FAKE_SOCKET_TARGET")
        if not socket_target:
            raise
        listener.close()
        listener = None
        socket_path.symlink_to(socket_target)

    stopping = [False]

    def stop(_signum, _frame):
        stopping[0] = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    pid = os.getpid()
    (run / "fake-daemon.pid").write_text(str(pid), encoding="utf-8")
    descendant = subprocess.Popen(
        [sys.executable, __file__, "--fake-child"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    (run / "fake-descendant.pid").write_text(
        str(descendant.pid),
        encoding="utf-8",
    )
    (run / "bridge-token").write_text("fake-token", encoding="utf-8")
    write_json(
        run / "bridge-endpoint.json",
        {"transport": "unix", "path": str(socket_path)},
    )
    rotate_logs(logs)
    with (logs / "bridge.jsonl").open("a", encoding="utf-8") as output:
        output.write(
            json.dumps(
                {
                    "event": "bridge_detached_ready",
                    "component": "bridge_launcher",
                    "details": {"child_pid": pid},
                }
            )
            + "\n"
        )
        output.flush()

    report_startup(startup_fd, "ready")

    requested_bytes = int(os.environ.get("FAKE_LOG_BYTES", "0"))
    if requested_bytes:
        with (logs / "bridge.1.jsonl").open("wb") as output:
            output.truncate(requested_bytes)

    while not stopping[0]:
        time.sleep(0.01)
    if listener is not None:
        listener.close()
    if os.environ.get("FAKE_LEAVE_DESCENDANT") != "1":
        descendant.terminate()
        try:
            descendant.wait(timeout=1)
        except subprocess.TimeoutExpired:
            descendant.kill()
            descendant.wait(timeout=1)
        try:
            (run / "fake-descendant.pid").unlink()
        except FileNotFoundError:
            pass
    if os.environ.get("FAKE_LEAVE_RUNTIME") != "1":
        for name in (
            "bridge-endpoint.json",
            "bridge-token",
            "hardknock.sock",
        ):
            path = run / name
            try:
                path.unlink()
            except FileNotFoundError:
                pass
    try:
        (run / "fake-daemon.pid").unlink()
    except FileNotFoundError:
        pass
    return 0


def fake_child():
    while True:
        time.sleep(60)


def spawn_daemon(home):
    read_fd, write_fd = os.pipe()
    try:
        process = subprocess.Popen(
            [
                sys.executable,
                __file__,
                "--fake-daemon",
                str(home),
                str(write_fd),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
            pass_fds=(write_fd,),
        )
    finally:
        os.close(write_fd)

    deadline = time.monotonic() + 1.5
    message = b""
    try:
        while b"\n" not in message:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                process.kill()
                raise RuntimeError("daemon readiness message timed out")
            readable, _, _ = select.select([read_fd], [], [], remaining)
            if not readable:
                process.kill()
                raise RuntimeError("daemon readiness message timed out")
            chunk = os.read(read_fd, 4096)
            if not chunk:
                break
            message += chunk
            if len(message) > 4096:
                process.kill()
                raise RuntimeError("daemon readiness message was too large")
    finally:
        os.close(read_fd)

    if not message:
        process.wait(timeout=1)
        raise RuntimeError(
            "daemon exited before reporting readiness (exit %d)"
            % process.returncode
        )
    try:
        response = json.loads(message.splitlines()[0].decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as error:
        process.kill()
        raise RuntimeError("daemon returned invalid readiness data") from error
    if response.get("status") != "ready":
        process.wait(timeout=1)
        raise RuntimeError(
            response.get("message", "daemon reported startup failure")
        )
    return process.pid


def bridge_call(home, value):
    if os.environ.get("FAKE_DISABLE_BRIDGE_CALL") == "1":
        print("unsupported fake command: bridge call", file=sys.stderr)
        return 2
    all_sessions = sessions(home)
    event = value.get("event")
    data = value.get("data", {})
    if event == "session_started":
        external = data["session_id"]
        session_id = "fake-" + external
        all_sessions[session_id] = {
            "id": session_id,
            "agent": data["agent"]["name"],
            "cwd": data["cwd"],
            "actions": 0,
            "ended": False,
            "runs": {},
        }
        save_sessions(home, all_sessions)
        print(json.dumps({"hardknock_session_id": session_id}))
        return 0

    session_id = data.get("hardknock_session_id")
    session = all_sessions.get(session_id)
    if session is None:
        print("unknown fake session", file=sys.stderr)
        return 2
    if event == "agent_message":
        print(json.dumps({"accepted": True}))
        return 0
    if event == "action_proposed":
        session["actions"] += 1
        session["last_action"] = data["action"]
        save_sessions(home, all_sessions)
        print(json.dumps({"decision": "continue"}))
        return 0
    if event == "action_completed":
        session["last_action_result"] = data["result"]
        save_sessions(home, all_sessions)
        print(json.dumps({"accepted": True}))
        return 0
    if event == "run_completed":
        run_id = data["run_id"]
        suffix = session_id.replace("/", "-")
        experience_id = "experience-" + suffix
        reality_id = "reality-" + suffix
        execution_id = "execution-" + suffix
        transient = home / "artifacts/transient/fake-evaluator"
        transient.mkdir(parents=True, exist_ok=True)
        (transient / "result.tmp").write_text("temporary", encoding="utf-8")
        if os.environ.get("FAKE_LEAVE_TRANSIENT") != "1":
            (transient / "result.tmp").unlink()
            transient.rmdir()
        if os.environ.get("FAKE_LEAVE_REALITY_DIR") == "1":
            (home / "realities/fake-reality-leak").mkdir(
                parents=True,
                exist_ok=True,
            )
        reality_records = records(home, "realities")
        reality_records.append(
            {
                "id": reality_id,
                "status": "observed",
                "root": session["cwd"],
                "ephemeral": False,
                "starting_state": {"repo_path": session["cwd"]},
            }
        )
        if os.environ.get("FAKE_STALE_REALITY_RECORD") == "1":
            reality_records.append(
                {
                    "id": "reality-stale",
                    "status": "running",
                    "root": str(home / "realities/reality-stale"),
                    "ephemeral": True,
                    "starting_state": {"repo_path": session["cwd"]},
                }
            )
        save_records(home, "realities", reality_records)
        execution_records = records(home, "executions")
        execution_records.append(
            {
                "id": execution_id,
                "status": "succeeded",
                "reality_id": reality_id,
            }
        )
        if os.environ.get("FAKE_STALE_EXECUTION_RECORD") == "1":
            execution_records.append(
                {
                    "id": "execution-unexpected",
                    "status": "failed",
                    "reality_id": "reality-stale",
                }
            )
        save_records(home, "executions", execution_records)
        if os.environ.get("FAKE_STALE_EXPERIMENT_RECORD") == "1":
            save_records(
                home,
                "experiments",
                [{"id": "experiment-stale", "status": "running"}],
            )
        if os.environ.get("FAKE_STALE_CURRICULUM_RECORD") == "1":
            save_records(
                home,
                "curricula",
                [{"id": "curriculum-stale", "status": "running"}],
            )
        experience = {
            "id": experience_id,
            "reality_id": reality_id,
            "execution_id": execution_id,
        }
        experiences = load_json(home / "fake-experiences.json", {})
        experiences[experience_id] = experience
        write_json(home / "fake-experiences.json", experiences)
        run = {
            "run_id": run_id,
            "experience_id": experience_id,
            "status": (
                "queued"
                if os.environ.get("FAKE_STALE_RUN") == "1"
                else "completed"
            ),
            "outcome": "pass",
            "error": None,
        }
        session["runs"][run_id] = run
        save_sessions(home, all_sessions)
        (home / "fake-workload-complete").write_text("1", encoding="utf-8")
        print(json.dumps(run))
        return 0
    if event == "session_ended":
        if os.environ.get("FAKE_STALE_SESSION") != "1":
            session["ended"] = True
        save_sessions(home, all_sessions)
        print(json.dumps({"accepted": True}))
        return 0
    print("unsupported fake event: %s" % event, file=sys.stderr)
    return 2


def query_disabled(name):
    disabled = os.environ.get("FAKE_DISABLE_QUERY", "")
    return name in [item.strip() for item in disabled.split(",")]


def cli(argv):
    home = Path(argv[argv.index("--home") + 1]).resolve()
    command_index = argv.index("--home") + 2
    command = argv[command_index]
    command_args = argv[command_index + 1 :]
    run = home / "run"
    pid_path = run / "fake-daemon.pid"

    if command == "bridge" and command_args[0] == "status":
        pid = int(pid_path.read_text()) if pid_path.exists() else 0
        active = sum(
            1
            for session in sessions(home).values()
            if not session.get("ended", False)
        )
        status = (
            "running"
            if alive(pid) and (run / "bridge-endpoint.json").exists()
            else "unavailable"
        )
        print(
            json.dumps(
                {
                    "status": status,
                    "protocol": 1,
                    "persistence_error": None,
                    "sessions": active,
                }
            )
        )
        return 0

    if command == "bridge" and command_args[0] == "start":
        home.mkdir(parents=True, exist_ok=True)
        count_path = home / "fake-start-count"
        count = int(count_path.read_text()) + 1 if count_path.exists() else 1
        count_path.write_text(str(count), encoding="utf-8")
        if os.environ.get("FAKE_FAIL_SECOND_START") == "1" and count >= 2:
            print("configured second start failure", file=sys.stderr)
            return 2
        existing = int(pid_path.read_text()) if pid_path.exists() else 0
        if not alive(existing):
            try:
                spawn_daemon(home)
            except RuntimeError as error:
                print("fake Bridge startup failed: %s" % error, file=sys.stderr)
                return 2
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            if (
                pid_path.exists()
                and (run / "bridge-endpoint.json").exists()
                and (home / "logs/bridge.jsonl").exists()
            ):
                print(json.dumps({"status": "running", "protocol": 1}))
                return 0
            time.sleep(0.01)
        print("fake Bridge startup timeout", file=sys.stderr)
        return 2

    if command == "bridge" and command_args[0] == "stop":
        pid = int(pid_path.read_text()) if pid_path.exists() else 0
        if alive(pid) and os.environ.get("FAKE_IGNORE_STOP") != "1":
            os.kill(pid, signal.SIGTERM)
        deadline = time.monotonic() + 2
        while (
            os.environ.get("FAKE_IGNORE_STOP") != "1"
            and alive(pid)
            and time.monotonic() < deadline
        ):
            time.sleep(0.01)
        print(json.dumps({"status": "stopped"}))
        return 0

    pid = int(pid_path.read_text()) if pid_path.exists() else 0
    if command == "bridge" and not alive(pid):
        print("fake Bridge is unavailable", file=sys.stderr)
        return 2
    if command == "bridge" and command_args[0] == "sessions":
        values = []
        for session in sessions(home).values():
            values.append(
                {
                    key: session[key]
                    for key in ("id", "agent", "cwd", "actions", "ended")
                }
            )
        print(json.dumps({"sessions": values}))
        return 0
    if command == "bridge" and command_args[0] == "inspect":
        session = sessions(home).get(command_args[1])
        if session is None:
            print("unknown fake session", file=sys.stderr)
            return 2
        print(
            json.dumps(
                {
                    "session": {
                        key: session[key]
                        for key in (
                            "id",
                            "agent",
                            "cwd",
                            "actions",
                            "ended",
                        )
                    },
                    "runs": session["runs"],
                    "actions": [],
                }
            )
        )
        return 0
    if command == "bridge" and command_args[0] == "call":
        return bridge_call(home, json.load(sys.stdin))

    if command == "experience" and command_args[0] == "show":
        experience = load_json(home / "fake-experiences.json", {}).get(
            command_args[1]
        )
        if experience is None:
            print("unknown fake experience", file=sys.stderr)
            return 2
        print(json.dumps({"event": "experience", "experience": experience}))
        return 0

    query_name = None
    key = None
    if command == "reality" and command_args == ["list"]:
        query_name, key = "realities", "realities"
    elif command == "execution" and command_args == ["list"]:
        query_name, key = "executions", "executions"
    elif command == "experiment" and command_args == ["list"]:
        query_name, key = "experiments", "experiments"
    elif command == "curriculum" and command_args == ["list"]:
        query_name, key = "curricula", "curricula"
    if query_name is not None:
        if query_disabled(query_name):
            print("configured query unavailable: %s" % query_name, file=sys.stderr)
            return 2
        values = records(home, query_name)
        if query_name == "experiments":
            print(
                json.dumps(
                    {
                        "event": "experimentation",
                        "result": {
                            "kind": "list",
                            key: values,
                            "lesson_experiments": [],
                        },
                    }
                )
            )
        elif query_name == "curricula":
            print(
                json.dumps(
                    {
                        "event": "curriculum",
                        "result": {"kind": "list", key: values},
                    }
                )
            )
        else:
            print(json.dumps({"event": key, key: values}))
        return 0

    print("unsupported fake command", file=sys.stderr)
    return 2


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] == "--fake-child":
        fake_child()
    if len(sys.argv) == 4 and sys.argv[1] == "--fake-daemon":
        sys.exit(daemon(Path(sys.argv[2]).resolve(), int(sys.argv[3])))
    sys.exit(cli(sys.argv[1:]))
"""


class BridgeSoakTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="hardknock-bridge-soak-test-",
            dir="/tmp",
        )
        self.root = Path(self.temporary.name)
        self.fake = self.root / "fake-hardknock"
        self.fake.write_text(
            textwrap.dedent(FAKE_HARDKNOCK),
            encoding="utf-8",
        )
        self.fake.chmod(0o755)
        self.home = self.root / "home"
        self.environment = dict(os.environ)
        for name, content in (
            ("docker", FAKE_CONTAINER_RUNTIME),
            ("podman", FAKE_CONTAINER_RUNTIME),
            ("ps", FAKE_PS),
        ):
            path = self.root / name
            path.write_text(textwrap.dedent(content), encoding="utf-8")
            path.chmod(0o755)
        self.environment["PATH"] = (
            str(self.root) + os.pathsep + self.environment.get("PATH", "")
        )
        self.environment["FAKE_HOME"] = str(self.home)
        socket_target = self.socket_fixture_target()
        if socket_target is not None:
            self.environment["FAKE_SOCKET_TARGET"] = str(socket_target)

    def tearDown(self) -> None:
        self.kill_fake_daemon(self.home)
        self.temporary.cleanup()

    def socket_fixture_target(self) -> Optional[Path]:
        probe_path = self.root / "socket-probe"
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            probe.bind(str(probe_path))
            return None
        except PermissionError:
            for directory in (Path("/var/run"), Path("/private/var/run")):
                try:
                    entries = directory.iterdir()
                    for path in entries:
                        try:
                            if stat.S_ISSOCK(path.stat().st_mode):
                                return path
                        except OSError:
                            continue
                except OSError:
                    continue
            self.skipTest(
                "Unix sockets cannot be created and no socket fixture exists"
            )
            return None
        finally:
            probe.close()
            try:
                probe_path.unlink()
            except FileNotFoundError:
                pass

    @staticmethod
    def fake_pid(home: Path) -> Optional[int]:
        path = home / "run/fake-daemon.pid"
        if not path.exists():
            return None
        try:
            return int(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None

    @classmethod
    def kill_fake_daemon(cls, home: Path) -> None:
        pid = cls.fake_pid(home)
        descendant_path = home / "run/fake-descendant.pid"
        descendant = None
        if descendant_path.exists():
            try:
                descendant = int(descendant_path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                descendant = None
        for process_id in (pid, descendant):
            if process_id is None or not bridge_soak.pid_is_alive(process_id):
                continue
            try:
                os.kill(process_id, signal.SIGKILL)
            except ProcessLookupError:
                continue
            deadline = time.monotonic() + 2
            while (
                bridge_soak.pid_is_alive(process_id)
                and time.monotonic() < deadline
            ):
                time.sleep(0.01)

    def harness_command(
        self,
        *,
        include_home: bool = True,
        duration: float = 0.03,
        shutdown_timeout: float = 0.3,
    ) -> List[str]:
        command = [
            sys.executable,
            str(HARNESS),
            str(self.fake),
        ]
        if include_home:
            command.extend(["--home", str(self.home)])
        command.extend(
            [
                "--duration-seconds",
                str(duration),
                "--poll-interval-seconds",
                "0.01",
                "--command-timeout-seconds",
                "2",
                "--shutdown-timeout-seconds",
                str(shutdown_timeout),
            ]
        )
        return command

    def run_harness(
        self,
        *,
        environment: Optional[Dict[str, str]] = None,
        include_home: bool = True,
        duration: float = 0.03,
        shutdown_timeout: float = 0.3,
    ) -> subprocess.CompletedProcess:
        variables = dict(self.environment)
        if environment:
            variables.update(environment)
        return subprocess.run(
            self.harness_command(
                include_home=include_home,
                duration=duration,
                shutdown_timeout=shutdown_timeout,
            ),
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
            env=variables,
        )

    def assert_runtime_clean(self, home: Path) -> None:
        for name in bridge_soak.RUNTIME_FILE_NAMES:
            self.assertFalse((home / "run" / name).exists(), name)

    def test_default_duration_is_one_day(self) -> None:
        arguments = bridge_soak.build_parser().parse_args([str(self.fake)])
        self.assertEqual(
            arguments.duration_seconds,
            bridge_soak.DEFAULT_DURATION_SECONDS,
        )

    def test_fixture_surfaces_daemon_startup_failure(self) -> None:
        environment = dict(self.environment)
        environment["FAKE_DAEMON_STARTUP_ERROR"] = "fixture startup failed"
        started_at = time.monotonic()
        completed = subprocess.run(
            [
                str(self.fake),
                "--json",
                "--home",
                str(self.home),
                "bridge",
                "start",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=3,
            env=environment,
        )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("fixture startup failed", completed.stderr)
        self.assertLess(time.monotonic() - started_at, 1)

    def test_temporary_home_completes_and_is_removed(self) -> None:
        completed = self.run_harness(include_home=False, duration=0)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result["status"], "passed")
        self.assertTrue(result["temporary_home"])
        self.assertTrue(result["restart_verified"])
        self.assertGreaterEqual(result["status_polls"], 2)
        self.assertFalse(Path(result["home"]).exists())
        self.assertFalse(
            bridge_soak.pid_is_alive(result["first_daemon_pid"])
        )
        self.assertFalse(
            bridge_soak.pid_is_alive(result["second_daemon_pid"])
        )

    def test_explicit_home_is_preserved_and_clean(self) -> None:
        completed = self.run_harness()
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertFalse(result["temporary_home"])
        self.assertEqual(result["home"], str(self.home.resolve()))
        self.assertEqual(
            int((self.home / "fake-start-count").read_text()),
            2,
        )
        self.assert_runtime_clean(self.home)
        self.assertLessEqual(
            result["diagnostic_bytes"],
            bridge_soak.MAX_DIAGNOSTIC_TOTAL_BYTES,
        )
        workload = result["workload"]
        self.assertEqual(workload["status"], "checked")
        self.assertTrue(workload["offline"])
        self.assertTrue(workload["persisted_after_restart"])
        self.assertEqual(
            workload["traffic"],
            [
                "session_started",
                "agent_message",
                "action_proposed",
                "action_completed",
                "run_completed",
                "session_ended",
            ],
        )
        inventory = result["leak_inventory"]
        checks = inventory["checks"]
        self.assertEqual(checks["daemon_descendants"]["status"], "checked")
        self.assertGreaterEqual(checks["daemon_descendants"]["tracked"], 2)
        self.assertEqual(
            checks["managed_reality_worktrees"]["filesystem"]["after_count"],
            0,
        )
        self.assertEqual(
            checks["transient_evaluator_artifacts"]["after_entries"],
            0,
        )
        self.assertEqual(
            checks["database_runtime_records"]["status"],
            "checked",
        )
        self.assertEqual(checks["container_resources"]["status"], "checked")
        self.assertFalse(
            checks["container_resources"]["deletion_performed"]
        )
        phases = [item["phase"] for item in inventory["snapshots"]]
        self.assertEqual(phases[0], "before")
        self.assertIn("during_after_workload", phases)
        self.assertEqual(phases[-1], "after")

    def test_preexisting_bridge_is_rejected_without_stopping_it(self) -> None:
        started = subprocess.run(
            [
                str(self.fake),
                "--json",
                "--home",
                str(self.home),
                "bridge",
                "start",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=3,
            env=self.environment,
        )
        self.assertEqual(started.returncode, 0, started.stderr)
        pid = self.fake_pid(self.home)
        self.assertIsNotNone(pid)

        completed = self.run_harness()
        self.assertEqual(completed.returncode, 1)
        self.assertIn("already running", completed.stderr)
        self.assertTrue(bridge_soak.pid_is_alive(pid))
        self.assertTrue(
            (self.home / "run/bridge-endpoint.json").exists()
        )

    def test_leftover_endpoint_or_socket_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_LEAVE_RUNTIME": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("runtime paths remain", completed.stderr)

    def test_live_daemon_pid_after_stop_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_IGNORE_STOP": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("remains alive", completed.stderr)

    def test_second_start_failure_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_FAIL_SECOND_START": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(
            "second Bridge start could not succeed",
            completed.stderr,
        )
        self.assert_runtime_clean(self.home)

    def test_oversized_diagnostic_logs_fail(self) -> None:
        completed = self.run_harness(
            environment={
                "FAKE_LOG_BYTES": str(
                    bridge_soak.MAX_DIAGNOSTIC_TOTAL_BYTES + 1
                )
            }
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("diagnostic", completed.stderr.lower())
        self.assertIn("exceed", completed.stderr.lower())
        self.assert_runtime_clean(self.home)

    def test_daemon_descendant_after_stop_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_LEAVE_DESCENDANT": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("descendant processes remain alive", completed.stderr)
        descendant_path = self.home / "run/fake-descendant.pid"
        self.assertTrue(descendant_path.exists())
        descendant = int(descendant_path.read_text(encoding="utf-8"))
        self.assertTrue(bridge_soak.pid_is_alive(descendant))

    def test_managed_reality_directory_after_stop_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_LEAVE_REALITY_DIR": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(
            "managed Reality/worktree directories remain changed",
            completed.stderr,
        )
        self.assertTrue(
            (self.home / "realities/fake-reality-leak").is_dir()
        )

    def test_transient_evaluator_artifact_after_stop_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_LEAVE_TRANSIENT": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(
            "transient evaluator artifacts remain changed",
            completed.stderr,
        )
        self.assertTrue(
            (
                self.home
                / "artifacts/transient/fake-evaluator/result.tmp"
            ).is_file()
        )

    def test_stale_bridge_session_after_workload_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_STALE_SESSION": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("left an active session", completed.stderr)

    def test_stale_bridge_run_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_STALE_RUN": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("did not reach a terminal state", completed.stderr)

    def test_new_nonterminal_database_record_fails(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_STALE_REALITY_RECORD": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(
            "new nonterminal realities records remain",
            completed.stderr,
        )

    def test_label_filtered_container_leak_fails_without_deletion(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_CONTAINER_LEAK": "1"}
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(
            "label-filtered Hardknock container resources remain",
            completed.stderr,
        )
        commands = (
            self.home / "fake-container-commands.log"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "label=io.openkedge.hardknock.reality",
            commands,
        )
        self.assertNotIn(" rm ", " " + commands + " ")
        self.assertNotIn(" delete ", " " + commands + " ")

    def test_unavailable_query_is_reported_without_overclaiming(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_DISABLE_QUERY": "curricula"}
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        records = result["leak_inventory"]["checks"][
            "database_runtime_records"
        ]
        self.assertEqual(records["status"], "partial")
        self.assertEqual(records["curricula"]["status"], "unavailable")
        self.assertIn(
            "database_runtime_records",
            result["leak_inventory"]["coverage"]["partial"],
        )
        self.assertFalse(result["leak_inventory"]["coverage_complete"])

    def test_unsupported_bridge_workload_is_reported(self) -> None:
        completed = self.run_harness(
            environment={"FAKE_DISABLE_BRIDGE_CALL": "1"}
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result["workload"]["status"], "unavailable")
        self.assertFalse(result["workload"]["persisted_after_restart"])
        self.assertIn(
            "representative_bridge_workload",
            result["leak_inventory"]["coverage"]["unavailable"],
        )

    def test_inventory_bound_is_enforced_without_following_symlinks(
        self,
    ) -> None:
        root = self.root / "bounded"
        root.mkdir()
        (root / "one").write_text("1", encoding="utf-8")
        (root / "two").write_text("2", encoding="utf-8")
        (root / "link").symlink_to(self.root)
        inventory = bridge_soak.inventory_directory(
            root,
            recursive=True,
            limit=3,
        )
        self.assertEqual(inventory.entries["link"][0], "symlink")
        with self.assertRaisesRegex(
            bridge_soak.BridgeSoakError,
            "2-entry bound",
        ):
            bridge_soak.inventory_directory(
                root,
                recursive=True,
                limit=2,
            )

    def test_database_nonterminal_status_sets_are_enforced(self) -> None:
        empty_records = {
            kind: bridge_soak.RecordProbe(True, {})
            for kind in bridge_soak.RECORD_QUERIES
        }
        empty_set_probe = bridge_soak.SetProbe({}, {})
        empty_paths = bridge_soak.PathInventory({}, 0)
        before = bridge_soak.InventorySnapshot(
            phase="before",
            reality_entries=empty_paths,
            transient_entries=empty_paths,
            records=empty_records,
            managed_worktrees=empty_set_probe,
            containers=empty_set_probe,
        )
        settings = bridge_soak.SoakSettings(
            executable=self.fake,
            home=self.home,
            workspace=Path.cwd(),
        )
        for kind, status in (
            ("realities", "created"),
            ("experiments", "accepted"),
            ("curricula", "running"),
        ):
            with self.subTest(kind=kind):
                records = dict(empty_records)
                records[kind] = bridge_soak.RecordProbe(
                    True,
                    {"new-record": {"id": "new-record", "status": status}},
                )
                after = bridge_soak.InventorySnapshot(
                    phase="after",
                    reality_entries=empty_paths,
                    transient_entries=empty_paths,
                    records=records,
                    managed_worktrees=empty_set_probe,
                    containers=empty_set_probe,
                )
                soak = bridge_soak.BridgeSoak(settings)
                with self.assertRaisesRegex(
                    bridge_soak.BridgeSoakError,
                    "new nonterminal",
                ):
                    soak._record_report(
                        before,
                        after,
                        bridge_soak.WorkloadResult(
                            status="unavailable"
                        ),
                    )

    def test_container_and_network_set_deltas_are_enforced(self) -> None:
        for scope in ("docker:containers", "docker:networks"):
            with self.subTest(scope=scope):
                before = bridge_soak.SetProbe({scope: set()}, {})
                after = bridge_soak.SetProbe(
                    {scope: {"hardknock-resource"}},
                    {},
                )
                with self.assertRaisesRegex(
                    bridge_soak.BridgeSoakError,
                    "remain after Bridge stop",
                ):
                    bridge_soak.BridgeSoak._set_probe_report(
                        before,
                        after,
                        "label-filtered resources",
                    )

    def test_process_and_container_unavailability_are_explicit(self) -> None:
        settings = bridge_soak.SoakSettings(
            executable=self.fake,
            home=self.home,
            workspace=Path.cwd(),
        )
        soak = bridge_soak.BridgeSoak(settings)
        with mock.patch("bridge_soak.shutil.which", return_value=None):
            table, process_reason = soak._process_table()
            containers = soak._container_probe()
        self.assertIsNone(table)
        self.assertIn("unavailable", process_reason)
        self.assertFalse(containers.checked_scopes)
        self.assertIn(
            "container_runtime",
            containers.unavailable_scopes,
        )

    def test_sigint_stops_bridge_and_cleans_runtime(self) -> None:
        process = subprocess.Popen(
            self.harness_command(duration=30, shutdown_timeout=1),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.environment,
        )
        deadline = time.monotonic() + 4
        while not (self.home / "run/bridge-endpoint.json").exists():
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    f"harness exited before interruption: {stdout} {stderr}"
                )
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        pid = self.fake_pid(self.home)
        self.assertIsNotNone(pid)

        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 130, stdout + stderr)
        self.assertIn("interrupted", stderr)
        self.assert_runtime_clean(self.home)
        self.assertFalse(bridge_soak.pid_is_alive(pid))


if __name__ == "__main__":
    unittest.main()
