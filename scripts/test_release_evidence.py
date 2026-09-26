#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import copy
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "release/evidence-template.json"
SCHEMA = ROOT / "schemas/release-evidence-v1.schema.json"
VERIFIER = ROOT / "scripts/verify_release_evidence.py"
VERSION = "1.0.0"
TAG = "v1.0.0"
SOURCE_TAG = "v1.0.0-rc.1"
SOURCE_COMMIT = "1" * 40
SOURCE_TREE = "2" * 40
SOURCE_EPOCH = 1790294400
REPOSITORY = "openkedge/hardknock"
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


def json_equal(left: Any, right: Any) -> bool:
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    return left == right


def resolve_ref(root: dict[str, Any], reference: str) -> dict[str, Any]:
    assert reference.startswith("#/"), reference
    value: Any = root
    for encoded in reference[2:].split("/"):
        component = encoded.replace("~1", "/").replace("~0", "~")
        value = value[component]
    assert isinstance(value, dict)
    return value


def schema_errors(
    value: Any,
    schema: dict[str, Any],
    root: dict[str, Any],
    path: str = "$",
) -> list[str]:
    errors: list[str] = []
    reference = schema.get("$ref")
    if isinstance(reference, str):
        errors.extend(schema_errors(value, resolve_ref(root, reference), root, path))

    for index, subschema in enumerate(schema.get("allOf", [])):
        errors.extend(
            schema_errors(value, subschema, root, f"{path}.allOf[{index}]")
        )

    condition = schema.get("if")
    if isinstance(condition, dict):
        condition_matches = not schema_errors(value, condition, root, path)
        selected = schema.get("then") if condition_matches else schema.get("else")
        if isinstance(selected, dict):
            errors.extend(schema_errors(value, selected, root, path))

    forbidden = schema.get("not")
    if isinstance(forbidden, dict) and not schema_errors(value, forbidden, root, path):
        errors.append(f"{path}: matched forbidden schema")

    expected_type = schema.get("type")
    if expected_type is not None:
        matches = {
            "object": isinstance(value, dict),
            "array": isinstance(value, list),
            "string": isinstance(value, str),
            "integer": isinstance(value, int) and not isinstance(value, bool),
            "boolean": isinstance(value, bool),
            "number": isinstance(value, (int, float))
            and not isinstance(value, bool),
            "null": value is None,
        }.get(expected_type, False)
        if not matches:
            errors.append(f"{path}: expected {expected_type}")
            return errors

    if "const" in schema and not json_equal(value, schema["const"]):
        errors.append(f"{path}: expected const {schema['const']!r}")
    if "enum" in schema and not any(
        json_equal(value, item) for item in schema["enum"]
    ):
        errors.append(f"{path}: value is not in enum")

    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{path}: shorter than minLength")
        maximum = schema.get("maxLength")
        if isinstance(maximum, int) and len(value) > maximum:
            errors.append(f"{path}: longer than maxLength")
        pattern = schema.get("pattern")
        if isinstance(pattern, str) and re.search(pattern, value) is None:
            errors.append(f"{path}: does not match pattern")
        if schema.get("format") == "date-time":
            try:
                parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
                if parsed.tzinfo is None:
                    raise ValueError("missing timezone")
            except ValueError:
                errors.append(f"{path}: invalid date-time")

    if (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and "minimum" in schema
        and value < schema["minimum"]
    ):
        errors.append(f"{path}: below minimum")
    if (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and "maximum" in schema
        and value > schema["maximum"]
    ):
        errors.append(f"{path}: above maximum")

    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            errors.append(f"{path}: fewer than minItems")
        maximum = schema.get("maxItems")
        if isinstance(maximum, int) and len(value) > maximum:
            errors.append(f"{path}: more than maxItems")
        if schema.get("uniqueItems") is True:
            encoded = [
                json.dumps(item, sort_keys=True, separators=(",", ":"))
                for item in value
            ]
            if len(encoded) != len(set(encoded)):
                errors.append(f"{path}: items are not unique")
        items = schema.get("items")
        if isinstance(items, dict):
            for index, item in enumerate(value):
                errors.extend(schema_errors(item, items, root, f"{path}[{index}]"))

    if isinstance(value, dict):
        required = schema.get("required", [])
        for field in required:
            if field not in value:
                errors.append(f"{path}.{field}: missing required property")
        minimum = schema.get("minProperties")
        if isinstance(minimum, int) and len(value) < minimum:
            errors.append(f"{path}: fewer than minProperties")
        maximum = schema.get("maxProperties")
        if isinstance(maximum, int) and len(value) > maximum:
            errors.append(f"{path}: more than maxProperties")
        properties = schema.get("properties", {})
        if isinstance(properties, dict):
            for field, subschema in properties.items():
                if field in value:
                    errors.extend(
                        schema_errors(
                            value[field], subschema, root, f"{path}.{field}"
                        )
                    )
            additional = schema.get("additionalProperties", True)
            unknown = set(value) - set(properties)
            if additional is False:
                for field in sorted(unknown):
                    errors.append(f"{path}.{field}: additional property")
            elif isinstance(additional, dict):
                for field in sorted(unknown):
                    errors.extend(
                        schema_errors(
                            value[field], additional, root, f"{path}.{field}"
                        )
                    )
    return errors


def schema_document_errors(
    record: dict[str, Any], evidence_root: Path, schema: dict[str, Any]
) -> list[str]:
    errors = schema_errors(record, schema, schema)
    for gate, _kind in gate_specs():
        try:
            references = gate_at(record, gate)["evidence"]
        except (KeyError, TypeError):
            continue
        if not isinstance(references, list):
            continue
        for index, reference in enumerate(references):
            if not isinstance(reference, dict) or not isinstance(
                reference.get("path"), str
            ):
                continue
            path = evidence_root / reference["path"]
            try:
                receipt = json.loads(path.read_text())
            except (OSError, json.JSONDecodeError):
                continue
            errors.extend(
                schema_errors(
                    receipt,
                    schema["$defs"]["gate_receipt"],
                    schema,
                    f"{gate}.receipt[{index}]",
                )
            )
    return errors


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(
    record: Path,
    evidence_root: Path,
    *,
    asset_root: Path | None = None,
    source_commit: str = SOURCE_COMMIT,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    command = [
        sys.executable,
        str(VERIFIER),
        str(record),
        "--evidence-root",
        str(evidence_root),
        "--expected-version",
        VERSION,
        "--expected-tag",
        TAG,
        "--expected-source-repository",
        REPOSITORY,
        "--expected-source-tag",
        SOURCE_TAG,
        "--expected-source-commit",
        source_commit,
        "--expected-source-tree",
        SOURCE_TREE,
        "--expected-source-date-epoch",
        str(SOURCE_EPOCH),
    ]
    if asset_root is not None:
        command.extend(["--asset-root", str(asset_root)])
    process = subprocess.run(command, check=False, capture_output=True, text=True)
    try:
        result = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise AssertionError(
            f"verifier did not emit JSON\nstdout={process.stdout}\nstderr={process.stderr}"
        ) from error
    return process, result


def artifact_names(version: str) -> dict[str, Any]:
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


def create_assets(asset_root: Path) -> dict[str, Any]:
    names = artifact_names(VERSION)
    artifacts: dict[str, Any] = {"metadata": {}, "targets": {}}
    for field, name in names["metadata"].items():
        path = asset_root / name
        path.write_text(f"metadata:{field}:{VERSION}\n")
        artifacts["metadata"][field] = {"name": name, "sha256": sha256(path)}
    for target in TARGETS:
        artifacts["targets"][target] = {}
        for field, name in names["targets"][target].items():
            path = asset_root / name
            path.write_text(f"{field}:{target}:{VERSION}\n")
            artifacts["targets"][target][field] = {
                "name": name,
                "sha256": sha256(path),
            }
    return artifacts


def candidate() -> dict[str, Any]:
    return {
        "version": VERSION,
        "tag": TAG,
        "source": {
            "repository": REPOSITORY,
            "tag": SOURCE_TAG,
            "commit": SOURCE_COMMIT,
            "tree": SOURCE_TREE,
            "source_date_epoch": SOURCE_EPOCH,
        },
        "operator": {
            "type": "User",
            "id": 2001,
            "login": "release-operator",
        },
        "independent_reviewer": {
            "type": "User",
            "id": 2002,
            "login": "release-reviewer",
        },
        "frozen_at_utc": "2026-09-25T00:00:00Z",
        "evidence_completed_at_utc": "2026-09-26T02:00:00Z",
    }


def receipt_candidate() -> dict[str, Any]:
    return {
        "version": VERSION,
        "source_repository": REPOSITORY,
        "source_tag": SOURCE_TAG,
        "source_commit": SOURCE_COMMIT,
        "source_tree": SOURCE_TREE,
    }


def checks() -> list[dict[str, str]]:
    return [{"name": "required validation", "status": "pass"}]


def details(kind: str, gate: str, artifacts: dict[str, Any]) -> dict[str, Any]:
    name = gate.rsplit(".", 1)[-1]
    if kind == "repository":
        value = {
            "clean_checkout": True,
            "serial_test_passes": {
                "linux": {
                    "passes": 2,
                    "workflow_run_url": (
                        "https://github.com/openkedge/hardknock/actions/runs/1"
                    ),
                },
                "macos": {
                    "passes": 2,
                    "workflow_run_url": (
                        "https://github.com/openkedge/hardknock/actions/runs/1"
                    ),
                },
            },
            "package_verified": True,
            "default_branch": "main",
            "default_branch_protected": True,
            "required_approving_reviews": 2,
            "force_push_blocked": True,
            "deletion_blocked": True,
            "admin_enforcement": True,
            "bypass_actor_count": 0,
        }
    elif kind == "release_controls":
        value = {
            "candidate_tag": SOURCE_TAG,
            "stable_tag": TAG,
            "candidate_tag_ruleset_id": 101,
            "candidate_tag_ruleset_name": "immutable release candidates",
            "candidate_tag_update_blocked": True,
            "candidate_tag_deletion_blocked": True,
            "candidate_tag_bypass_actor_count": 0,
            "stable_tag_ruleset_id": 102,
            "stable_tag_ruleset_name": "immutable stable releases",
            "stable_tag_update_blocked": True,
            "stable_tag_deletion_blocked": True,
            "stable_tag_bypass_actor_count": 0,
            "candidate_environment": "hardknock-candidate-publication",
            "candidate_environment_protected": True,
            "candidate_environment_deployment_branch_protected": True,
            "candidate_environment_default_branch_only": True,
            "candidate_environment_prevent_self_review": True,
            "candidate_environment_required_reviewers": [
                {"type": "User", "id": 1001, "login": "candidate-reviewer"}
            ],
            "stable_environment": "hardknock-stable-publication",
            "stable_environment_protected": True,
            "stable_environment_deployment_branch_protected": True,
            "stable_environment_default_branch_only": True,
            "stable_environment_prevent_self_review": True,
            "stable_environment_required_reviewers": [
                {"type": "User", "id": 1002, "login": "stable-reviewer"}
            ],
            "reviewer_user_ids_disjoint": True,
        }
    elif kind == "published_metadata":
        value = {"release_tag": SOURCE_TAG, "release_immutable": True, "assets": copy.deepcopy(artifacts["metadata"])}
    elif kind == "published_artifact":
        value = {"target": name, "release_tag": SOURCE_TAG, "release_immutable": True, **copy.deepcopy(artifacts["targets"][name])}
    elif kind == "hosted_matrix":
        value = {
            "target": name,
            **HOSTED_MATRIX[name],
            "workflow_run_url": "https://github.com/openkedge/hardknock/actions/runs/1",
        }
    elif kind == "native_service":
        value = {
            "manager": "systemd-user" if name == "systemd_user" else "launchd",
            "platform": "linux" if name == "systemd_user" else "macos",
            "architecture": "x86_64" if name == "systemd_user" else "aarch64",
            "restart_verified": True,
            "bounded_shutdown": True,
            "logs_verified": True,
            "upgrade_verified": True,
            "uninstall_verified": True,
        }
    elif kind == "live_agent":
        value = {"agent": name, "agent_version": "test-version", "lifecycle_verified": True}
    elif kind == "container_security":
        value = {"runtime": "docker" if name == "rootless_docker" else "podman", "runtime_version": "test-version", "rootless": True, "image_digest": "sha256:" + "3" * 64}
    elif kind == "recovery_drill":
        value = {"drill": name, "rollback_or_restore_verified": True}
    elif kind == "soak":
        value = {"platform": "linux" if name == "linux_24h" else "macos", "duration_seconds": 86400, "probes_checked": True, "failures": 0}
    elif kind == "dependency_advisories":
        value = {"database_updated_at_utc": "2026-09-26T00:00:00Z", "critical": 0, "high": 0}
    elif kind == "security_support":
        value = {"security_contact": "security@example.invalid", "support_contact": "support@example.invalid", "published": True}
    elif kind == "compatibility_policy":
        value = {"policy_version": "1", "published": True}
    elif kind == "bootstrap_interruption":
        value = {"shells": ["/bin/sh", "/bin/dash"], "scenarios": ["fresh_install", "upgrade", "profile_mutation", "uninstall"], "recovered": True}
    elif kind == "repository_release_immutability":
        value = {"source_release_tag": SOURCE_TAG, "source_release_verified": True, "verification_command": "gh release verify"}
    else:
        raise AssertionError(kind)
    return {"checks": checks(), **value}


def gate_specs() -> list[tuple[str, str]]:
    specs = [
        ("gates.repository", "repository"),
        ("gates.release_controls", "release_controls"),
        ("gates.published_artifacts.metadata", "published_metadata"),
        ("gates.dependency_advisories", "dependency_advisories"),
        ("gates.security_support", "security_support"),
        ("gates.compatibility_policy", "compatibility_policy"),
        ("gates.bootstrap_interruption", "bootstrap_interruption"),
        ("gates.repository_release_immutability", "repository_release_immutability"),
    ]
    specs += [(f"gates.published_artifacts.{target}", "published_artifact") for target in TARGETS]
    specs += [(f"gates.hosted_matrix.{target}", "hosted_matrix") for target in TARGETS]
    specs += [(f"gates.native_services.{name}", "native_service") for name in ("systemd_user", "launchd")]
    specs += [(f"gates.live_agents.{name}", "live_agent") for name in ("generic_mcp", "claude_code", "codex", "second_agent_transfer")]
    specs += [(f"gates.container_security.{name}", "container_security") for name in ("rootless_docker", "rootless_podman")]
    specs += [(f"gates.recovery_drills.{name}", "recovery_drill") for name in ("interrupted_setup", "failed_upgrade_restore", "corrupted_database", "full_disk_or_quota", "killed_bridge", "failed_uninstall", "signing_compromise")]
    specs += [("gates.soak.linux_24h", "soak"), ("gates.soak.macos_24h", "soak")]
    return specs


def gate_at(record: dict[str, Any], gate: str) -> dict[str, Any]:
    value: Any = record
    for component in gate.split("."):
        value = value[component]
    assert isinstance(value, dict)
    return value


def make_passing(evidence_root: Path, asset_root: Path) -> dict[str, Any]:
    artifacts = create_assets(asset_root)
    record = json.loads(TEMPLATE.read_text())
    record["candidate"] = candidate()
    record["artifacts"] = artifacts
    record["decision"] = "promote"
    receipts = evidence_root / "receipts"
    receipts.mkdir()
    for gate, kind in gate_specs():
        receipt = {
            "schema": "hardknock-release-gate-receipt-v1",
            "gate": gate,
            "kind": kind,
            "candidate": receipt_candidate(),
            "status": "pass",
            "observed_at_utc": "2026-09-26T01:00:00Z",
            "producer": "validation-system",
            "details": details(kind, gate, artifacts),
        }
        path = receipts / f"{gate.removeprefix('gates.').replace('.', '-')}.json"
        path.write_text(json.dumps(receipt, sort_keys=True))
        gate_at(record, gate).update({
            "status": "pass",
            "evidence": [{"path": path.relative_to(evidence_root).as_posix(), "sha256": sha256(path), "kind": kind}],
        })
    return record


def write_record(path: Path, record: dict[str, Any]) -> None:
    path.write_text(json.dumps(record, sort_keys=True))


def assert_rejected(
    evidence_root: Path,
    asset_root: Path,
    passing: dict[str, Any],
    name: str,
    mutate: Callable[[dict[str, Any]], None],
    expected: str,
    *,
    schema_must_reject: bool = False,
    schema_must_accept: bool = False,
) -> None:
    assert not (schema_must_reject and schema_must_accept)
    receipt_contents = {
        path: path.read_bytes()
        for path in (evidence_root / "receipts").iterdir()
        if path.is_file()
    }
    record = copy.deepcopy(passing)
    try:
        mutate(record)
        path = evidence_root / f"{name}.json"
        write_record(path, record)
        if schema_must_reject or schema_must_accept:
            schema = json.loads(SCHEMA.read_text())
            errors = schema_document_errors(record, evidence_root, schema)
            if schema_must_reject:
                assert errors, f"{name} unexpectedly passed the JSON schema"
            else:
                assert not errors, f"{name} unexpectedly failed schema: {errors}"
        process, result = run(path, evidence_root, asset_root=asset_root)
        assert process.returncode == 1, f"{name} unexpectedly passed"
        assert any(expected in blocker for blocker in result["blockers"]), result
    finally:
        for path, contents in receipt_contents.items():
            path.write_bytes(contents)


def assert_accepted(
    evidence_root: Path,
    asset_root: Path,
    passing: dict[str, Any],
    name: str,
    mutate: Callable[[dict[str, Any]], None],
) -> None:
    receipt_contents = {
        path: path.read_bytes()
        for path in (evidence_root / "receipts").iterdir()
        if path.is_file()
    }
    record = copy.deepcopy(passing)
    try:
        mutate(record)
        path = evidence_root / f"{name}.json"
        write_record(path, record)
        schema = json.loads(SCHEMA.read_text())
        errors = schema_document_errors(record, evidence_root, schema)
        assert not errors, f"{name} unexpectedly failed schema: {errors}"
        process, result = run(path, evidence_root, asset_root=asset_root)
        assert process.returncode == 0, result
        assert result["ok"] is True
    finally:
        for path, contents in receipt_contents.items():
            path.write_bytes(contents)


def validate_schema_contract(schema: dict[str, Any]) -> None:
    assert schema["additionalProperties"] is False
    assert "artifacts" in schema["required"]
    assert set(schema["$defs"]["evidence_reference"]["required"]) == {"path", "sha256", "kind"}
    assert schema["$defs"]["gate_receipt"]["properties"]["schema"]["const"] == "hardknock-release-gate-receipt-v1"
    source_required = set(schema["$defs"]["source"]["required"])
    assert source_required == {"repository", "tag", "commit", "tree", "source_date_epoch"}
    candidate_required = set(schema["$defs"]["candidate"]["required"])
    assert {
        "operator",
        "independent_reviewer",
        "frozen_at_utc",
        "evidence_completed_at_utc",
    } <= candidate_required
    assert (
        schema["$defs"]["candidate"]["properties"]["operator"]["$ref"]
        == "#/$defs/github_user_identity"
    )
    assert (
        schema["$defs"]["candidate"]["properties"]["independent_reviewer"]["$ref"]
        == "#/$defs/github_user_identity"
    )
    published = schema["properties"]["gates"]["properties"]["published_artifacts"]
    assert "metadata" in published["required"]
    assert "release_controls" in schema["properties"]["gates"]["required"]
    assert (
        schema["$defs"]["gate_repository"]["allOf"][0]["then"]["properties"][
            "evidence"
        ]["minItems"]
        == 1
    )
    assert (
        schema["$defs"]["metadata_artifacts"]["properties"]["installer"]["allOf"][
            1
        ]["properties"]["name"]["const"]
        == "install-hardknock"
    )
    assert (
        "release_controls"
        in schema["$defs"]["evidence_reference"]["properties"]["kind"]["enum"]
    )
    release_control_fields = set(
        schema["$defs"]["release_controls_details"]["required"]
    )
    assert {
        "candidate_tag_ruleset_id",
        "stable_tag_ruleset_id",
        "candidate_environment_required_reviewers",
        "stable_environment_required_reviewers",
        "reviewer_user_ids_disjoint",
    } <= release_control_fields
    identity = schema["$defs"]["github_user_identity"]
    assert identity["properties"]["type"]["const"] == "User"
    assert identity["properties"]["id"]["minimum"] == 1
    assert identity["properties"]["login"]["pattern"].startswith("^[a-z0-9]")
    reviewer = schema["$defs"]["environment_user_reviewer"]
    assert reviewer["$ref"] == "#/$defs/github_user_identity"
    hosted = schema["$defs"]["hosted_matrix_details"]
    assert len(hosted["allOf"]) == len(TARGETS)
    assert set(hosted["properties"]["runner_image"]["enum"]) == {
        value["runner_image"] for value in HOSTED_MATRIX.values()
    }
    native = schema["$defs"]["native_service_details"]
    assert "architecture" not in native["required"]
    assert len(native["allOf"]) == 2
    repository_fields = set(schema["$defs"]["repository_details"]["required"])
    assert {
        "required_approving_reviews",
        "force_push_blocked",
        "bypass_actor_count",
    } <= repository_fields
    serial = schema["$defs"]["repository_details"]["properties"][
        "serial_test_passes"
    ]
    assert set(serial["required"]) == {"linux", "macos"}
    assert serial["additionalProperties"] is False
    assert serial["properties"]["linux"]["$ref"] == (
        "#/$defs/serial_test_pass_result"
    )
    serial_result = schema["$defs"]["serial_test_pass_result"]
    assert set(serial_result["required"]) == {"passes", "workflow_run_url"}
    assert serial_result["properties"]["passes"]["minimum"] == 2
    assert serial_result["properties"]["workflow_run_url"]["pattern"].startswith(
        "^https://github"
    )
    assert "(?!0{64}" in schema["$defs"]["artifact"]["properties"]["sha256"]["pattern"]
    assert "(?!0{40}" in schema["$defs"]["source"]["properties"]["commit"]["pattern"]
    assert "-rc\\." in schema["$defs"]["source"]["properties"]["tag"]["pattern"]


def main() -> None:
    schema = json.loads(SCHEMA.read_text())
    validate_schema_contract(schema)
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        evidence_root = root / "evidence"
        asset_root = root / "assets"
        evidence_root.mkdir()
        asset_root.mkdir()
        passing = make_passing(evidence_root, asset_root)
        record_path = evidence_root / "record.json"
        write_record(record_path, passing)
        agreement_errors = schema_document_errors(passing, evidence_root, schema)
        assert not agreement_errors, agreement_errors

        process, result = run(record_path, evidence_root, asset_root=asset_root)
        assert process.returncode == 0, result
        assert result["ok"] is True
        assert result["candidate"]["tag"] == TAG
        assert result["candidate"]["source"]["tag"] == SOURCE_TAG
        assert result["candidate"]["source"]["commit"] == SOURCE_COMMIT

        mismatch_process, mismatch = run(
            record_path, evidence_root, asset_root=asset_root, source_commit="9" * 40
        )
        assert mismatch_process.returncode == 1
        assert any("candidate.source.commit: expected" in item for item in mismatch["blockers"])

        def replace_with_arbitrary(record: dict[str, Any]) -> None:
            reference = gate_at(record, "gates.repository")["evidence"][0]
            path = evidence_root / reference["path"]
            path.write_text('{"result":"pass"}\n')
            reference["sha256"] = sha256(path)

        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "arbitrary-receipt",
            replace_with_arbitrary,
            "missing field",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "wrong-reference-kind",
            lambda record: gate_at(record, "gates.repository")["evidence"][
                0
            ].update({"kind": "soak"}),
            "kind: expected",
            schema_must_reject=True,
        )

        def wrong_receipt_candidate(record: dict[str, Any]) -> None:
            reference = gate_at(record, "gates.security_support")["evidence"][0]
            path = evidence_root / reference["path"]
            receipt = json.loads(path.read_text())
            receipt["candidate"]["source_commit"] = "8" * 40
            path.write_text(json.dumps(receipt))
            reference["sha256"] = sha256(path)

        assert_rejected(evidence_root, asset_root, passing, "wrong-receipt-candidate", wrong_receipt_candidate, "does not match promoted candidate")

        def wrong_target(record: dict[str, Any]) -> None:
            gate = f"gates.published_artifacts.{TARGETS[0]}"
            reference = gate_at(record, gate)["evidence"][0]
            path = evidence_root / reference["path"]
            receipt = json.loads(path.read_text())
            receipt["details"]["target"] = TARGETS[1]
            path.write_text(json.dumps(receipt))
            reference["sha256"] = sha256(path)

        assert_rejected(evidence_root, asset_root, passing, "wrong-target", wrong_target, f"expected '{TARGETS[0]}'")
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "missing-metadata-gate",
            lambda record: record["gates"]["published_artifacts"].pop("metadata"),
            "metadata: missing field",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "empty-passing-evidence",
            lambda record: gate_at(record, "gates.repository").update(
                {"evidence": []}
            ),
            "passing gate has no evidence",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "noncanonical-artifact-name",
            lambda record: record["artifacts"]["metadata"]["installer"].update(
                {"name": "installer"}
            ),
            "expected 'install-hardknock'",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "placeholder-artifact-digest",
            lambda record: record["artifacts"]["metadata"]["installer"].update(
                {"sha256": "0" * 64}
            ),
            "unresolved placeholder digest",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "placeholder-source-object",
            lambda record: record["candidate"]["source"].update(
                {"commit": "0" * 40}
            ),
            "unresolved placeholder object ID",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "invalid-candidate-source-tag",
            lambda record: record["candidate"]["source"].update(
                {"tag": "v1.0.0-rc.01"}
            ),
            "expected immutable v<version>-rc.N",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "free-form-candidate-operator",
            lambda record: record["candidate"].update(
                {"operator": "release-operator"}
            ),
            "candidate.operator: expected an object",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "nonpositive-candidate-operator-id",
            lambda record: record["candidate"]["operator"].update({"id": 0}),
            "candidate.operator.id: must be a positive GitHub user ID",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "nonnormalized-candidate-operator-login",
            lambda record: record["candidate"]["operator"].update(
                {"login": "Release-Operator"}
            ),
            "canonical lowercase GitHub user login",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "same-candidate-user-id",
            lambda record: record["candidate"]["independent_reviewer"].update(
                {"id": record["candidate"]["operator"]["id"]}
            ),
            "operator and independent reviewer GitHub user IDs must differ",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "same-candidate-normalized-login",
            lambda record: record["candidate"]["independent_reviewer"].update(
                {"login": record["candidate"]["operator"]["login"]}
            ),
            "operator and independent reviewer normalized logins must differ",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "old-candidate-freeze",
            lambda record: record["candidate"].update(
                {"frozen_at_utc": "2000-01-01T00:00:00Z"}
            ),
            "precedes exact source commit timestamp",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "late-candidate-freeze",
            lambda record: record["candidate"]["source"].update(
                {"source_date_epoch": SOURCE_EPOCH - 4 * 24 * 60 * 60}
            ),
            "exceeds the 72-hour source-to-freeze window",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "completion-before-freeze",
            lambda record: record["candidate"].update(
                {"evidence_completed_at_utc": "2026-09-24T23:59:59Z"}
            ),
            "evidence_completed_at_utc: precedes candidate.frozen_at_utc",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "evidence-window-too-long",
            lambda record: (
                record["candidate"]["source"].update(
                    {"source_date_epoch": SOURCE_EPOCH - 25 * 24 * 60 * 60}
                ),
                record["candidate"].update(
                    {
                        "frozen_at_utc": "2026-09-01T00:00:00Z",
                        "evidence_completed_at_utc": "2026-09-16T00:00:01Z",
                    }
                ),
            ),
            "exceeds the 14-day evidence window",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "invalid-evidence-completion-time",
            lambda record: record["candidate"].update(
                {"evidence_completed_at_utc": "not-a-time"}
            ),
            "candidate.evidence_completed_at_utc: invalid",
            schema_must_reject=True,
        )

        def mutate_receipt(
            record: dict[str, Any], gate: str, field: str, value: Any
        ) -> None:
            reference = gate_at(record, gate)["evidence"][0]
            path = evidence_root / reference["path"]
            receipt = json.loads(path.read_text())
            receipt["details"][field] = value
            path.write_text(json.dumps(receipt, sort_keys=True))
            reference["sha256"] = sha256(path)

        def mutate_receipt_root(
            record: dict[str, Any], gate: str, field: str, value: Any
        ) -> None:
            reference = gate_at(record, gate)["evidence"][0]
            path = evidence_root / reference["path"]
            receipt = json.loads(path.read_text())
            receipt[field] = value
            path.write_text(json.dumps(receipt, sort_keys=True))
            reference["sha256"] = sha256(path)

        def mutate_receipt_fields(
            record: dict[str, Any], gate: str, values: dict[str, Any]
        ) -> None:
            reference = gate_at(record, gate)["evidence"][0]
            path = evidence_root / reference["path"]
            receipt = json.loads(path.read_text())
            receipt["details"].update(values)
            path.write_text(json.dumps(receipt, sort_keys=True))
            reference["sha256"] = sha256(path)

        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "old-receipt-observation",
            lambda record: mutate_receipt_root(
                record,
                "gates.repository",
                "observed_at_utc",
                "2000-01-01T00:00:00Z",
            ),
            "observed_at_utc: precedes candidate.frozen_at_utc",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "future-skewed-receipt-observation",
            lambda record: mutate_receipt_root(
                record,
                "gates.repository",
                "observed_at_utc",
                "2026-09-26T02:00:01Z",
            ),
            "observed_at_utc: exceeds candidate.evidence_completed_at_utc",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "invalid-receipt-observation",
            lambda record: mutate_receipt_root(
                record,
                "gates.repository",
                "observed_at_utc",
                "2000",
            ),
            "observed_at_utc: invalid",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "repository-bypass-actor",
            lambda record: mutate_receipt(
                record, "gates.repository", "bypass_actor_count", False
            ),
            "bypass_actor_count: must be integer zero",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "legacy-serial-pass-scalar",
            lambda record: mutate_receipt(
                record, "gates.repository", "serial_test_passes", 2
            ),
            "must contain exactly linux and macos",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "missing-macos-serial-pass",
            lambda record: mutate_receipt(
                record,
                "gates.repository",
                "serial_test_passes",
                {
                    "linux": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    }
                },
            ),
            "must contain exactly linux and macos",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "one-linux-serial-pass",
            lambda record: mutate_receipt(
                record,
                "gates.repository",
                "serial_test_passes",
                {
                    "linux": {
                        "passes": 1,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                    "macos": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                },
            ),
            "linux.passes: must be an integer from 2 through 16",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "extra-windows-serial-pass",
            lambda record: mutate_receipt(
                record,
                "gates.repository",
                "serial_test_passes",
                {
                    "linux": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                    "macos": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                    "windows": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                },
            ),
            "must contain exactly linux and macos",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "invalid-serial-workflow-url",
            lambda record: mutate_receipt(
                record,
                "gates.repository",
                "serial_test_passes",
                {
                    "linux": {
                        "passes": 2,
                        "workflow_run_url": "https://example.invalid/run/1",
                    },
                    "macos": {
                        "passes": 2,
                        "workflow_run_url": (
                            "https://github.com/openkedge/hardknock/"
                            "actions/runs/1"
                        ),
                    },
                },
            ),
            "must be an exact Hardknock GitHub Actions run URL",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "advisory-database-before-freeze",
            lambda record: mutate_receipt(
                record,
                "gates.dependency_advisories",
                "database_updated_at_utc",
                "2026-09-24T23:59:59Z",
            ),
            "precedes candidate.frozen_at_utc",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "advisory-database-after-observation",
            lambda record: mutate_receipt(
                record,
                "gates.dependency_advisories",
                "database_updated_at_utc",
                "2026-09-26T01:00:01Z",
            ),
            "exceeds receipt observation time",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "stale-advisory-database",
            lambda record: (
                mutate_receipt_root(
                    record,
                    "gates.dependency_advisories",
                    "observed_at_utc",
                    "2026-09-26T00:00:01Z",
                ),
                mutate_receipt(
                    record,
                    "gates.dependency_advisories",
                    "database_updated_at_utc",
                    "2026-09-25T00:00:00Z",
                ),
            ),
            "advisory database is more than 24 hours old",
            schema_must_accept=True,
        )
        assert_accepted(
            evidence_root,
            asset_root,
            passing,
            "advisory-database-exactly-24-hours-old",
            lambda record: (
                mutate_receipt_root(
                    record,
                    "gates.dependency_advisories",
                    "observed_at_utc",
                    "2026-09-26T00:00:00Z",
                ),
                mutate_receipt(
                    record,
                    "gates.dependency_advisories",
                    "database_updated_at_utc",
                    "2026-09-25T00:00:00Z",
                ),
            ),
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "unprotected-candidate-environment",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "candidate_environment_protected",
                False,
            ),
            "candidate_environment_protected: must be true",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "mutable-candidate-tag-ruleset",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "candidate_tag_update_blocked",
                False,
            ),
            "candidate_tag_update_blocked: must be true",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "stable-environment-self-review",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "stable_environment_prevent_self_review",
                False,
            ),
            "stable_environment_prevent_self_review: must be true",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "empty-stable-environment-reviewer",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "stable_environment_required_reviewers",
                [],
            ),
            "stable_environment_required_reviewers: expected 1 to 6",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "team-environment-reviewer",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "candidate_environment_required_reviewers",
                [{"type": "Team", "id": 2001, "login": "release-team"}],
            ),
            "team reviewers are forbidden",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "overlapping-environment-reviewer-id",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "stable_environment_required_reviewers",
                [{"type": "User", "id": 1001, "login": "stable-reviewer"}],
            ),
            "reviewer user IDs must be disjoint",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "overlapping-environment-normalized-login",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "stable_environment_required_reviewers",
                [{"type": "User", "id": 1002, "login": "candidate-reviewer"}],
            ),
            "one normalized GitHub login maps to reviewer IDs in both environments",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "nonnormalized-environment-reviewer-login",
            lambda record: mutate_receipt(
                record,
                "gates.release_controls",
                "stable_environment_required_reviewers",
                [{"type": "User", "id": 1002, "login": "Stable-Reviewer"}],
            ),
            "canonical lowercase GitHub user login",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "operator-is-environment-reviewer",
            lambda record: record["candidate"]["operator"].update({"id": 1001}),
            "must differ from candidate operator",
            schema_must_accept=True,
        )

        first_target = TARGETS[0]
        second_target = TARGETS[1]
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "hosted-matrix-wrong-gate",
            lambda record: mutate_receipt_fields(
                record,
                f"gates.hosted_matrix.{first_target}",
                {"target": second_target, **HOSTED_MATRIX[second_target]},
            ),
            f"target: expected {first_target!r}",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "hosted-matrix-self-hosted",
            lambda record: mutate_receipt(
                record,
                f"gates.hosted_matrix.{first_target}",
                "runner_image",
                "self-hosted",
            ),
            "runner_image: expected 'ubuntu-24.04'",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "hosted-matrix-windows",
            lambda record: mutate_receipt(
                record,
                f"gates.hosted_matrix.{first_target}",
                "operating_system",
                "windows",
            ),
            "operating_system: expected 'linux'",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "hosted-matrix-s390x",
            lambda record: mutate_receipt(
                record,
                f"gates.hosted_matrix.{first_target}",
                "architecture",
                "s390x",
            ),
            "architecture: expected 'x86_64'",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "hosted-matrix-arbitrary-combination",
            lambda record: mutate_receipt_fields(
                record,
                f"gates.hosted_matrix.{first_target}",
                {
                    "runner_image": "macos-15-intel",
                    "operating_system": "macos",
                },
            ),
            "runner_image: expected 'ubuntu-24.04'",
            schema_must_reject=True,
        )

        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "native-service-wrong-gate",
            lambda record: mutate_receipt_fields(
                record,
                "gates.native_services.systemd_user",
                {
                    "manager": "launchd",
                    "platform": "macos",
                    "architecture": "aarch64",
                },
            ),
            "manager: expected 'systemd-user'",
            schema_must_accept=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "native-service-platform-mismatch",
            lambda record: mutate_receipt(
                record,
                "gates.native_services.systemd_user",
                "platform",
                "macos",
            ),
            "platform: expected 'linux'",
            schema_must_reject=True,
        )
        assert_rejected(
            evidence_root,
            asset_root,
            passing,
            "native-service-s390x",
            lambda record: mutate_receipt(
                record,
                "gates.native_services.systemd_user",
                "architecture",
                "s390x",
            ),
            "architecture: expected one of",
            schema_must_reject=True,
        )

        tampered = asset_root / passing["artifacts"]["metadata"]["installer"]["name"]
        original = tampered.read_bytes()
        tampered.write_bytes(b"tampered")
        tamper_process, tamper_result = run(record_path, evidence_root, asset_root=asset_root)
        assert tamper_process.returncode == 1
        assert any("SHA-256 mismatch" in item for item in tamper_result["blockers"])
        tampered.write_bytes(original)

        extra = asset_root / "unexpected"
        extra.write_text("unexpected")
        extra_process, extra_result = run(record_path, evidence_root, asset_root=asset_root)
        assert extra_process.returncode == 1
        assert any("unexpected assets" in item for item in extra_result["blockers"])
        extra.unlink()

        reference = gate_at(passing, "gates.repository")["evidence"][0]
        real = evidence_root / reference["path"]
        link = evidence_root / "receipt-link.json"
        os.symlink(real, link)
        linked = copy.deepcopy(passing)
        gate_at(linked, "gates.repository")["evidence"][0]["path"] = link.name
        linked_path = evidence_root / "linked.json"
        write_record(linked_path, linked)
        linked_process, linked_result = run(linked_path, evidence_root, asset_root=asset_root)
        assert linked_process.returncode == 1
        assert any("non-symlink" in item for item in linked_result["blockers"])

    print("release evidence verification tests passed")


if __name__ == "__main__":
    main()
