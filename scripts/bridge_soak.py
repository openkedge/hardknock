#!/usr/bin/env python3
"""Run and verify a bounded Hardknock detached-Bridge soak.

Detached Bridge diagnostics retain ``bridge.jsonl`` plus four archives. Each
file is capped at 1 MiB, so their documented aggregate limit is 5 MiB. The
harness also exercises one offline Bridge lifecycle and reports exactly which
process, filesystem, database, worktree, and container leak probes ran.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import (
    Any,
    Dict,
    Iterator,
    List,
    Mapping,
    Optional,
    Sequence,
    Set,
    Tuple,
)


DEFAULT_DURATION_SECONDS = 86_400.0
DEFAULT_POLL_INTERVAL_SECONDS = 30.0
DEFAULT_COMMAND_TIMEOUT_SECONDS = 30.0
DEFAULT_SHUTDOWN_TIMEOUT_SECONDS = 15.0
MAX_DIAGNOSTIC_FILE_BYTES = 1024 * 1024
MAX_DIAGNOSTIC_FILES = 5
MAX_DIAGNOSTIC_TOTAL_BYTES = (
    MAX_DIAGNOSTIC_FILE_BYTES * MAX_DIAGNOSTIC_FILES
)
MAX_INVENTORY_ENTRIES = 4096
MAX_CLI_RECORDS = 4096
MAX_PROCESS_ROWS = 32768
MAX_EXTERNAL_OUTPUT_BYTES = 4 * 1024 * 1024
MAX_REPOSITORIES = 64
CONTAINER_LABEL = "io.openkedge.hardknock.reality"
DIAGNOSTIC_NAME = re.compile(r"bridge(?:\.([1-9][0-9]*))?\.jsonl")
SAFE_RESOURCE_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}")
RUNTIME_FILE_NAMES = (
    "bridge-endpoint.json",
    "bridge-token",
    "hardknock.sock",
)
RECORD_QUERIES = {
    "realities": (("reality", "list"), ("realities",)),
    "executions": (("execution", "list"), ("executions",)),
    "experiments": (
        ("experiment", "list"),
        ("experiments", "strategy_experiments", "lesson_experiments"),
    ),
    "curricula": (("curriculum", "list"), ("curricula",)),
}
NONTERMINAL_STATUSES = {
    "realities": {"created", "running"},
    "executions": set(),
    "experiments": {"accepted", "running"},
    "curricula": {"running"},
}
TERMINAL_RUN_STATUSES = {"completed", "failed", "interrupted"}


class BridgeSoakError(RuntimeError):
    """A soak invariant failed."""


class BridgeSoakInterrupted(Exception):
    """The harness received an interrupt that still requires Bridge cleanup."""

    def __init__(self, signum: int) -> None:
        super().__init__(f"received {signal.Signals(signum).name}")
        self.signum = signum


@dataclass(frozen=True)
class SoakSettings:
    executable: Path
    home: Path
    workspace: Path
    duration_seconds: float = DEFAULT_DURATION_SECONDS
    poll_interval_seconds: float = DEFAULT_POLL_INTERVAL_SECONDS
    command_timeout_seconds: float = DEFAULT_COMMAND_TIMEOUT_SECONDS
    shutdown_timeout_seconds: float = DEFAULT_SHUTDOWN_TIMEOUT_SECONDS


@dataclass(frozen=True)
class PathInventory:
    entries: Mapping[str, Tuple[str, int]]
    total_file_bytes: int


@dataclass(frozen=True)
class RecordProbe:
    available: bool
    records: Mapping[str, Mapping[str, object]]
    reason: Optional[str] = None


@dataclass(frozen=True)
class SetProbe:
    checked_scopes: Mapping[str, Set[str]]
    unavailable_scopes: Mapping[str, str]


@dataclass(frozen=True)
class InventorySnapshot:
    phase: str
    reality_entries: PathInventory
    transient_entries: PathInventory
    records: Mapping[str, RecordProbe]
    managed_worktrees: SetProbe
    containers: SetProbe


@dataclass(frozen=True)
class WorkloadResult:
    status: str
    reason: Optional[str] = None
    session_id: Optional[str] = None
    run_id: Optional[str] = None
    experience_id: Optional[str] = None
    reality_id: Optional[str] = None
    execution_id: Optional[str] = None
    persisted_after_restart: bool = False


def _short_output(value: str, limit: int = 2048) -> str:
    value = value.strip()
    if len(value) <= limit:
        return value
    return value[:limit] + "...[truncated]"


def _positive_float(value: str) -> float:
    parsed = float(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def _non_negative_float(value: str) -> float:
    parsed = float(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be zero or greater")
    return parsed


def resolve_executable(value: str) -> Path:
    expanded = os.path.expanduser(value)
    has_separator = os.sep in expanded or (
        os.altsep is not None and os.altsep in expanded
    )
    if has_separator:
        executable = Path(expanded).resolve()
    else:
        located = shutil.which(expanded)
        if located is None:
            raise BridgeSoakError(f"hardknock executable not found: {value}")
        executable = Path(located).resolve()
    if not executable.is_file() or not os.access(str(executable), os.X_OK):
        raise BridgeSoakError(
            f"hardknock executable is not an executable file: {executable}"
        )
    return executable


def prepare_home(path: Path) -> Path:
    expanded = path.expanduser()
    if expanded.is_symlink():
        raise BridgeSoakError(
            f"Bridge home must be a real directory: {expanded}"
        )
    home = expanded.resolve()
    if home.exists():
        if not home.is_dir():
            raise BridgeSoakError(
                f"Bridge home must be a real directory: {home}"
            )
    else:
        home.mkdir(parents=True, mode=0o700)
    return home


def _contains_path(parent: Path, child: Path) -> bool:
    try:
        child.relative_to(parent)
        return True
    except ValueError:
        return False


def prepare_workspace(path: Path, home: Path) -> Path:
    expanded = path.expanduser()
    if expanded.is_symlink():
        raise BridgeSoakError(
            f"Bridge workload workspace must be a real directory: {expanded}"
        )
    workspace = expanded.resolve()
    if not workspace.is_dir():
        raise BridgeSoakError(
            f"Bridge workload workspace must be a directory: {workspace}"
        )
    if _contains_path(home, workspace) or _contains_path(workspace, home):
        raise BridgeSoakError(
            "Bridge workload workspace and Hardknock home must be separate "
            "directories with no ancestor relationship"
        )
    return workspace


def _mode_kind(mode: int) -> str:
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISSOCK(mode):
        return "socket"
    return "other"


def inventory_directory(
    root: Path,
    *,
    recursive: bool,
    limit: int = MAX_INVENTORY_ENTRIES,
) -> PathInventory:
    """Inventory without following symlinks and fail closed at a fixed bound."""
    if limit <= 0:
        raise ValueError("inventory limit must be positive")
    try:
        metadata = root.lstat()
    except FileNotFoundError:
        return PathInventory({}, 0)
    except OSError as error:
        raise BridgeSoakError(f"could not inspect inventory root {root}: {error}")
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise BridgeSoakError(f"inventory root is not a real directory: {root}")

    entries: Dict[str, Tuple[str, int]] = {}
    total_file_bytes = 0
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            children = sorted(os.scandir(str(directory)), key=lambda item: item.name)
        except OSError as error:
            raise BridgeSoakError(
                f"could not enumerate inventory directory {directory}: {error}"
            )
        for child in children:
            try:
                child_metadata = child.stat(follow_symlinks=False)
            except OSError as error:
                raise BridgeSoakError(
                    f"could not inspect inventory entry {child.path}: {error}"
                )
            path = Path(child.path)
            relative = path.relative_to(root).as_posix()
            kind = _mode_kind(child_metadata.st_mode)
            size = child_metadata.st_size if kind == "file" else 0
            entries[relative] = (kind, size)
            if len(entries) > limit:
                raise BridgeSoakError(
                    f"inventory for {root} exceeds the {limit}-entry bound"
                )
            total_file_bytes += size
            if recursive and kind == "directory":
                pending.append(path)
    return PathInventory(entries, total_file_bytes)


def pid_is_alive(pid: Optional[int]) -> bool:
    if pid is None or pid <= 1:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


class BridgeSoak:
    def __init__(self, settings: SoakSettings) -> None:
        self.settings = settings
        self._tracked_descendants: Dict[Tuple[int, str], int] = {}
        self._process_method: Optional[str] = None
        self._process_unavailable: Optional[str] = None
        self._process_probe_disabled = False
        self._container_unavailable_cache: Optional[SetProbe] = None
        self._peak_descendants = 0
        self._peak_transient_entries = 0
        self._peak_transient_bytes = 0

    @property
    def run_directory(self) -> Path:
        return self.settings.home / "run"

    @property
    def logs_directory(self) -> Path:
        return self.settings.home / "logs"

    @property
    def realities_directory(self) -> Path:
        return self.settings.home / "realities"

    @property
    def transient_directory(self) -> Path:
        return self.settings.home / "artifacts" / "transient"

    def _json_command(
        self,
        arguments: Sequence[str],
        *,
        input_value: Optional[Mapping[str, object]] = None,
    ) -> Dict[str, object]:
        command = [
            str(self.settings.executable),
            "--json",
            "--home",
            str(self.settings.home),
            *arguments,
        ]
        label = " ".join(arguments)
        try:
            completed = subprocess.run(
                command,
                check=False,
                capture_output=True,
                text=True,
                input=(
                    json.dumps(input_value, sort_keys=True)
                    if input_value is not None
                    else None
                ),
                timeout=self.settings.command_timeout_seconds,
            )
        except subprocess.TimeoutExpired as error:
            raise BridgeSoakError(
                f"{label} exceeded "
                f"{self.settings.command_timeout_seconds:g} seconds"
            ) from error
        except OSError as error:
            raise BridgeSoakError(
                f"could not execute {label}: {error}"
            ) from error

        if completed.returncode != 0:
            diagnostic = _short_output(completed.stderr or completed.stdout)
            raise BridgeSoakError(
                f"{label} exited {completed.returncode}: {diagnostic}"
            )
        try:
            response = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise BridgeSoakError(
                f"{label} returned invalid JSON: "
                f"{_short_output(completed.stdout)}"
            ) from error
        if not isinstance(response, dict):
            raise BridgeSoakError(
                f"{label} returned a non-object JSON response"
            )
        return response

    def _command(self, action: str) -> Dict[str, object]:
        return self._json_command(("bridge", action))

    def _optional_json(
        self,
        arguments: Sequence[str],
    ) -> Tuple[Optional[Dict[str, object]], Optional[str]]:
        try:
            return self._json_command(arguments), None
        except BridgeSoakError as error:
            return None, str(error)

    @staticmethod
    def _lists_for_keys(
        value: object,
        keys: Sequence[str],
        depth: int = 0,
    ) -> List[List[object]]:
        if depth > 4 or not isinstance(value, dict):
            return []
        found = [
            candidate
            for key in keys
            for candidate in [value.get(key)]
            if isinstance(candidate, list)
        ]
        nested = value.get("result")
        if isinstance(nested, dict):
            found.extend(BridgeSoak._lists_for_keys(nested, keys, depth + 1))
        return found

    @staticmethod
    def _object_for_key(
        value: object,
        key: str,
        depth: int = 0,
    ) -> Optional[Mapping[str, object]]:
        if depth > 4 or not isinstance(value, dict):
            return None
        candidate = value.get(key)
        if isinstance(candidate, dict):
            return candidate
        nested = value.get("result")
        if isinstance(nested, dict):
            return BridgeSoak._object_for_key(nested, key, depth + 1)
        return None

    def _record_probe(
        self,
        arguments: Sequence[str],
        keys: Sequence[str],
    ) -> RecordProbe:
        response, reason = self._optional_json(arguments)
        if response is None:
            return RecordProbe(False, {}, reason)
        lists = self._lists_for_keys(response, keys)
        if not lists:
            return RecordProbe(
                False,
                {},
                f"{' '.join(arguments)} JSON did not contain "
                f"any of {', '.join(keys)}",
            )

        records: Dict[str, Mapping[str, object]] = {}
        for values in lists:
            for value in values:
                if not isinstance(value, dict):
                    return RecordProbe(
                        False,
                        {},
                        f"{' '.join(arguments)} returned a non-object record",
                    )
                identifier = value.get("id")
                if not isinstance(identifier, str) or not identifier:
                    return RecordProbe(
                        False,
                        {},
                        f"{' '.join(arguments)} returned a record without an id",
                    )
                previous = records.get(identifier)
                if previous is not None and previous != value:
                    return RecordProbe(
                        False,
                        {},
                        f"{' '.join(arguments)} returned conflicting duplicate "
                        f"record {identifier}",
                    )
                records[identifier] = value
                if len(records) > MAX_CLI_RECORDS:
                    raise BridgeSoakError(
                        f"{' '.join(arguments)} exceeds the "
                        f"{MAX_CLI_RECORDS}-record inventory bound"
                    )
        return RecordProbe(True, records)

    @staticmethod
    def _record_status(record: Mapping[str, object]) -> str:
        status_value = record.get("status")
        return status_value.lower() if isinstance(status_value, str) else "unknown"

    def _process_table(
        self,
    ) -> Tuple[Optional[Mapping[int, Tuple[int, str]]], Optional[str]]:
        if self._process_probe_disabled:
            return None, self._process_unavailable
        executable = shutil.which("ps")
        if executable is None:
            self._process_probe_disabled = True
            return None, "ps is unavailable on this platform"
        try:
            completed = subprocess.run(
                [executable, "-axo", "pid=,ppid=,lstart="],
                check=False,
                capture_output=True,
                text=True,
                timeout=min(self.settings.command_timeout_seconds, 1.0),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            return None, f"ps process inventory failed: {error}"
        if completed.returncode != 0:
            reason = (
                "ps process inventory failed: "
                + _short_output(completed.stderr or completed.stdout)
            )
            self._process_probe_disabled = True
            self._process_unavailable = reason
            return None, reason
        if len(completed.stdout.encode("utf-8", errors="replace")) > (
            MAX_EXTERNAL_OUTPUT_BYTES
        ):
            reason = "ps process inventory exceeded the 4 MiB output bound"
            self._process_probe_disabled = True
            self._process_unavailable = reason
            return None, reason
        lines = completed.stdout.splitlines()
        if len(lines) > MAX_PROCESS_ROWS:
            reason = (
                f"ps process inventory exceeded the {MAX_PROCESS_ROWS}-row bound"
            )
            self._process_probe_disabled = True
            self._process_unavailable = reason
            return None, reason
        table: Dict[int, Tuple[int, str]] = {}
        for line in lines:
            fields = line.split()
            if len(fields) < 7:
                continue
            try:
                pid = int(fields[0])
                parent = int(fields[1])
            except ValueError:
                continue
            if pid <= 0 or parent < 0:
                continue
            started = " ".join(fields[2:7])
            table[pid] = (parent, started)
        if not table:
            reason = "ps returned no parseable process identities"
            self._process_probe_disabled = True
            self._process_unavailable = reason
            return None, reason
        return table, None

    def _record_daemon_descendants(self, daemon_pid: int) -> None:
        table, reason = self._process_table()
        if table is None:
            if self._process_method is None:
                self._process_unavailable = reason
            return
        self._process_method = "ps pid/ppid/lstart"
        self._process_unavailable = None
        children: Dict[int, List[int]] = {}
        for pid, (parent, _) in table.items():
            children.setdefault(parent, []).append(pid)
        pending = list(children.get(daemon_pid, []))
        descendants: Set[int] = set()
        while pending:
            pid = pending.pop()
            if pid in descendants:
                continue
            descendants.add(pid)
            pending.extend(children.get(pid, []))
            if len(descendants) > MAX_PROCESS_ROWS:
                raise BridgeSoakError(
                    "daemon descendant inventory exceeded the process bound"
                )
        for pid in descendants:
            identity = table.get(pid)
            if identity is not None:
                self._tracked_descendants[(pid, identity[1])] = daemon_pid
        self._peak_descendants = max(self._peak_descendants, len(descendants))

    def _verify_descendants_stopped(self) -> None:
        if not self._tracked_descendants:
            return
        table, reason = self._process_table()
        if table is None:
            self._process_unavailable = reason
            return
        remaining = sorted(
            pid
            for pid, started in self._tracked_descendants
            if table.get(pid, (-1, ""))[1] == started
        )
        if remaining:
            raise BridgeSoakError(
                "daemon descendant processes remain alive after Bridge stop: "
                + ", ".join(str(pid) for pid in remaining)
            )

    def _run_probe_command(
        self,
        command: Sequence[str],
    ) -> Tuple[Optional[str], Optional[str]]:
        try:
            completed = subprocess.run(
                list(command),
                check=False,
                capture_output=True,
                text=True,
                timeout=min(self.settings.command_timeout_seconds, 1.0),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            return None, str(error)
        output_bytes = len(completed.stdout.encode("utf-8", errors="replace"))
        error_bytes = len(completed.stderr.encode("utf-8", errors="replace"))
        if output_bytes + error_bytes > MAX_EXTERNAL_OUTPUT_BYTES:
            return None, "command output exceeded the 4 MiB bound"
        if completed.returncode != 0:
            return None, _short_output(completed.stderr or completed.stdout)
        return completed.stdout, None

    def _container_probe(self) -> SetProbe:
        if self._container_unavailable_cache is not None:
            return self._container_unavailable_cache
        checked: Dict[str, Set[str]] = {}
        unavailable: Dict[str, str] = {}
        runtime_seen = False
        for runtime in ("docker", "podman"):
            executable = shutil.which(runtime)
            if executable is None:
                continue
            runtime_seen = True
            _, reason = self._run_probe_command(
                (executable, "version", "--format", "{{.Client.Version}}")
            )
            if reason is not None:
                unavailable[runtime] = f"runtime unavailable: {reason}"
                continue
            commands = {
                "containers": (
                    executable,
                    "ps",
                    "-a",
                    "--filter",
                    f"label={CONTAINER_LABEL}",
                    "--format",
                    "{{.ID}}",
                ),
                "networks": (
                    executable,
                    "network",
                    "ls",
                    "--filter",
                    f"label={CONTAINER_LABEL}",
                    "--format",
                    "{{.ID}}",
                ),
            }
            for resource_kind, command in commands.items():
                output, reason = self._run_probe_command(command)
                scope = f"{runtime}:{resource_kind}"
                if output is None:
                    unavailable[scope] = reason or "query failed"
                    continue
                identifiers: Set[str] = set()
                for line in output.splitlines():
                    identifier = line.strip()
                    if not identifier:
                        continue
                    if SAFE_RESOURCE_ID.fullmatch(identifier) is None:
                        unavailable[scope] = (
                            "runtime returned an unsafe or malformed resource id"
                        )
                        identifiers.clear()
                        break
                    identifiers.add(identifier)
                    if len(identifiers) > MAX_INVENTORY_ENTRIES:
                        raise BridgeSoakError(
                            f"{scope} exceeds the resource inventory bound"
                        )
                if scope not in unavailable:
                    checked[scope] = identifiers
            # Hardknock selects the first responsive runtime in this order.
            # A second runtime cannot own resources created by this soak.
            break
        if not runtime_seen:
            unavailable["container_runtime"] = (
                "docker and podman executables are unavailable"
            )
        elif not checked and not unavailable:
            unavailable["container_runtime"] = (
                "no label-filtered container query was available"
            )
        result = SetProbe(checked, unavailable)
        if not checked:
            self._container_unavailable_cache = result
        return result

    @staticmethod
    def _repository_roots(
        workspace: Path,
        realities: RecordProbe,
    ) -> List[Path]:
        candidates = [workspace]
        if realities.available:
            for record in realities.records.values():
                state = record.get("starting_state")
                if not isinstance(state, dict):
                    continue
                repository = state.get("repo_path")
                if isinstance(repository, str) and repository:
                    candidates.append(Path(repository))
        unique: Dict[str, Path] = {}
        for candidate in candidates:
            try:
                resolved = candidate.expanduser().resolve()
            except OSError:
                continue
            unique[str(resolved)] = resolved
            if len(unique) >= MAX_REPOSITORIES:
                break
        return list(unique.values())

    def _managed_worktree_probe(self, realities: RecordProbe) -> SetProbe:
        git = shutil.which("git")
        if git is None:
            return SetProbe({}, {"git": "git executable is unavailable"})
        checked: Dict[str, Set[str]] = {}
        unavailable: Dict[str, str] = {}
        for repository in self._repository_roots(
            self.settings.workspace,
            realities,
        ):
            scope = str(repository)
            if not repository.is_dir():
                unavailable[scope] = "repository path is unavailable"
                continue
            output, reason = self._run_probe_command(
                (git, "-C", scope, "worktree", "list", "--porcelain")
            )
            if output is None:
                unavailable[scope] = reason or "git worktree query failed"
                continue
            paths: Set[str] = set()
            for line in output.splitlines():
                if not line.startswith("worktree "):
                    continue
                candidate = Path(line[len("worktree ") :]).resolve()
                if _contains_path(self.realities_directory, candidate):
                    paths.add(str(candidate))
                if len(paths) > MAX_INVENTORY_ENTRIES:
                    raise BridgeSoakError(
                        f"managed worktrees for {repository} exceed the bound"
                    )
            checked[scope] = paths
        if not checked and not unavailable:
            unavailable["git"] = "no repository was available for worktree query"
        return SetProbe(checked, unavailable)

    def _observe_transient(self) -> PathInventory:
        inventory = inventory_directory(
            self.transient_directory,
            recursive=True,
        )
        self._peak_transient_entries = max(
            self._peak_transient_entries,
            len(inventory.entries),
        )
        self._peak_transient_bytes = max(
            self._peak_transient_bytes,
            inventory.total_file_bytes,
        )
        return inventory

    def _inventory_snapshot(self, phase: str) -> InventorySnapshot:
        records = {
            kind: self._record_probe(arguments, keys)
            for kind, (arguments, keys) in RECORD_QUERIES.items()
        }
        return InventorySnapshot(
            phase=phase,
            reality_entries=inventory_directory(
                self.realities_directory,
                recursive=False,
            ),
            transient_entries=self._observe_transient(),
            records=records,
            managed_worktrees=self._managed_worktree_probe(records["realities"]),
            containers=self._container_probe(),
        )

    def _diagnostic_paths(self) -> List[Tuple[int, Path]]:
        if not self.logs_directory.exists():
            return []
        paths = []
        for path in self.logs_directory.iterdir():
            match = DIAGNOSTIC_NAME.fullmatch(path.name)
            if match is None:
                continue
            generation = int(match.group(1) or 0)
            paths.append((generation, path))
        return sorted(paths, key=lambda item: item[0])

    def _diagnostic_total(self, require_logs: bool = True) -> int:
        paths = self._diagnostic_paths()
        if require_logs and not paths:
            raise BridgeSoakError("detached Bridge created no diagnostic log")
        if len(paths) > MAX_DIAGNOSTIC_FILES:
            raise BridgeSoakError(
                "Bridge diagnostics retained more than five generations"
            )

        total = 0
        for generation, path in paths:
            if generation >= MAX_DIAGNOSTIC_FILES:
                raise BridgeSoakError(
                    f"unexpected Bridge diagnostic generation: {path.name}"
                )
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(
                metadata.st_mode
            ):
                raise BridgeSoakError(
                    f"Bridge diagnostic is not a regular file: {path}"
                )
            if metadata.st_size > MAX_DIAGNOSTIC_FILE_BYTES:
                raise BridgeSoakError(
                    f"Bridge diagnostic exceeds 1 MiB: "
                    f"{path.name}={metadata.st_size}"
                )
            total += metadata.st_size

        if total > MAX_DIAGNOSTIC_TOTAL_BYTES:
            raise BridgeSoakError(
                "Bridge diagnostics exceed the documented 5 MiB total: "
                f"{total}"
            )
        return total

    def _latest_daemon_pid(self) -> Optional[int]:
        for _, path in self._diagnostic_paths():
            try:
                metadata = path.lstat()
                if (
                    not stat.S_ISREG(metadata.st_mode)
                    or metadata.st_size > MAX_DIAGNOSTIC_FILE_BYTES
                ):
                    continue
                lines = path.read_bytes().splitlines()
            except (FileNotFoundError, OSError):
                continue
            for line in reversed(lines):
                try:
                    record = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                if not isinstance(record, dict):
                    continue
                if record.get("event") != "bridge_detached_ready":
                    continue
                details = record.get("details")
                if not isinstance(details, dict):
                    continue
                pid = details.get("child_pid")
                if (
                    isinstance(pid, int)
                    and not isinstance(pid, bool)
                    and pid > 1
                ):
                    return pid
        return None

    def _wait_for_daemon_pid(self) -> int:
        deadline = time.monotonic() + self.settings.command_timeout_seconds
        while True:
            pid = self._latest_daemon_pid()
            if pid is not None and pid_is_alive(pid):
                return pid
            if time.monotonic() >= deadline:
                raise BridgeSoakError(
                    "detached Bridge did not publish a live daemon PID"
                )
            time.sleep(0.02)

    def _verify_endpoint(self) -> None:
        endpoint_path = self.run_directory / "bridge-endpoint.json"
        token_path = self.run_directory / "bridge-token"
        socket_path = self.run_directory / "hardknock.sock"
        for path in (endpoint_path, token_path, socket_path):
            if not path.exists():
                raise BridgeSoakError(
                    f"running Bridge is missing runtime path: {path}"
                )
        try:
            endpoint = json.loads(endpoint_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise BridgeSoakError(
                f"Bridge endpoint description is invalid: {endpoint_path}"
            ) from error
        if not isinstance(endpoint, dict) or endpoint.get("transport") != "unix":
            raise BridgeSoakError(
                "soak requires the local Unix Bridge endpoint, not TCP"
            )
        described_path = endpoint.get("path")
        if not isinstance(described_path, str):
            raise BridgeSoakError("Unix Bridge endpoint has no socket path")
        if Path(described_path).resolve() != socket_path.resolve():
            raise BridgeSoakError(
                "Unix Bridge endpoint does not identify the dedicated home"
            )
        try:
            mode = socket_path.stat().st_mode
        except OSError as error:
            raise BridgeSoakError(
                f"could not inspect Bridge socket: {socket_path}"
            ) from error
        if not stat.S_ISSOCK(mode):
            raise BridgeSoakError(
                f"Bridge endpoint is not a Unix socket: {socket_path}"
            )

    def _runtime_residue(self) -> List[Path]:
        if not self.run_directory.exists():
            return []
        residue = [
            self.run_directory / name
            for name in RUNTIME_FILE_NAMES
            if (
                (self.run_directory / name).exists()
                or (self.run_directory / name).is_symlink()
            )
        ]
        for path in self.run_directory.rglob("*.sock"):
            if path not in residue:
                residue.append(path)
        return sorted(residue)

    def _status(self) -> Dict[str, object]:
        response = self._command("status")
        if response.get("status") != "running":
            raise BridgeSoakError(
                f"Bridge status is not running: {response!r}"
            )
        persistence_error = response.get("persistence_error")
        if persistence_error not in (None, ""):
            raise BridgeSoakError(
                f"Bridge reported a persistence error: {persistence_error}"
            )
        return response

    def _session_probe(self) -> RecordProbe:
        response, reason = self._optional_json(("bridge", "sessions"))
        if response is None:
            return RecordProbe(False, {}, reason)
        sessions = response.get("sessions")
        if not isinstance(sessions, list):
            return RecordProbe(
                False,
                {},
                "bridge sessions JSON did not contain a sessions list",
            )
        if len(sessions) > MAX_CLI_RECORDS:
            raise BridgeSoakError(
                "bridge sessions exceeds the session inventory bound"
            )
        records: Dict[str, Mapping[str, object]] = {}
        for session in sessions:
            if not isinstance(session, dict):
                return RecordProbe(
                    False,
                    {},
                    "bridge sessions returned a non-object session",
                )
            identifier = session.get("id")
            if not isinstance(identifier, str) or not identifier:
                return RecordProbe(
                    False,
                    {},
                    "bridge sessions returned a session without an id",
                )
            records[identifier] = session
        return RecordProbe(True, records)

    def _assert_no_stale_runtime_sessions(self) -> RecordProbe:
        status = self._status()
        active_count = status.get("sessions")
        if isinstance(active_count, int) and not isinstance(active_count, bool):
            if active_count != 0:
                raise BridgeSoakError(
                    "Bridge loaded active sessions before the soak workload: "
                    f"{active_count}"
                )
        probe = self._session_probe()
        if probe.available:
            active = sorted(
                identifier
                for identifier, session in probe.records.items()
                if session.get("ended") is not True
            )
            if active:
                raise BridgeSoakError(
                    "Bridge loaded stale non-ended session records before the "
                    "soak workload: " + ", ".join(active)
                )
        return probe

    @staticmethod
    def _looks_unsupported(error: BridgeSoakError) -> bool:
        message = str(error).lower()
        return any(
            marker in message
            for marker in (
                "unsupported",
                "unrecognized subcommand",
                "unknown command",
                "unexpected argument 'call'",
            )
        )

    def _bridge_call(
        self,
        event: Mapping[str, object],
    ) -> Dict[str, object]:
        return self._json_command(
            ("bridge", "call"),
            input_value=event,
        )

    def _inspect_session(self, session_id: str) -> Dict[str, object]:
        return self._json_command(("bridge", "inspect", session_id))

    def _wait_for_run(
        self,
        daemon_pid: int,
        session_id: str,
        run_id: str,
    ) -> Mapping[str, object]:
        deadline = time.monotonic() + self.settings.command_timeout_seconds
        while True:
            response = self._inspect_session(session_id)
            runs = response.get("runs")
            if not isinstance(runs, dict):
                raise BridgeSoakError(
                    "bridge inspect did not return a runs object"
                )
            run = runs.get(run_id)
            if not isinstance(run, dict):
                raise BridgeSoakError(
                    f"bridge inspect did not return run {run_id}"
                )
            status = run.get("status")
            if isinstance(status, str) and status in TERMINAL_RUN_STATUSES:
                return run
            if time.monotonic() >= deadline:
                raise BridgeSoakError(
                    f"Bridge workload run {run_id} did not reach a terminal "
                    "state before the command timeout"
                )
            self._record_daemon_descendants(daemon_pid)
            self._observe_transient()
            time.sleep(0.02)

    def _workload_experience(
        self,
        experience_id: str,
    ) -> Mapping[str, object]:
        response = self._json_command(("experience", "show", experience_id))
        experience = self._object_for_key(response, "experience")
        if experience is None:
            raise BridgeSoakError(
                "experience show did not return the completed workload "
                "experience"
            )
        return experience

    def _exercise_workload(
        self,
        daemon_pid: int,
        baseline_sessions: RecordProbe,
    ) -> WorkloadResult:
        external_session = (
            f"bridge-soak-{os.getpid()}-{time.monotonic_ns()}"
        )
        start_event: Dict[str, object] = {
            "event": "session_started",
            "data": {
                "session_id": external_session,
                "agent": {
                    "name": "hardknock-soak",
                    "version": "1",
                    "model": "offline-lifecycle",
                    "adapter_version": "bridge-soak-v1",
                },
                "cwd": str(self.settings.workspace),
                "repository": None,
                "task": "Exercise the bounded offline Bridge lifecycle",
                "environment": {
                    "os": platform.system().lower() or None,
                    "arch": platform.machine() or None,
                    "versions": {"bridge_soak": "1"},
                },
            },
        }
        try:
            start_response = self._bridge_call(start_event)
        except BridgeSoakError as error:
            if self._looks_unsupported(error):
                return WorkloadResult(
                    status="unavailable",
                    reason=(
                        "Bridge lifecycle calls are unavailable: "
                        + str(error)
                    ),
                )
            raise
        session_id = start_response.get("hardknock_session_id")
        if not isinstance(session_id, str) or not session_id:
            raise BridgeSoakError(
                "session_started did not return a Hardknock session id"
            )
        if (
            baseline_sessions.available
            and session_id in baseline_sessions.records
        ):
            raise BridgeSoakError(
                f"Bridge workload reused preexisting session {session_id}"
            )

        action_id = "bridge-soak-action"
        action = {
            "type": "custom",
            "kind": "bridge_soak",
            "payload": {"offline": True, "state_change": False},
        }
        self._bridge_call(
            {
                "event": "agent_message",
                "data": {
                    "hardknock_session_id": session_id,
                    "summary": "Bridge soak lifecycle traffic",
                },
            }
        )
        self._bridge_call(
            {
                "event": "action_proposed",
                "data": {
                    "hardknock_session_id": session_id,
                    "action_id": action_id,
                    "action": action,
                    "context": {
                        "no_state_change": True,
                        "config_changed": False,
                        "can_intercept": False,
                    },
                },
            }
        )
        self._bridge_call(
            {
                "event": "action_completed",
                "data": {
                    "hardknock_session_id": session_id,
                    "action_id": action_id,
                    "action": action,
                    "result": {
                        "success": True,
                        "exit_code": 0,
                        "output_summary": "offline lifecycle probe completed",
                        "artifacts": [],
                    },
                    "duration_ms": 1,
                },
            }
        )

        run_id = "bridge-soak-run"
        queued = self._bridge_call(
            {
                "event": "run_completed",
                "data": {
                    "hardknock_session_id": session_id,
                    "run_id": run_id,
                    "success": True,
                    "final_message": "bounded soak probe complete",
                    "duration_ms": 1,
                    "termination": "completed",
                    "external_metadata": {
                        "source": "bridge_soak",
                        "offline": True,
                    },
                },
            }
        )
        experience_id = queued.get("experience_id")
        if not isinstance(experience_id, str) or not experience_id:
            raise BridgeSoakError(
                "run_completed did not return an experience id"
            )
        run = self._wait_for_run(
            daemon_pid,
            session_id,
            run_id,
        )
        if run.get("status") != "completed":
            raise BridgeSoakError(
                "Bridge workload recording did not complete successfully: "
                f"{run!r}"
            )
        experience = self._workload_experience(experience_id)
        reality_id = experience.get("reality_id")
        execution_id = experience.get("execution_id")
        if not isinstance(reality_id, str) or not isinstance(
            execution_id,
            str,
        ):
            raise BridgeSoakError(
                "completed workload experience did not identify its Reality "
                "and execution records"
            )

        self._bridge_call(
            {
                "event": "session_ended",
                "data": {"hardknock_session_id": session_id},
            }
        )
        status = self._status()
        active_count = status.get("sessions")
        if isinstance(active_count, int) and active_count != 0:
            raise BridgeSoakError(
                "Bridge workload left an active session after session_ended"
            )
        sessions = self._session_probe()
        if sessions.available:
            session = sessions.records.get(session_id)
            if session is None or session.get("ended") is not True:
                raise BridgeSoakError(
                    "Bridge workload session was not recorded as ended"
                )
        return WorkloadResult(
            status="checked",
            session_id=session_id,
            run_id=run_id,
            experience_id=experience_id,
            reality_id=reality_id,
            execution_id=execution_id,
        )

    def _verify_workload_after_restart(
        self,
        workload: WorkloadResult,
    ) -> WorkloadResult:
        if workload.status != "checked":
            return workload
        assert workload.session_id is not None
        assert workload.run_id is not None
        sessions = self._session_probe()
        if not sessions.available:
            raise BridgeSoakError(
                "could not verify persisted Bridge session after restart: "
                + (sessions.reason or "session query unavailable")
            )
        session = sessions.records.get(workload.session_id)
        if session is None or session.get("ended") is not True:
            raise BridgeSoakError(
                "ended workload session was not persisted across Bridge restart"
            )
        inspected = self._inspect_session(workload.session_id)
        runs = inspected.get("runs")
        run = runs.get(workload.run_id) if isinstance(runs, dict) else None
        if not isinstance(run, dict) or run.get("status") != "completed":
            raise BridgeSoakError(
                "completed workload run was not persisted across Bridge restart"
            )
        status = self._status()
        if status.get("sessions") not in (None, 0):
            raise BridgeSoakError(
                "Bridge restart reactivated an ended workload session"
            )
        return WorkloadResult(
            status=workload.status,
            reason=workload.reason,
            session_id=workload.session_id,
            run_id=workload.run_id,
            experience_id=workload.experience_id,
            reality_id=workload.reality_id,
            execution_id=workload.execution_id,
            persisted_after_restart=True,
        )

    def _assert_not_running(self) -> None:
        response = self._command("status")
        if response.get("status") == "running":
            raise BridgeSoakError(
                "a Bridge is already running in the selected home"
            )

    def _start(self) -> int:
        response = self._command("start")
        if response.get("status") != "running":
            raise BridgeSoakError(
                f"bridge start did not report running: {response!r}"
            )
        pid = self._wait_for_daemon_pid()
        self._status()
        self._verify_endpoint()
        self._diagnostic_total()
        self._record_daemon_descendants(pid)
        return pid

    def _start_again(self) -> int:
        deadline = time.monotonic() + self.settings.shutdown_timeout_seconds
        last_error = None
        while True:
            try:
                return self._start()
            except BridgeSoakError as error:
                last_error = error
                pid = self._latest_daemon_pid()
                if self._runtime_residue() or pid_is_alive(pid):
                    raise
                if time.monotonic() >= deadline:
                    raise BridgeSoakError(
                        f"second Bridge start could not succeed: {last_error}"
                    ) from last_error
                time.sleep(0.05)

    def _stop_and_verify(
        self,
        pid: Optional[int],
        validate_diagnostics: bool = True,
    ) -> None:
        stop_error = None
        if pid is not None and pid_is_alive(pid):
            self._record_daemon_descendants(pid)
        if self._runtime_residue() or pid_is_alive(pid):
            try:
                response = self._command("stop")
                if response.get("status") != "stopped":
                    stop_error = BridgeSoakError(
                        f"bridge stop did not report stopped: {response!r}"
                    )
            except BridgeSoakError as error:
                stop_error = error

        deadline = time.monotonic() + self.settings.shutdown_timeout_seconds
        while True:
            residue = self._runtime_residue()
            alive = pid_is_alive(pid)
            if not residue and not alive:
                break
            if time.monotonic() >= deadline:
                problems = []
                if residue:
                    problems.append(
                        "runtime paths remain: "
                        + ", ".join(str(path) for path in residue)
                    )
                if alive:
                    problems.append(f"daemon PID {pid} remains alive")
                raise BridgeSoakError("; ".join(problems))
            time.sleep(0.02)

        self._verify_descendants_stopped()
        if validate_diagnostics:
            self._diagnostic_total()
        if stop_error is not None:
            raise stop_error

    @staticmethod
    def _set_probe_report(
        before: SetProbe,
        after: SetProbe,
        label: str,
    ) -> Dict[str, object]:
        before_scopes = set(before.checked_scopes)
        after_scopes = set(after.checked_scopes)
        comparable = sorted(before_scopes & after_scopes)
        introduced: Dict[str, List[str]] = {}
        removed: Dict[str, List[str]] = {}
        for scope in comparable:
            additions = sorted(
                after.checked_scopes[scope] - before.checked_scopes[scope]
            )
            deletions = sorted(
                before.checked_scopes[scope] - after.checked_scopes[scope]
            )
            if additions:
                introduced[scope] = additions
            if deletions:
                removed[scope] = deletions
        if introduced:
            details = "; ".join(
                f"{scope}: {', '.join(values)}"
                for scope, values in introduced.items()
            )
            raise BridgeSoakError(f"{label} remain after Bridge stop: {details}")

        unavailable = dict(before.unavailable_scopes)
        unavailable.update(after.unavailable_scopes)
        missing_baseline = sorted(after_scopes - before_scopes)
        missing_after = sorted(before_scopes - after_scopes)
        if missing_baseline:
            unavailable["missing_baseline"] = ", ".join(missing_baseline)
        if missing_after:
            unavailable["missing_after"] = ", ".join(missing_after)
        if comparable and not unavailable:
            status = "checked"
        elif comparable:
            status = "partial"
        else:
            status = "unavailable"
        return {
            "status": status,
            "checked_scopes": comparable,
            "baseline_count": sum(
                len(before.checked_scopes[scope]) for scope in comparable
            ),
            "after_count": sum(
                len(after.checked_scopes[scope]) for scope in comparable
            ),
            "introduced": introduced,
            "removed": removed,
            "unavailable": unavailable,
        }

    @staticmethod
    def _path_changes(
        before: PathInventory,
        after: PathInventory,
    ) -> Tuple[List[str], List[str]]:
        introduced = sorted(set(after.entries) - set(before.entries))
        changed = sorted(
            path
            for path in set(before.entries) & set(after.entries)
            if before.entries[path] != after.entries[path]
        )
        return introduced, changed

    def _record_report(
        self,
        before: InventorySnapshot,
        after: InventorySnapshot,
        workload: WorkloadResult,
    ) -> Dict[str, object]:
        expected = {
            "realities": {workload.reality_id}
            if workload.reality_id is not None
            else set(),
            "executions": {workload.execution_id}
            if workload.execution_id is not None
            else set(),
            "experiments": set(),
            "curricula": set(),
        }
        report: Dict[str, object] = {}
        all_checked = True
        any_checked = False
        for kind in RECORD_QUERIES:
            baseline = before.records[kind]
            final = after.records[kind]
            if not baseline.available or not final.available:
                all_checked = False
                report[kind] = {
                    "status": "unavailable",
                    "reason": baseline.reason or final.reason,
                }
                continue
            any_checked = True
            introduced_ids = set(final.records) - set(baseline.records)
            expected_ids = {
                identifier
                for identifier in expected[kind]
                if identifier is not None
            }
            unexpected = sorted(introduced_ids - expected_ids)
            missing = sorted(expected_ids - introduced_ids)
            nonterminal = sorted(
                identifier
                for identifier in introduced_ids
                if self._record_status(final.records[identifier])
                in NONTERMINAL_STATUSES[kind]
            )
            if nonterminal:
                raise BridgeSoakError(
                    f"new nonterminal {kind} records remain after Bridge stop: "
                    + ", ".join(nonterminal)
                )
            if missing:
                raise BridgeSoakError(
                    f"expected workload {kind} records are missing: "
                    + ", ".join(missing)
                )
            if unexpected:
                raise BridgeSoakError(
                    f"unexpected {kind} records were created during the soak: "
                    + ", ".join(unexpected)
                )
            expected_status = {
                "realities": "observed",
                "executions": "succeeded",
            }.get(kind)
            if expected_status is not None:
                wrong = sorted(
                    identifier
                    for identifier in expected_ids
                    if self._record_status(final.records[identifier])
                    != expected_status
                )
                if wrong:
                    raise BridgeSoakError(
                        f"workload {kind} records did not reach "
                        f"{expected_status}: " + ", ".join(wrong)
                    )
            report[kind] = {
                "status": "checked",
                "baseline_count": len(baseline.records),
                "after_count": len(final.records),
                "introduced": sorted(introduced_ids),
                "introduced_nonterminal": nonterminal,
                "expected_durable": sorted(expected_ids),
            }

        baseline_sessions = getattr(self, "_baseline_sessions", None)
        restart_sessions = getattr(self, "_restart_sessions", None)
        if (
            isinstance(baseline_sessions, RecordProbe)
            and isinstance(restart_sessions, RecordProbe)
            and baseline_sessions.available
            and restart_sessions.available
        ):
            introduced_sessions = sorted(
                set(restart_sessions.records) - set(baseline_sessions.records)
            )
            expected_session = (
                {workload.session_id}
                if workload.session_id is not None
                else set()
            )
            unexpected_sessions = sorted(
                set(introduced_sessions) - expected_session
            )
            if unexpected_sessions:
                raise BridgeSoakError(
                    "unexpected Bridge session records were created: "
                    + ", ".join(unexpected_sessions)
                )
            report["bridge_sessions"] = {
                "status": "checked",
                "baseline_count": len(baseline_sessions.records),
                "after_restart_count": len(restart_sessions.records),
                "introduced": introduced_sessions,
                "active_after_restart": [],
            }
        else:
            all_checked = False
            report["bridge_sessions"] = {
                "status": "unavailable",
                "reason": (
                    getattr(baseline_sessions, "reason", None)
                    or getattr(restart_sessions, "reason", None)
                    or "Bridge session inventory unavailable"
                ),
            }
        report["status"] = (
            "checked"
            if all_checked
            else ("partial" if any_checked else "unavailable")
        )
        return report

    def _inventory_report(
        self,
        before: InventorySnapshot,
        during: Sequence[InventorySnapshot],
        after: InventorySnapshot,
        workload: WorkloadResult,
    ) -> Dict[str, object]:
        reality_introduced, reality_changed = self._path_changes(
            before.reality_entries,
            after.reality_entries,
        )
        if reality_introduced or reality_changed:
            paths = reality_introduced + reality_changed
            raise BridgeSoakError(
                "managed Reality/worktree directories remain changed after "
                "Bridge stop: " + ", ".join(paths)
            )
        transient_introduced, transient_changed = self._path_changes(
            before.transient_entries,
            after.transient_entries,
        )
        if transient_introduced or transient_changed:
            paths = transient_introduced + transient_changed
            raise BridgeSoakError(
                "transient evaluator artifacts remain changed after Bridge "
                "stop: " + ", ".join(paths)
            )

        git_worktrees = self._set_probe_report(
            before.managed_worktrees,
            after.managed_worktrees,
            "managed Git worktrees",
        )
        managed_status = (
            "checked"
            if git_worktrees["status"] == "checked"
            else "partial"
        )
        containers = self._set_probe_report(
            before.containers,
            after.containers,
            "label-filtered Hardknock container resources",
        )
        containers["query"] = (
            f"read-only runtime filters for label {CONTAINER_LABEL}"
        )
        containers["deletion_performed"] = False

        process_status = (
            "unavailable"
            if self._process_method is None
            else ("partial" if self._process_unavailable else "checked")
        )
        process_report = {
            "status": process_status,
            "method": self._process_method,
            "reason": self._process_unavailable,
            "tracked": len(self._tracked_descendants),
            "peak_per_snapshot": self._peak_descendants,
            "remaining": [],
        }
        records = self._record_report(before, after, workload)

        checks: Dict[str, Dict[str, object]] = {
            "daemon_descendants": process_report,
            "managed_reality_worktrees": {
                "status": managed_status,
                "filesystem": {
                    "status": "checked",
                    "root": str(self.realities_directory),
                    "baseline_count": len(before.reality_entries.entries),
                    "peak_count": max(
                        [len(before.reality_entries.entries)]
                        + [
                            len(snapshot.reality_entries.entries)
                            for snapshot in during
                        ]
                        + [len(after.reality_entries.entries)]
                    ),
                    "after_count": len(after.reality_entries.entries),
                    "introduced": reality_introduced,
                    "changed": reality_changed,
                },
                "git_worktrees": git_worktrees,
            },
            "transient_evaluator_artifacts": {
                "status": "checked",
                "root": str(self.transient_directory),
                "baseline_entries": len(before.transient_entries.entries),
                "peak_entries": self._peak_transient_entries,
                "after_entries": len(after.transient_entries.entries),
                "peak_file_bytes": self._peak_transient_bytes,
                "after_file_bytes": after.transient_entries.total_file_bytes,
                "introduced": transient_introduced,
                "changed": transient_changed,
            },
            "database_runtime_records": records,
            "container_resources": containers,
            "runtime_paths": {
                "status": "checked",
                "checked": list(RUNTIME_FILE_NAMES) + ["run/**/*.sock"],
                "remaining": [],
            },
            "diagnostics": {
                "status": "checked",
                "retained_files_limit": MAX_DIAGNOSTIC_FILES,
                "retained_bytes_limit": MAX_DIAGNOSTIC_TOTAL_BYTES,
            },
        }
        coverage = {"checked": [], "partial": [], "unavailable": []}
        for name, check in checks.items():
            status_value = check.get("status")
            if status_value in coverage:
                coverage[status_value].append(name)
        for values in coverage.values():
            values.sort()
        return {
            "checks": checks,
            "coverage": coverage,
            "coverage_complete": not (
                coverage["partial"] or coverage["unavailable"]
            ),
            "snapshots": [
                {
                    "phase": snapshot.phase,
                    "reality_entries": len(
                        snapshot.reality_entries.entries
                    ),
                    "transient_entries": len(
                        snapshot.transient_entries.entries
                    ),
                    "record_queries_available": sorted(
                        kind
                        for kind, probe in snapshot.records.items()
                        if probe.available
                    ),
                    "container_scopes_available": sorted(
                        snapshot.containers.checked_scopes
                    ),
                }
                for snapshot in [before, *during, after]
            ],
        }

    def _cleanup_after_failure(self, pid: Optional[int]) -> Optional[str]:
        tracked_pid = pid if pid is not None else self._latest_daemon_pid()
        if not self._runtime_residue() and not pid_is_alive(tracked_pid):
            return None
        try:
            self._stop_and_verify(
                tracked_pid,
                validate_diagnostics=False,
            )
        except BridgeSoakError as error:
            return str(error)
        return None

    def _poll_for_duration(self, daemon_pid: int) -> int:
        deadline = time.monotonic() + self.settings.duration_seconds
        polls = 0
        while True:
            self._status()
            self._verify_endpoint()
            self._diagnostic_total()
            self._record_daemon_descendants(daemon_pid)
            self._observe_transient()
            polls += 1
            now = time.monotonic()
            if now >= deadline:
                return polls
            time.sleep(
                min(self.settings.poll_interval_seconds, deadline - now)
            )

    def run(self) -> Dict[str, object]:
        active_pid = None
        started_at = time.monotonic()
        self._assert_not_running()
        try:
            before = self._inventory_snapshot("before")

            first_pid = self._start()
            active_pid = first_pid
            self._baseline_sessions = self._assert_no_stale_runtime_sessions()
            workload = self._exercise_workload(
                first_pid,
                self._baseline_sessions,
            )
            during_after_workload = self._inventory_snapshot(
                "during_after_workload"
            )
            polls = self._poll_for_duration(first_pid)
            during_before_stop = self._inventory_snapshot(
                "during_before_first_stop"
            )
            self._stop_and_verify(first_pid)
            active_pid = None
            after_first_stop = self._inventory_snapshot("after_first_stop")
            self._inventory_report(
                before,
                [during_after_workload, during_before_stop],
                after_first_stop,
                workload,
            )

            second_pid = self._start_again()
            active_pid = second_pid
            self._restart_sessions = self._assert_no_stale_runtime_sessions()
            workload = self._verify_workload_after_restart(workload)
            self._verify_endpoint()
            during_restart = self._inventory_snapshot("during_restart")
            self._stop_and_verify(second_pid)
            active_pid = None
            after = self._inventory_snapshot("after")
            inventory = self._inventory_report(
                before,
                [
                    during_after_workload,
                    during_before_stop,
                    after_first_stop,
                    during_restart,
                ],
                after,
                workload,
            )
            workload_report = {
                "status": workload.status,
                "reason": workload.reason,
                "offline": True,
                "workspace": str(self.settings.workspace),
                "traffic": [
                    "session_started",
                    "agent_message",
                    "action_proposed",
                    "action_completed",
                    "run_completed",
                    "session_ended",
                ]
                if workload.status == "checked"
                else [],
                "session_id": workload.session_id,
                "run_id": workload.run_id,
                "experience_id": workload.experience_id,
                "reality_id": workload.reality_id,
                "execution_id": workload.execution_id,
                "persisted_after_restart": (
                    workload.persisted_after_restart
                ),
            }
            workload_coverage = (
                workload.status
                if workload.status in inventory["coverage"]
                else "unavailable"
            )
            inventory["coverage"][workload_coverage].append(
                "representative_bridge_workload"
            )
            inventory["coverage"][workload_coverage].sort()
            inventory["coverage_complete"] = not (
                inventory["coverage"]["partial"]
                or inventory["coverage"]["unavailable"]
            )

            return {
                "status": "passed",
                "home": str(self.settings.home),
                "workspace": str(self.settings.workspace),
                "duration_seconds": self.settings.duration_seconds,
                "elapsed_seconds": round(time.monotonic() - started_at, 3),
                "status_polls": polls + 1,
                "first_daemon_pid": first_pid,
                "second_daemon_pid": second_pid,
                "restart_verified": True,
                "diagnostic_bytes": self._diagnostic_total(),
                "diagnostic_limit_bytes": MAX_DIAGNOSTIC_TOTAL_BYTES,
                "workload": workload_report,
                "leak_inventory": inventory,
            }
        except BaseException as error:
            cleanup_error = self._cleanup_after_failure(active_pid)
            if cleanup_error is not None:
                raise BridgeSoakError(
                    f"{error}; cleanup also failed: {cleanup_error}"
                ) from error
            raise


@contextmanager
def interruption_handlers() -> Iterator[None]:
    previous = {}

    def interrupt(signum: int, _frame: object) -> None:
        raise BridgeSoakInterrupted(signum)

    for signum in (signal.SIGINT, signal.SIGTERM):
        previous[signum] = signal.getsignal(signum)
        signal.signal(signum, interrupt)
    try:
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Soak a detached Hardknock Bridge, verify Unix runtime cleanup, "
            "restartability, daemon/descendant exit, managed Reality and "
            "transient cleanup, queryable record state, label-filtered "
            "container resources, and the 5 MiB diagnostic bound."
        )
    )
    parser.add_argument(
        "hardknock",
        help="Hardknock executable path or command name",
    )
    parser.add_argument(
        "--home",
        type=Path,
        help="Dedicated Hardknock home; otherwise a temporary home is used",
    )
    parser.add_argument(
        "--workspace",
        type=Path,
        default=Path.cwd(),
        help=(
            "Stable workspace used for offline Bridge lifecycle traffic "
            "(default: current directory)"
        ),
    )
    parser.add_argument(
        "--duration-seconds",
        type=_non_negative_float,
        default=DEFAULT_DURATION_SECONDS,
        help="Status-polling duration (default: 86400)",
    )
    parser.add_argument(
        "--poll-interval-seconds",
        type=_positive_float,
        default=DEFAULT_POLL_INTERVAL_SECONDS,
        help="Delay between status polls (default: 30)",
    )
    parser.add_argument(
        "--command-timeout-seconds",
        type=_positive_float,
        default=DEFAULT_COMMAND_TIMEOUT_SECONDS,
        help="Timeout for each Hardknock CLI call (default: 30)",
    )
    parser.add_argument(
        "--shutdown-timeout-seconds",
        type=_positive_float,
        default=DEFAULT_SHUTDOWN_TIMEOUT_SECONDS,
        help="Time allowed for shutdown cleanup or restart (default: 15)",
    )
    return parser


def _run_with_home(
    arguments: argparse.Namespace,
    executable: Path,
    home: Path,
    temporary_home: bool,
) -> Dict[str, object]:
    prepared_home = prepare_home(home)
    settings = SoakSettings(
        executable=executable,
        home=prepared_home,
        workspace=prepare_workspace(arguments.workspace, prepared_home),
        duration_seconds=arguments.duration_seconds,
        poll_interval_seconds=arguments.poll_interval_seconds,
        command_timeout_seconds=arguments.command_timeout_seconds,
        shutdown_timeout_seconds=arguments.shutdown_timeout_seconds,
    )
    with interruption_handlers():
        result = BridgeSoak(settings).run()
    result["temporary_home"] = temporary_home
    return result


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    try:
        executable = resolve_executable(arguments.hardknock)
        if arguments.home is not None:
            result = _run_with_home(
                arguments,
                executable,
                arguments.home,
                temporary_home=False,
            )
        else:
            with tempfile.TemporaryDirectory(
                prefix="hardknock-bridge-soak-",
                dir="/tmp",
            ) as temporary:
                result = _run_with_home(
                    arguments,
                    executable,
                    Path(temporary),
                    temporary_home=True,
                )
        print(json.dumps(result, sort_keys=True))
        return 0
    except BridgeSoakInterrupted as error:
        print(f"bridge soak interrupted: {error}", file=sys.stderr)
        return 128 + error.signum
    except (BridgeSoakError, OSError, ValueError) as error:
        print(f"bridge soak failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
