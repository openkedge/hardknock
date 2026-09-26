#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Fail closed unless typed, content-bound evidence permits stable promotion."""

from __future__ import annotations

import argparse
import calendar
import hashlib
import json
import os
import re
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any

MAX_RECORD_BYTES = 1024 * 1024
MAX_EVIDENCE_BYTES = 16 * 1024 * 1024
MAX_ASSET_BYTES = 512 * 1024 * 1024
MAX_SOURCE_DATE_EPOCH = 253402300799
NANOSECONDS_PER_SECOND = 1_000_000_000
MAX_SOURCE_TO_FREEZE_SECONDS = 3 * 24 * 60 * 60
MAX_EVIDENCE_WINDOW_SECONDS = 14 * 24 * 60 * 60
SCHEMA = "hardknock-release-evidence-v1"
RECEIPT_SCHEMA = "hardknock-release-gate-receipt-v1"
RESULT_SCHEMA = "hardknock-release-evidence-verification-v1"
PASS = "pass"
STATUSES = {"pass", "fail", "pending", "partial", "unavailable"}
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
RELEASE_CANDIDATE_TAG = re.compile(
    r"^v(?P<version>[0-9]+\.[0-9]+\.[0-9]+)-rc\.(?P<number>[1-9][0-9]*)$"
)
SHA256 = re.compile(r"^[0-9a-f]{64}$")
GIT_OBJECT = re.compile(r"^[0-9a-f]{40}$")
IMAGE_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
GITHUB_LOGIN = re.compile(
    r"^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$"
)
RFC3339_UTC = re.compile(
    r"^(?P<year>[0-9]{4})-(?P<month>[0-9]{2})-(?P<day>[0-9]{2})"
    r"T(?P<hour>[0-9]{2}):(?P<minute>[0-9]{2}):(?P<second>[0-9]{2})"
    r"(?:\.(?P<fraction>[0-9]{1,9}))?Z$"
)
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)
HOSTED_MATRIX = {
    "x86_64-unknown-linux-gnu": {
        "runner_image": "ubuntu-24.04",
        "operating_system": "linux",
        "architecture": "x86_64",
    },
    "aarch64-unknown-linux-gnu": {
        "runner_image": "ubuntu-24.04-arm",
        "operating_system": "linux",
        "architecture": "aarch64",
    },
    "x86_64-apple-darwin": {
        "runner_image": "macos-15-intel",
        "operating_system": "macos",
        "architecture": "x86_64",
    },
    "aarch64-apple-darwin": {
        "runner_image": "macos-15",
        "operating_system": "macos",
        "architecture": "aarch64",
    },
}
NATIVE_SERVICE_PLATFORM = {
    "systemd_user": {"manager": "systemd-user", "platform": "linux"},
    "launchd": {"manager": "launchd", "platform": "macos"},
}
SUPPORTED_ARCHITECTURES = {"x86_64", "aarch64"}
TOP_LEVEL_GATES = (
    "repository",
    "release_controls",
    "published_artifacts",
    "hosted_matrix",
    "native_services",
    "live_agents",
    "container_security",
    "recovery_drills",
    "soak",
    "dependency_advisories",
    "security_support",
    "compatibility_policy",
    "bootstrap_interruption",
    "repository_release_immutability",
)
DIRECT_GATE_KINDS = {
    "repository": "repository",
    "release_controls": "release_controls",
    "dependency_advisories": "dependency_advisories",
    "security_support": "security_support",
    "compatibility_policy": "compatibility_policy",
    "bootstrap_interruption": "bootstrap_interruption",
    "repository_release_immutability": "repository_release_immutability",
}
GROUP_GATES: dict[str, tuple[tuple[str, str], ...]] = {
    "published_artifacts": (
        ("metadata", "published_metadata"),
        *((target, "published_artifact") for target in TARGETS),
    ),
    "hosted_matrix": tuple((target, "hosted_matrix") for target in TARGETS),
    "native_services": (
        ("systemd_user", "native_service"),
        ("launchd", "native_service"),
    ),
    "live_agents": (
        ("generic_mcp", "live_agent"),
        ("claude_code", "live_agent"),
        ("codex", "live_agent"),
        ("second_agent_transfer", "live_agent"),
    ),
    "container_security": (
        ("rootless_docker", "container_security"),
        ("rootless_podman", "container_security"),
    ),
    "recovery_drills": tuple(
        (name, "recovery_drill")
        for name in (
            "interrupted_setup",
            "failed_upgrade_restore",
            "corrupted_database",
            "full_disk_or_quota",
            "killed_bridge",
            "failed_uninstall",
            "signing_compromise",
        )
    ),
    "soak": (("linux_24h", "soak"), ("macos_24h", "soak")),
}
OPTIONAL_GROUP_GATES = {
    "live_agents": (("hermes", "live_agent"), ("openclaw", "live_agent"))
}


def reject_json_constant(value: str) -> None:
    raise ValueError(f"non-standard JSON constant is forbidden: {value}")


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object key: {key}")
        value[key] = item
    return value


def file_identity(metadata: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def read_open_regular_file(
    descriptor: int, *, maximum: int, description: str
) -> bytes:
    before = os.fstat(descriptor)
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f"{description} must be a regular file")
    if not 0 < before.st_size <= maximum:
        raise ValueError(f"{description} is empty or exceeds {maximum} bytes")
    chunks: list[bytes] = []
    total = 0
    while True:
        chunk = os.read(descriptor, min(64 * 1024, maximum + 1 - total))
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
        if total > maximum:
            raise ValueError(f"{description} exceeds {maximum} bytes")
    after = os.fstat(descriptor)
    if file_identity(before) != file_identity(after) or total != before.st_size:
        raise ValueError(f"{description} changed while it was being read")
    return b"".join(chunks)


def read_regular_file(path: Path, *, maximum: int, description: str) -> bytes:
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{description} must be a regular, non-symlink file")
    descriptor = os.open(
        path, os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK | os.O_NOFOLLOW
    )
    try:
        return read_open_regular_file(
            descriptor, maximum=maximum, description=description
        )
    finally:
        os.close(descriptor)


def decode_json(encoded: bytes, description: str) -> Any:
    try:
        return json.loads(
            encoded.decode("utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=reject_json_constant,
        )
    except UnicodeDecodeError as error:
        raise ValueError(f"{description} is not valid UTF-8") from error


def read_record(path: Path) -> dict[str, Any]:
    value = decode_json(
        read_regular_file(
            path, maximum=MAX_RECORD_BYTES, description="evidence record"
        ),
        "evidence record",
    )
    if not isinstance(value, dict):
        raise ValueError("evidence record must be a JSON object")
    return value


def is_nonempty_string(value: Any, maximum: int) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= maximum
        and bool(value.strip())
        and not any(ord(character) < 32 or ord(character) == 127 for character in value)
    )


def exact_object(
    value: Any,
    label: str,
    required: tuple[str, ...],
    optional: tuple[str, ...],
    blockers: list[str],
) -> bool:
    if not isinstance(value, dict):
        blockers.append(f"{label}: expected an object")
        return False
    allowed = set(required) | set(optional)
    for key in sorted(set(value) - allowed):
        blockers.append(f"{label}.{key}: unknown field")
    for key in required:
        if key not in value:
            blockers.append(f"{label}.{key}: missing field")
    return all(key in value for key in required)


def rfc3339_utc_nanoseconds(value: Any) -> int | None:
    if not isinstance(value, str):
        return None
    match = RFC3339_UTC.fullmatch(value)
    if match is None:
        return None
    try:
        parsed = datetime(
            int(match.group("year")),
            int(match.group("month")),
            int(match.group("day")),
            int(match.group("hour")),
            int(match.group("minute")),
            int(match.group("second")),
            tzinfo=timezone.utc,
        )
    except ValueError:
        return None
    fraction = (match.group("fraction") or "").ljust(9, "0")
    return (
        calendar.timegm(parsed.utctimetuple()) * NANOSECONDS_PER_SECOND
        + int(fraction or "0")
    )


def valid_rfc3339_utc(value: Any) -> bool:
    return rfc3339_utc_nanoseconds(value) is not None


def validate_github_user_identity(
    value: Any,
    label: str,
    blockers: list[str],
) -> tuple[int | None, str | None]:
    if not exact_object(value, label, ("type", "id", "login"), (), blockers):
        return None, None
    assert isinstance(value, dict)
    if value["type"] != "User":
        blockers.append(f"{label}.type: must be 'User'; team reviewers are forbidden")
    identity = value["id"]
    valid_identity = (
        isinstance(identity, int)
        and not isinstance(identity, bool)
        and identity >= 1
    )
    if not valid_identity:
        blockers.append(f"{label}.id: must be a positive GitHub user ID")
    login = value["login"]
    normalized_login: str | None = None
    if not isinstance(login, str) or GITHUB_LOGIN.fullmatch(login) is None:
        blockers.append(
            f"{label}.login: must be a canonical lowercase GitHub user login"
        )
    elif login != login.casefold():
        blockers.append(
            f"{label}.login: must be normalized to canonical lowercase"
        )
    else:
        normalized_login = login.casefold()
    return identity if valid_identity else None, normalized_login


def canonical_evidence_path(value: Any) -> tuple[str | None, str | None]:
    if not is_nonempty_string(value, 4096):
        return None, "path must be a nonempty safe string"
    assert isinstance(value, str)
    if "\\" in value:
        return None, "path must use portable forward-slash components"
    path = PurePosixPath(value)
    if path.is_absolute():
        return None, "path must be relative to the evidence root"
    if not path.parts or any(component in ("", ".", "..") for component in path.parts):
        return None, "path contains a forbidden component"
    if path.as_posix() != value:
        return None, "path must be canonical"
    return value, None


class EvidenceResolver:
    def __init__(self, root: Path) -> None:
        self.root_descriptor = os.open(
            root, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
        )
        self.cache: dict[tuple[str, str], tuple[bytes | None, str | None]] = {}

    def close(self) -> None:
        os.close(self.root_descriptor)

    def load(
        self, relative_path: str, expected_digest: str
    ) -> tuple[bytes | None, str | None]:
        cache_key = (relative_path, expected_digest)
        if cache_key in self.cache:
            return self.cache[cache_key]
        path = PurePosixPath(relative_path)
        parent_descriptor = os.dup(self.root_descriptor)
        file_descriptor: int | None = None
        contents: bytes | None = None
        try:
            directory_flags = (
                os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
            )
            for component in path.parts[:-1]:
                next_descriptor = os.open(
                    component, directory_flags, dir_fd=parent_descriptor
                )
                os.close(parent_descriptor)
                parent_descriptor = next_descriptor
            metadata = os.stat(
                path.parts[-1], dir_fd=parent_descriptor, follow_symlinks=False
            )
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise ValueError("referenced path is not a regular, non-symlink file")
            file_descriptor = os.open(
                path.parts[-1],
                os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK | os.O_NOFOLLOW,
                dir_fd=parent_descriptor,
            )
            contents = read_open_regular_file(
                file_descriptor,
                maximum=MAX_EVIDENCE_BYTES,
                description=f"evidence file {relative_path!r}",
            )
            actual_digest = hashlib.sha256(contents).hexdigest()
            if actual_digest != expected_digest:
                error = (
                    f"SHA-256 mismatch for {relative_path!r}: "
                    f"expected {expected_digest}, got {actual_digest}"
                )
                contents = None
            else:
                error = None
        except (OSError, ValueError) as failure:
            contents = None
            error = f"cannot verify {relative_path!r}: {failure}"
        finally:
            if file_descriptor is not None:
                os.close(file_descriptor)
            os.close(parent_descriptor)
        result = (contents, error)
        self.cache[cache_key] = result
        return result


def validate_sha256(value: Any, label: str, blockers: list[str]) -> bool:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        blockers.append(f"{label}: expected 64 lowercase hexadecimal digits")
        return False
    if value == "0" * 64:
        blockers.append(f"{label}: unresolved placeholder digest")
        return False
    return True


def validate_artifact(
    value: Any,
    label: str,
    blockers: list[str],
    *,
    expected_name: str | None = None,
    expected: dict[str, Any] | None = None,
) -> None:
    if not exact_object(value, label, ("name", "sha256"), (), blockers):
        return
    assert isinstance(value, dict)
    if not is_nonempty_string(value["name"], 255) or "/" in str(value["name"]):
        blockers.append(f"{label}.name: invalid release asset name")
    elif expected_name is not None and value["name"] != expected_name:
        blockers.append(f"{label}.name: expected {expected_name!r}")
    validate_sha256(value["sha256"], f"{label}.sha256", blockers)
    if expected is not None and value != expected:
        blockers.append(f"{label}: does not match the promotion artifact manifest")


def expected_artifact_names(version: str) -> dict[str, Any]:
    return {
        "metadata": {
            "installer": "install-hardknock",
            "installer_checksum": "install-hardknock.sha256",
            "sbom": f"hardknock-{version}-sbom.cdx.json",
            "license_inventory": f"hardknock-{version}-third-party-licenses.json",
        },
        "targets": {
            target: {
                "archive": f"hardknock-{version}-{target}.tar.gz",
                "checksum": f"hardknock-{version}-{target}.tar.gz.sha256",
            }
            for target in TARGETS
        },
    }


def validate_artifacts(
    value: Any, version: str | None, blockers: list[str]
) -> dict[str, Any] | None:
    if not exact_object(value, "artifacts", ("metadata", "targets"), (), blockers):
        return None
    assert isinstance(value, dict)
    names = expected_artifact_names(version or "invalid")
    metadata_fields = (
        "installer",
        "installer_checksum",
        "sbom",
        "license_inventory",
    )
    metadata = value["metadata"]
    if exact_object(metadata, "artifacts.metadata", metadata_fields, (), blockers):
        assert isinstance(metadata, dict)
        for field in metadata_fields:
            validate_artifact(
                metadata[field],
                f"artifacts.metadata.{field}",
                blockers,
                expected_name=names["metadata"][field] if version else None,
            )
    targets = value["targets"]
    if exact_object(targets, "artifacts.targets", TARGETS, (), blockers):
        assert isinstance(targets, dict)
        for target in TARGETS:
            target_value = targets[target]
            if exact_object(
                target_value,
                f"artifacts.targets.{target}",
                ("archive", "checksum"),
                (),
                blockers,
            ):
                for field in ("archive", "checksum"):
                    validate_artifact(
                        target_value[field],
                        f"artifacts.targets.{target}.{field}",
                        blockers,
                        expected_name=names["targets"][target][field] if version else None,
                    )
    return value


def validate_checks(value: Any, label: str, blockers: list[str]) -> None:
    if not isinstance(value, list) or not value or len(value) > 128:
        blockers.append(f"{label}: expected 1 to 128 checks")
        return
    for index, check in enumerate(value):
        check_label = f"{label}[{index}]"
        if not exact_object(check, check_label, ("name", "status"), (), blockers):
            continue
        if not is_nonempty_string(check["name"], 256):
            blockers.append(f"{check_label}.name: invalid")
        if check["status"] != PASS:
            blockers.append(f"{check_label}.status: must be pass")


def receipt_candidate(candidate: dict[str, Any]) -> dict[str, Any]:
    source = candidate["source"]
    return {
        "version": candidate["version"],
        "source_repository": source["repository"],
        "source_tag": source["tag"],
        "source_commit": source["commit"],
        "source_tree": source["tree"],
    }


def require_true(value: Any, label: str, blockers: list[str]) -> None:
    if value is not True:
        blockers.append(f"{label}: must be true")


def require_zero_integer(value: Any, label: str, blockers: list[str]) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or value != 0:
        blockers.append(f"{label}: must be integer zero")


def validate_receipt_details(
    kind: str,
    details: Any,
    label: str,
    gate_label: str,
    candidate: dict[str, Any] | None,
    artifacts: dict[str, Any] | None,
    blockers: list[str],
) -> None:
    fields: dict[str, tuple[str, ...]] = {
        "repository": (
            "clean_checkout",
            "serial_test_passes",
            "package_verified",
            "default_branch",
            "default_branch_protected",
            "required_approving_reviews",
            "force_push_blocked",
            "deletion_blocked",
            "admin_enforcement",
            "bypass_actor_count",
        ),
        "release_controls": (
            "candidate_tag",
            "stable_tag",
            "candidate_tag_ruleset_id",
            "candidate_tag_ruleset_name",
            "candidate_tag_update_blocked",
            "candidate_tag_deletion_blocked",
            "candidate_tag_bypass_actor_count",
            "stable_tag_ruleset_id",
            "stable_tag_ruleset_name",
            "stable_tag_update_blocked",
            "stable_tag_deletion_blocked",
            "stable_tag_bypass_actor_count",
            "candidate_environment",
            "candidate_environment_protected",
            "candidate_environment_deployment_branch_protected",
            "candidate_environment_default_branch_only",
            "candidate_environment_prevent_self_review",
            "candidate_environment_required_reviewers",
            "stable_environment",
            "stable_environment_protected",
            "stable_environment_deployment_branch_protected",
            "stable_environment_default_branch_only",
            "stable_environment_prevent_self_review",
            "stable_environment_required_reviewers",
            "reviewer_user_ids_disjoint",
        ),
        "published_metadata": ("release_tag", "release_immutable", "assets"),
        "published_artifact": (
            "target",
            "release_tag",
            "release_immutable",
            "archive",
            "checksum",
        ),
        "hosted_matrix": (
            "target",
            "runner_image",
            "operating_system",
            "architecture",
            "workflow_run_url",
        ),
        "native_service": (
            "manager",
            "platform",
            "restart_verified",
            "bounded_shutdown",
            "logs_verified",
            "upgrade_verified",
            "uninstall_verified",
        ),
        "live_agent": ("agent", "agent_version", "lifecycle_verified"),
        "container_security": (
            "runtime",
            "runtime_version",
            "rootless",
            "image_digest",
        ),
        "recovery_drill": ("drill", "rollback_or_restore_verified"),
        "soak": ("platform", "duration_seconds", "probes_checked", "failures"),
        "dependency_advisories": (
            "database_updated_at_utc",
            "critical",
            "high",
        ),
        "security_support": (
            "security_contact",
            "support_contact",
            "published",
        ),
        "compatibility_policy": ("policy_version", "published"),
        "bootstrap_interruption": ("shells", "scenarios", "recovered"),
        "repository_release_immutability": (
            "source_release_tag",
            "source_release_verified",
            "verification_command",
        ),
    }
    required = ("checks",) + fields[kind]
    optional = ("architecture",) if kind == "native_service" else ()
    if not exact_object(details, label, required, optional, blockers):
        return
    assert isinstance(details, dict)
    validate_checks(details["checks"], f"{label}.checks", blockers)
    source_tag = (
        candidate["source"]["tag"]
        if isinstance(candidate, dict) and isinstance(candidate.get("source"), dict)
        else None
    )
    gate_name = gate_label.rsplit(".", 1)[-1]

    if kind == "repository":
        require_true(details["clean_checkout"], f"{label}.clean_checkout", blockers)
        if (
            not isinstance(details["serial_test_passes"], int)
            or isinstance(details["serial_test_passes"], bool)
            or details["serial_test_passes"] < 2
        ):
            blockers.append(f"{label}.serial_test_passes: must be at least 2")
        require_true(details["package_verified"], f"{label}.package_verified", blockers)
        if not is_nonempty_string(details["default_branch"], 255):
            blockers.append(f"{label}.default_branch: invalid")
        require_true(
            details["default_branch_protected"],
            f"{label}.default_branch_protected",
            blockers,
        )
        if (
            not isinstance(details["required_approving_reviews"], int)
            or isinstance(details["required_approving_reviews"], bool)
            or details["required_approving_reviews"] < 1
        ):
            blockers.append(
                f"{label}.required_approving_reviews: must be at least 1"
            )
        for field in (
            "force_push_blocked",
            "deletion_blocked",
            "admin_enforcement",
        ):
            require_true(details[field], f"{label}.{field}", blockers)
        require_zero_integer(
            details["bypass_actor_count"],
            f"{label}.bypass_actor_count",
            blockers,
        )
    elif kind == "release_controls":
        source = (
            candidate.get("source")
            if isinstance(candidate, dict)
            and isinstance(candidate.get("source"), dict)
            else None
        )
        expected_candidate_tag = source.get("tag") if isinstance(source, dict) else None
        expected_stable_tag = (
            candidate.get("tag") if isinstance(candidate, dict) else None
        )
        if details["candidate_tag"] != expected_candidate_tag:
            blockers.append(
                f"{label}.candidate_tag: does not match candidate source tag"
            )
        if details["stable_tag"] != expected_stable_tag:
            blockers.append(f"{label}.stable_tag: does not match stable tag")
        for prefix in ("candidate", "stable"):
            ruleset_id = details[f"{prefix}_tag_ruleset_id"]
            if (
                not isinstance(ruleset_id, int)
                or isinstance(ruleset_id, bool)
                or ruleset_id < 1
            ):
                blockers.append(
                    f"{label}.{prefix}_tag_ruleset_id: must be a positive integer"
                )
            if not is_nonempty_string(
                details[f"{prefix}_tag_ruleset_name"], 256
            ):
                blockers.append(f"{label}.{prefix}_tag_ruleset_name: invalid")
            for field in ("update_blocked", "deletion_blocked"):
                require_true(
                    details[f"{prefix}_tag_{field}"],
                    f"{label}.{prefix}_tag_{field}",
                    blockers,
                )
            require_zero_integer(
                details[f"{prefix}_tag_bypass_actor_count"],
                f"{label}.{prefix}_tag_bypass_actor_count",
                blockers,
            )
        environments = {
            "candidate": "hardknock-candidate-publication",
            "stable": "hardknock-stable-publication",
        }
        reviewer_ids: dict[str, set[int]] = {}
        reviewer_logins: dict[str, set[str]] = {}
        for prefix, expected_environment in environments.items():
            if details[f"{prefix}_environment"] != expected_environment:
                blockers.append(
                    f"{label}.{prefix}_environment: "
                    f"expected {expected_environment!r}"
                )
            for field in (
                "environment_protected",
                "environment_deployment_branch_protected",
                "environment_default_branch_only",
                "environment_prevent_self_review",
            ):
                require_true(
                    details[f"{prefix}_{field}"],
                    f"{label}.{prefix}_{field}",
                    blockers,
                )
            reviewers = details[f"{prefix}_environment_required_reviewers"]
            ids: set[int] = set()
            logins: set[str] = set()
            if (
                not isinstance(reviewers, list)
                or not reviewers
                or len(reviewers) > 6
            ):
                blockers.append(
                    f"{label}.{prefix}_environment_required_reviewers: "
                    "expected 1 to 6 direct user reviewers"
                )
            else:
                for index, reviewer in enumerate(reviewers):
                    reviewer_label = (
                        f"{label}.{prefix}_environment_required_reviewers[{index}]"
                    )
                    identity, login = validate_github_user_identity(
                        reviewer, reviewer_label, blockers
                    )
                    if identity is not None and identity in ids:
                        blockers.append(
                            f"{reviewer_label}.id: duplicate reviewer user ID"
                        )
                    elif identity is not None:
                        ids.add(identity)
                    if login is not None and login in logins:
                        blockers.append(
                            f"{reviewer_label}.login: duplicate reviewer login"
                        )
                    elif login is not None:
                        logins.add(login)
            reviewer_ids[prefix] = ids
            reviewer_logins[prefix] = logins
        require_true(
            details["reviewer_user_ids_disjoint"],
            f"{label}.reviewer_user_ids_disjoint",
            blockers,
        )
        overlap = reviewer_ids.get("candidate", set()) & reviewer_ids.get(
            "stable", set()
        )
        if overlap:
            blockers.append(
                f"{label}: candidate and stable environment reviewer user IDs "
                f"must be disjoint; overlap={sorted(overlap)}"
            )
        login_overlap = reviewer_logins.get(
            "candidate", set()
        ) & reviewer_logins.get("stable", set())
        if login_overlap:
            blockers.append(
                f"{label}: one normalized GitHub login maps to reviewer IDs in "
                f"both environments; overlap={sorted(login_overlap)}"
            )
        operator = (
            candidate.get("operator") if isinstance(candidate, dict) else None
        )
        if isinstance(operator, dict):
            operator_id = operator.get("id")
            operator_login = operator.get("login")
            normalized_operator_login = (
                operator_login.casefold()
                if isinstance(operator_login, str)
                else None
            )
            for prefix in environments:
                if (
                    operator_id in reviewer_ids.get(prefix, set())
                    or normalized_operator_login
                    in reviewer_logins.get(prefix, set())
                ):
                    blockers.append(
                        f"{label}.{prefix}_environment_required_reviewers: "
                        "must differ from candidate operator"
                    )
    elif kind in ("published_metadata", "published_artifact"):
        if details["release_tag"] != source_tag:
            blockers.append(f"{label}.release_tag: does not match candidate source tag")
        require_true(
            details["release_immutable"], f"{label}.release_immutable", blockers
        )
        if kind == "published_metadata":
            expected = artifacts.get("metadata") if isinstance(artifacts, dict) else None
            asset_fields = (
                "installer",
                "installer_checksum",
                "sbom",
                "license_inventory",
            )
            if exact_object(
                details["assets"], f"{label}.assets", asset_fields, (), blockers
            ):
                for field in asset_fields:
                    validate_artifact(
                        details["assets"][field],
                        f"{label}.assets.{field}",
                        blockers,
                        expected=expected.get(field) if isinstance(expected, dict) else None,
                    )
        else:
            if details["target"] != gate_name:
                blockers.append(f"{label}.target: expected {gate_name!r}")
            expected_target = (
                artifacts.get("targets", {}).get(gate_name)
                if isinstance(artifacts, dict)
                else None
            )
            for field in ("archive", "checksum"):
                validate_artifact(
                    details[field],
                    f"{label}.{field}",
                    blockers,
                    expected=(
                        expected_target.get(field)
                        if isinstance(expected_target, dict)
                        else None
                    ),
                )
    elif kind == "hosted_matrix":
        if details["target"] != gate_name:
            blockers.append(f"{label}.target: expected {gate_name!r}")
        expected = HOSTED_MATRIX[gate_name]
        for field, expected_value in expected.items():
            if details[field] != expected_value:
                blockers.append(
                    f"{label}.{field}: expected {expected_value!r} "
                    f"for target {gate_name!r}"
                )
        if not (
            is_nonempty_string(details["workflow_run_url"], 2048)
            and details["workflow_run_url"].startswith("https://")
        ):
            blockers.append(f"{label}.workflow_run_url: expected an HTTPS URL")
    elif kind == "native_service":
        expected = NATIVE_SERVICE_PLATFORM[gate_name]
        for field, expected_value in expected.items():
            if details[field] != expected_value:
                blockers.append(
                    f"{label}.{field}: expected {expected_value!r} "
                    f"for gate {gate_name!r}"
                )
        if (
            "architecture" in details
            and details["architecture"] not in SUPPORTED_ARCHITECTURES
        ):
            blockers.append(
                f"{label}.architecture: expected one of "
                f"{sorted(SUPPORTED_ARCHITECTURES)!r}"
            )
        for field in fields[kind][2:]:
            require_true(details[field], f"{label}.{field}", blockers)
    elif kind == "live_agent":
        if details["agent"] != gate_name:
            blockers.append(f"{label}.agent: expected {gate_name!r}")
        if not is_nonempty_string(details["agent_version"], 256):
            blockers.append(f"{label}.agent_version: invalid")
        require_true(
            details["lifecycle_verified"], f"{label}.lifecycle_verified", blockers
        )
    elif kind == "container_security":
        expected = {"rootless_docker": "docker", "rootless_podman": "podman"}[
            gate_name
        ]
        if details["runtime"] != expected:
            blockers.append(f"{label}.runtime: expected {expected!r}")
        if not is_nonempty_string(details["runtime_version"], 256):
            blockers.append(f"{label}.runtime_version: invalid")
        require_true(details["rootless"], f"{label}.rootless", blockers)
        if not isinstance(details["image_digest"], str) or IMAGE_DIGEST.fullmatch(
            details["image_digest"]
        ) is None:
            blockers.append(f"{label}.image_digest: invalid")
    elif kind == "recovery_drill":
        if details["drill"] != gate_name:
            blockers.append(f"{label}.drill: expected {gate_name!r}")
        require_true(
            details["rollback_or_restore_verified"],
            f"{label}.rollback_or_restore_verified",
            blockers,
        )
    elif kind == "soak":
        expected = {"linux_24h": "linux", "macos_24h": "macos"}[gate_name]
        if details["platform"] != expected:
            blockers.append(f"{label}.platform: expected {expected!r}")
        if (
            not isinstance(details["duration_seconds"], int)
            or isinstance(details["duration_seconds"], bool)
            or details["duration_seconds"] < 86400
        ):
            blockers.append(f"{label}.duration_seconds: must be at least 86400")
        require_true(details["probes_checked"], f"{label}.probes_checked", blockers)
        require_zero_integer(details["failures"], f"{label}.failures", blockers)
    elif kind == "dependency_advisories":
        if not valid_rfc3339_utc(details["database_updated_at_utc"]):
            blockers.append(f"{label}.database_updated_at_utc: invalid")
        for field in ("critical", "high"):
            require_zero_integer(details[field], f"{label}.{field}", blockers)
    elif kind == "security_support":
        for field in ("security_contact", "support_contact"):
            if not is_nonempty_string(details[field], 512):
                blockers.append(f"{label}.{field}: invalid")
        require_true(details["published"], f"{label}.published", blockers)
    elif kind == "compatibility_policy":
        if not is_nonempty_string(details["policy_version"], 128):
            blockers.append(f"{label}.policy_version: invalid")
        require_true(details["published"], f"{label}.published", blockers)
    elif kind == "bootstrap_interruption":
        if not isinstance(details["shells"], list) or set(details["shells"]) != {
            "/bin/sh",
            "/bin/dash",
        }:
            blockers.append(f"{label}.shells: must contain /bin/sh and /bin/dash")
        if not isinstance(details["scenarios"], list) or set(
            details["scenarios"]
        ) != {
            "fresh_install",
            "upgrade",
            "profile_mutation",
            "uninstall",
        }:
            blockers.append(f"{label}.scenarios: incomplete interruption coverage")
        require_true(details["recovered"], f"{label}.recovered", blockers)
    elif kind == "repository_release_immutability":
        if details["source_release_tag"] != source_tag:
            blockers.append(
                f"{label}.source_release_tag: does not match candidate source tag"
            )
        require_true(
            details["source_release_verified"],
            f"{label}.source_release_verified",
            blockers,
        )
        if details["verification_command"] != "gh release verify":
            blockers.append(f"{label}.verification_command: invalid")


def validate_receipt(
    encoded: bytes,
    *,
    label: str,
    gate_label: str,
    expected_kind: str,
    candidate: dict[str, Any] | None,
    artifacts: dict[str, Any] | None,
    blockers: list[str],
) -> None:
    try:
        receipt = decode_json(encoded, label)
    except (ValueError, json.JSONDecodeError) as error:
        blockers.append(f"{label}: invalid typed receipt: {error}")
        return
    fields = (
        "schema",
        "gate",
        "kind",
        "candidate",
        "status",
        "observed_at_utc",
        "producer",
        "details",
    )
    if not exact_object(receipt, label, fields, (), blockers):
        return
    assert isinstance(receipt, dict)
    if receipt["schema"] != RECEIPT_SCHEMA:
        blockers.append(f"{label}.schema: unsupported value")
    if receipt["gate"] != gate_label:
        blockers.append(f"{label}.gate: expected {gate_label!r}")
    if receipt["kind"] != expected_kind:
        blockers.append(f"{label}.kind: expected {expected_kind!r}")
    if receipt["status"] != PASS:
        blockers.append(f"{label}.status: must be pass")
    observed_at = rfc3339_utc_nanoseconds(receipt["observed_at_utc"])
    if observed_at is None:
        blockers.append(f"{label}.observed_at_utc: invalid")
    elif isinstance(candidate, dict):
        frozen_at = rfc3339_utc_nanoseconds(candidate.get("frozen_at_utc"))
        completed_at = rfc3339_utc_nanoseconds(
            candidate.get("evidence_completed_at_utc")
        )
        if frozen_at is not None and observed_at < frozen_at:
            blockers.append(
                f"{label}.observed_at_utc: precedes candidate.frozen_at_utc"
            )
        if completed_at is not None and observed_at > completed_at:
            blockers.append(
                f"{label}.observed_at_utc: exceeds "
                "candidate.evidence_completed_at_utc"
            )
    if not is_nonempty_string(receipt["producer"], 256):
        blockers.append(f"{label}.producer: invalid")
    if isinstance(candidate, dict):
        expected_candidate = receipt_candidate(candidate)
        if receipt["candidate"] != expected_candidate:
            blockers.append(f"{label}.candidate: does not match promoted candidate")
    validate_receipt_details(
        expected_kind,
        receipt["details"],
        f"{label}.details",
        gate_label,
        candidate,
        artifacts,
        blockers,
    )


def validate_reference(
    value: Any,
    label: str,
    *,
    gate_label: str,
    expected_kind: str,
    resolver: EvidenceResolver | None,
    candidate: dict[str, Any] | None,
    artifacts: dict[str, Any] | None,
    blockers: list[str],
) -> None:
    if not exact_object(value, label, ("path", "sha256", "kind"), (), blockers):
        return
    assert isinstance(value, dict)
    relative_path, path_error = canonical_evidence_path(value["path"])
    if path_error:
        blockers.append(f"{label}.path: {path_error}")
    digest_valid = validate_sha256(value["sha256"], f"{label}.sha256", blockers)
    if value["kind"] != expected_kind:
        blockers.append(f"{label}.kind: expected {expected_kind!r}")
    if relative_path and digest_valid and resolver is not None:
        encoded, error = resolver.load(relative_path, value["sha256"])
        if error:
            blockers.append(f"{label}: {error}")
        elif encoded is not None:
            validate_receipt(
                encoded,
                label=label,
                gate_label=gate_label,
                expected_kind=expected_kind,
                candidate=candidate,
                artifacts=artifacts,
                blockers=blockers,
            )


def validate_gate(
    value: Any,
    label: str,
    expected_kind: str,
    resolver: EvidenceResolver | None,
    candidate: dict[str, Any] | None,
    artifacts: dict[str, Any] | None,
    blockers: list[str],
) -> None:
    if not exact_object(value, label, ("status", "evidence"), ("notes",), blockers):
        return
    assert isinstance(value, dict)
    status_value = value["status"]
    if not isinstance(status_value, str) or status_value not in STATUSES:
        blockers.append(f"{label}.status: invalid status")
    evidence = value["evidence"]
    if not isinstance(evidence, list):
        blockers.append(f"{label}.evidence: expected an array")
    elif len(evidence) > 256:
        blockers.append(f"{label}.evidence: exceeds 256 entries")
    else:
        for index, reference in enumerate(evidence):
            validate_reference(
                reference,
                f"{label}.evidence[{index}]",
                gate_label=label,
                expected_kind=expected_kind,
                resolver=resolver,
                candidate=candidate,
                artifacts=artifacts,
                blockers=blockers,
            )
        if status_value == PASS and not evidence:
            blockers.append(f"{label}: passing gate has no evidence")
    if "notes" in value and not is_nonempty_string(value["notes"], 8192):
        blockers.append(f"{label}.notes: invalid")
    if status_value in STATUSES and status_value != PASS:
        blockers.append(f"{label}: status is {status_value}")


def validate_string_array(value: Any, label: str, blockers: list[str]) -> None:
    if not isinstance(value, list):
        blockers.append(f"{label}: expected an array")
        return
    if len(value) > 128:
        blockers.append(f"{label}: exceeds 128 entries")
    for index, item in enumerate(value):
        if not is_nonempty_string(item, 2048):
            blockers.append(f"{label}[{index}]: invalid")


def validate_candidate(
    value: Any,
    blockers: list[str],
    *,
    expected_version: str,
    expected_tag: str,
    expected_source_repository: str,
    expected_source_tag: str | None,
    expected_source_commit: str | None,
    expected_source_tree: str | None,
    expected_source_date_epoch: int | None,
) -> dict[str, Any] | None:
    fields = (
        "version",
        "tag",
        "source",
        "operator",
        "independent_reviewer",
        "frozen_at_utc",
        "evidence_completed_at_utc",
    )
    if not exact_object(value, "candidate", fields, (), blockers):
        return None
    assert isinstance(value, dict)
    version = value["version"]
    version_valid = isinstance(version, str) and VERSION.fullmatch(version) is not None
    if not version_valid:
        blockers.append("candidate.version: expected an exact stable X.Y.Z version")
    elif version != expected_version:
        blockers.append(f"candidate.version: expected {expected_version!r}")
    if not isinstance(version, str) or value["tag"] != f"v{version}":
        blockers.append("candidate.tag: must equal v plus candidate.version")
    if value["tag"] != expected_tag:
        blockers.append(f"candidate.tag: expected {expected_tag!r}")

    source = value["source"]
    source_fields = (
        "repository",
        "tag",
        "commit",
        "tree",
        "source_date_epoch",
    )
    if exact_object(source, "candidate.source", source_fields, (), blockers):
        assert isinstance(source, dict)
        if source["repository"] != expected_source_repository:
            blockers.append(
                f"candidate.source.repository: expected {expected_source_repository!r}"
            )
        pattern = (
            re.compile(rf"^v{re.escape(version)}-rc\.[1-9][0-9]*$")
            if version_valid
            else None
        )
        if (
            not isinstance(source["tag"], str)
            or pattern is None
            or pattern.fullmatch(source["tag"]) is None
        ):
            blockers.append(
                "candidate.source.tag: expected immutable v<version>-rc.N"
            )
        elif RELEASE_CANDIDATE_TAG.fullmatch(source["tag"]) is None:
            blockers.append("candidate.source.tag: invalid release-candidate tag")
        if source.get("tag") == value.get("tag"):
            blockers.append(
                "candidate.source.tag: must differ from the intended stable tag"
            )
        for field in ("commit", "tree"):
            if not isinstance(source[field], str) or GIT_OBJECT.fullmatch(
                source[field]
            ) is None:
                blockers.append(f"candidate.source.{field}: invalid Git object ID")
            elif source[field] == "0" * 40:
                blockers.append(
                    f"candidate.source.{field}: unresolved placeholder object ID"
                )
        epoch = source["source_date_epoch"]
        if (
            not isinstance(epoch, int)
            or isinstance(epoch, bool)
            or not 1 <= epoch <= MAX_SOURCE_DATE_EPOCH
        ):
            blockers.append("candidate.source.source_date_epoch: invalid")
        for field, expected in (
            ("tag", expected_source_tag),
            ("commit", expected_source_commit),
            ("tree", expected_source_tree),
            ("source_date_epoch", expected_source_date_epoch),
        ):
            if expected is not None and source.get(field) != expected:
                blockers.append(
                    f"candidate.source.{field}: expected {expected!r}, "
                    f"got {source.get(field)!r}"
                )

    operator_id, operator_login = validate_github_user_identity(
        value["operator"], "candidate.operator", blockers
    )
    reviewer_id, reviewer_login = validate_github_user_identity(
        value["independent_reviewer"],
        "candidate.independent_reviewer",
        blockers,
    )
    if (
        operator_id is not None
        and reviewer_id is not None
        and operator_id == reviewer_id
    ):
        blockers.append(
            "candidate: operator and independent reviewer GitHub user IDs "
            "must differ"
        )
    if (
        operator_login is not None
        and reviewer_login is not None
        and operator_login == reviewer_login
    ):
        blockers.append(
            "candidate: operator and independent reviewer normalized logins "
            "must differ"
        )

    frozen_at = rfc3339_utc_nanoseconds(value["frozen_at_utc"])
    if frozen_at is None:
        blockers.append("candidate.frozen_at_utc: invalid")
    completed_at = rfc3339_utc_nanoseconds(value["evidence_completed_at_utc"])
    if completed_at is None:
        blockers.append("candidate.evidence_completed_at_utc: invalid")

    source_epoch = (
        source.get("source_date_epoch") if isinstance(source, dict) else None
    )
    if (
        frozen_at is not None
        and isinstance(source_epoch, int)
        and not isinstance(source_epoch, bool)
        and 1 <= source_epoch <= MAX_SOURCE_DATE_EPOCH
    ):
        source_at = source_epoch * NANOSECONDS_PER_SECOND
        if frozen_at < source_at:
            blockers.append(
                "candidate.frozen_at_utc: precedes exact source commit timestamp"
            )
        elif frozen_at - source_at > (
            MAX_SOURCE_TO_FREEZE_SECONDS * NANOSECONDS_PER_SECOND
        ):
            blockers.append(
                "candidate.frozen_at_utc: exceeds the 72-hour "
                "source-to-freeze window"
            )
    if frozen_at is not None and completed_at is not None:
        if completed_at < frozen_at:
            blockers.append(
                "candidate.evidence_completed_at_utc: precedes "
                "candidate.frozen_at_utc"
            )
        elif completed_at - frozen_at > (
            MAX_EVIDENCE_WINDOW_SECONDS * NANOSECONDS_PER_SECOND
        ):
            blockers.append(
                "candidate.evidence_completed_at_utc: exceeds the 14-day "
                "evidence window"
            )
    return value


def verify(
    record: dict[str, Any],
    resolver: EvidenceResolver | None,
    *,
    expected_version: str,
    expected_tag: str,
    expected_source_repository: str,
    expected_source_tag: str | None,
    expected_source_commit: str | None,
    expected_source_tree: str | None,
    expected_source_date_epoch: int | None,
) -> list[str]:
    blockers: list[str] = []
    root_fields = (
        "schema",
        "candidate",
        "artifacts",
        "decision",
        "gates",
        "known_limitations",
        "blocking_defects",
    )
    if not exact_object(record, "record", root_fields, (), blockers):
        return blockers
    if record["schema"] != SCHEMA:
        blockers.append("schema: unsupported value")
    candidate = validate_candidate(
        record["candidate"],
        blockers,
        expected_version=expected_version,
        expected_tag=expected_tag,
        expected_source_repository=expected_source_repository,
        expected_source_tag=expected_source_tag,
        expected_source_commit=expected_source_commit,
        expected_source_tree=expected_source_tree,
        expected_source_date_epoch=expected_source_date_epoch,
    )
    version = candidate.get("version") if isinstance(candidate, dict) else None
    artifacts = validate_artifacts(
        record["artifacts"], version if isinstance(version, str) else None, blockers
    )
    if record["decision"] != "promote":
        blockers.append("decision: must be promote")

    gates = record["gates"]
    if exact_object(gates, "gates", TOP_LEVEL_GATES, (), blockers):
        assert isinstance(gates, dict)
        for name, kind in DIRECT_GATE_KINDS.items():
            validate_gate(
                gates[name],
                f"gates.{name}",
                kind,
                resolver,
                candidate,
                artifacts,
                blockers,
            )
        for group_name, required in GROUP_GATES.items():
            optional = OPTIONAL_GROUP_GATES.get(group_name, ())
            group = gates[group_name]
            if exact_object(
                group,
                f"gates.{group_name}",
                tuple(name for name, _ in required),
                tuple(name for name, _ in optional),
                blockers,
            ):
                for name, kind in required + optional:
                    if name in group:
                        validate_gate(
                            group[name],
                            f"gates.{group_name}.{name}",
                            kind,
                            resolver,
                            candidate,
                            artifacts,
                            blockers,
                        )

    validate_string_array(record["known_limitations"], "known_limitations", blockers)
    defects = record["blocking_defects"]
    validate_string_array(defects, "blocking_defects", blockers)
    if isinstance(defects, list) and defects:
        blockers.append(f"blocking_defects: {len(defects)} unresolved")
    return blockers


def iter_artifacts(artifacts: dict[str, Any]) -> list[dict[str, str]]:
    result: list[dict[str, str]] = []
    for field in ("installer", "installer_checksum", "sbom", "license_inventory"):
        result.append(artifacts["metadata"][field])
    for target in TARGETS:
        for field in ("archive", "checksum"):
            result.append(artifacts["targets"][target][field])
    return result


def verify_asset_root(
    root: Path, artifacts: dict[str, Any], blockers: list[str]
) -> None:
    try:
        metadata = os.lstat(root)
    except OSError as error:
        blockers.append(f"asset_root: cannot inspect safely: {error}")
        return
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        blockers.append("asset_root: expected a regular, non-symlink directory")
        return
    expected = {item["name"]: item["sha256"] for item in iter_artifacts(artifacts)}
    actual = {entry.name for entry in os.scandir(root)}
    if actual != set(expected):
        missing = sorted(set(expected) - actual)
        extra = sorted(actual - set(expected))
        if missing:
            blockers.append(f"asset_root: missing assets: {', '.join(missing)}")
        if extra:
            blockers.append(f"asset_root: unexpected assets: {', '.join(extra)}")
    for name, digest in expected.items():
        try:
            contents = read_regular_file(
                root / name,
                maximum=MAX_ASSET_BYTES,
                description=f"release asset {name!r}",
            )
        except (OSError, ValueError) as error:
            blockers.append(f"asset_root: {error}")
            continue
        actual_digest = hashlib.sha256(contents).hexdigest()
        if actual_digest != digest:
            blockers.append(
                f"asset_root: SHA-256 mismatch for {name!r}: "
                f"expected {digest}, got {actual_digest}"
            )


def positive_epoch(value: str) -> int:
    try:
        epoch = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if not 1 <= epoch <= MAX_SOURCE_DATE_EPOCH:
        raise argparse.ArgumentTypeError(
            f"must be between 1 and {MAX_SOURCE_DATE_EPOCH}"
        )
    return epoch


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("record", type=Path)
    parser.add_argument("--evidence-root", required=True, type=Path)
    parser.add_argument("--asset-root", type=Path)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--expected-tag", required=True)
    parser.add_argument("--expected-source-repository", required=True)
    parser.add_argument("--expected-source-tag")
    parser.add_argument("--expected-source-commit")
    parser.add_argument("--expected-source-tree")
    parser.add_argument("--expected-source-date-epoch", type=positive_epoch)
    args = parser.parse_args()

    resolver: EvidenceResolver | None = None
    record: dict[str, Any] | None = None
    try:
        record = read_record(args.record)
        resolver = EvidenceResolver(args.evidence_root)
        blockers = verify(
            record,
            resolver,
            expected_version=args.expected_version,
            expected_tag=args.expected_tag,
            expected_source_repository=args.expected_source_repository,
            expected_source_tag=args.expected_source_tag,
            expected_source_commit=args.expected_source_commit,
            expected_source_tree=args.expected_source_tree,
            expected_source_date_epoch=args.expected_source_date_epoch,
        )
        if (
            args.asset_root is not None
            and isinstance(record.get("artifacts"), dict)
            and not any(blocker.startswith("artifacts") for blocker in blockers)
        ):
            verify_asset_root(args.asset_root, record["artifacts"], blockers)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        blockers = [f"record: {error}"]
    finally:
        if resolver is not None:
            resolver.close()

    result: dict[str, Any] = {
        "schema": RESULT_SCHEMA,
        "ok": not blockers,
        "record": str(args.record),
        "evidence_root": str(args.evidence_root),
        "blockers": blockers,
    }
    if not blockers and record is not None:
        result["candidate"] = record["candidate"]
        result["artifacts"] = record["artifacts"]
    json.dump(result, sys.stdout, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")
    return 0 if not blockers else 1


if __name__ == "__main__":
    raise SystemExit(main())
