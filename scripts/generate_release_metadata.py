#!/usr/bin/env python3
"""Generate deterministic release SBOM and license inventory documents."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from typing import Any
from urllib.parse import quote

if sys.version_info < (3, 11):
    raise SystemExit("generate_release_metadata.py requires Python 3.11 or newer")

import tomllib


CYCLONEDX_SPEC_VERSION = "1.7"
SBOM_FILENAME = "sbom.cdx.json"
LICENSE_INVENTORY_FILENAME = "third-party-licenses.json"
LICENSE_INVENTORY_FORMAT_VERSION = 1
CHECKSUM_PATTERN = re.compile(r"[0-9a-fA-F]{64}")
LEGACY_LICENSE_PATTERN = re.compile(
    r"[A-Za-z0-9.+-]+(?:/[A-Za-z0-9.+-]+)+"
)


class ReleaseMetadataError(ValueError):
    """Raised when Cargo inputs are incomplete or internally inconsistent."""


def _require_mapping(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReleaseMetadataError(f"{field} must be an object")
    return value


def _require_list(value: Any, field: str) -> list[Any]:
    if not isinstance(value, list):
        raise ReleaseMetadataError(f"{field} must be an array")
    return value


def _require_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ReleaseMetadataError(f"{field} must be a non-empty string")
    return value


def _optional_string(value: Any, field: str) -> str | None:
    if value is None:
        return None
    return _require_string(value, field)


def _load_json(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ReleaseMetadataError(f"cannot read Cargo metadata {path}: {error}") from error
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReleaseMetadataError(f"invalid Cargo metadata JSON {path}: {error}") from error
    return _require_mapping(value, "Cargo metadata")


def _load_lockfile(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ReleaseMetadataError(f"cannot read Cargo.lock {path}: {error}") from error
    try:
        value = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ReleaseMetadataError(f"invalid Cargo.lock TOML {path}: {error}") from error
    return _require_mapping(value, "Cargo.lock")


def _normalize_license_file(package: dict[str, Any], license_file: str | None) -> str | None:
    if license_file is None:
        return None

    license_path = Path(license_file)
    if not license_path.is_absolute():
        return license_path.as_posix()

    manifest_path = package.get("manifest_path")
    if isinstance(manifest_path, str) and manifest_path:
        try:
            return license_path.relative_to(Path(manifest_path).parent).as_posix()
        except ValueError:
            pass
    return license_path.name


def _local_source_identity(package: dict[str, Any], workspace_root: str | None) -> str:
    manifest_path = package["manifest_path"]
    package_dir = Path(manifest_path).parent
    if workspace_root:
        try:
            relative = package_dir.relative_to(Path(workspace_root))
            relative_text = relative.as_posix() or "."
            return f"workspace:{relative_text}"
        except ValueError:
            try:
                relative = Path(os.path.relpath(package_dir, workspace_root))
                return f"path:{relative.as_posix()}"
            except ValueError:
                pass
    return f"path:{package_dir.as_posix()}"


def _source_identity(package: dict[str, Any], workspace_root: str | None) -> str:
    source = package.get("source")
    if source is None:
        return _local_source_identity(package, workspace_root)
    return _require_string(source, f"package {package.get('name', '<unknown>')} source")


def _package_key(package: dict[str, Any]) -> tuple[str, str, str | None]:
    name = _require_string(package.get("name"), "package name")
    version = _require_string(package.get("version"), f"package {name} version")
    source = package.get("source")
    if source is not None:
        source = _require_string(source, f"package {name} source")
    return name, version, source


def _package_url(name: str, version: str) -> str:
    return f"pkg:cargo/{quote(name, safe='')}@{quote(version, safe='')}"


def _bom_ref(name: str, version: str, source_identity: str) -> str:
    identity = "\0".join((name, version, source_identity)).encode("utf-8")
    return f"urn:cdx:cargo:{hashlib.sha256(identity).hexdigest()}"


def _lock_index(lockfile: dict[str, Any]) -> dict[tuple[str, str, str | None], dict[str, Any]]:
    packages = _require_list(lockfile.get("package"), "Cargo.lock package")
    index: dict[tuple[str, str, str | None], dict[str, Any]] = {}
    for position, raw_package in enumerate(packages):
        package = _require_mapping(raw_package, f"Cargo.lock package[{position}]")
        key = _package_key(package)
        if key in index:
            name, version, source = key
            raise ReleaseMetadataError(
                f"Cargo.lock contains duplicate package {name} {version} from {source or 'path'}"
            )

        checksum = package.get("checksum")
        if checksum is not None:
            checksum = _require_string(
                checksum, f"Cargo.lock package {key[0]} {key[1]} checksum"
            )
            if CHECKSUM_PATTERN.fullmatch(checksum) is None:
                raise ReleaseMetadataError(
                    f"Cargo.lock package {key[0]} {key[1]} has an invalid SHA-256 checksum"
                )
            package = dict(package)
            package["checksum"] = checksum.lower()
        index[key] = package
    return index


def _resolved_packages(
    metadata: dict[str, Any],
    lockfile: dict[str, Any],
) -> tuple[
    dict[str, dict[str, Any]],
    dict[str, dict[str, Any]],
    str,
    set[str],
    dict[str, str],
    dict[str, str | None],
]:
    if metadata.get("version") != 1:
        raise ReleaseMetadataError("Cargo metadata version must be 1")

    raw_packages = _require_list(metadata.get("packages"), "Cargo metadata packages")
    packages_by_id: dict[str, dict[str, Any]] = {}
    for position, raw_package in enumerate(raw_packages):
        package = _require_mapping(raw_package, f"Cargo metadata packages[{position}]")
        package_id = _require_string(package.get("id"), f"package[{position}] id")
        name, version, _ = _package_key(package)
        _require_string(
            package.get("manifest_path"), f"package {name} {version} manifest_path"
        )
        if package_id in packages_by_id:
            raise ReleaseMetadataError(f"duplicate Cargo metadata package id {package_id}")

        license_expression = _optional_string(
            package.get("license"), f"package {name} {version} license"
        )
        license_file = _optional_string(
            package.get("license_file"), f"package {name} {version} license_file"
        )
        if license_expression is None and license_file is None:
            raise ReleaseMetadataError(
                f"package {name} {version} must declare license or license_file"
            )
        packages_by_id[package_id] = package

    resolve = _require_mapping(metadata.get("resolve"), "Cargo metadata resolve")
    root_id = _require_string(resolve.get("root"), "Cargo metadata resolve.root")
    if root_id not in packages_by_id:
        raise ReleaseMetadataError(f"Cargo metadata root package {root_id} is missing")

    raw_nodes = _require_list(resolve.get("nodes"), "Cargo metadata resolve.nodes")
    nodes_by_id: dict[str, dict[str, Any]] = {}
    for position, raw_node in enumerate(raw_nodes):
        node = _require_mapping(raw_node, f"Cargo metadata resolve.nodes[{position}]")
        node_id = _require_string(node.get("id"), f"resolve node[{position}] id")
        if node_id not in packages_by_id:
            raise ReleaseMetadataError(f"resolve node references unknown package {node_id}")
        if node_id in nodes_by_id:
            raise ReleaseMetadataError(f"duplicate resolve node {node_id}")
        dependencies = _require_list(
            node.get("dependencies"), f"resolve node {node_id} dependencies"
        )
        for dependency_id in dependencies:
            dependency_id = _require_string(
                dependency_id, f"resolve node {node_id} dependency id"
            )
            if dependency_id not in packages_by_id:
                raise ReleaseMetadataError(
                    f"resolve node {node_id} references unknown dependency {dependency_id}"
                )
        nodes_by_id[node_id] = node

    reachable: set[str] = set()
    pending = [root_id]
    while pending:
        package_id = pending.pop()
        if package_id in reachable:
            continue
        node = nodes_by_id.get(package_id)
        if node is None:
            raise ReleaseMetadataError(f"resolved package {package_id} has no resolve node")
        reachable.add(package_id)
        pending.extend(node["dependencies"])

    lock_packages = _lock_index(lockfile)
    checksums: dict[str, str | None] = {}
    source_identities: dict[str, str] = {}
    workspace_root = metadata.get("workspace_root")
    if workspace_root is not None:
        workspace_root = _require_string(workspace_root, "Cargo metadata workspace_root")

    for package_id in reachable:
        package = packages_by_id[package_id]
        name, version, source = _package_key(package)
        locked = lock_packages.get((name, version, source))
        if locked is None:
            raise ReleaseMetadataError(
                f"resolved package {name} {version} from {source or 'path'} is absent from Cargo.lock"
            )
        checksums[package_id] = locked.get("checksum")
        source_identities[package_id] = _source_identity(package, workspace_root)

    raw_workspace_members = _require_list(
        metadata.get("workspace_members"), "Cargo metadata workspace_members"
    )
    workspace_members: set[str] = set()
    for member in raw_workspace_members:
        member_id = _require_string(member, "Cargo metadata workspace member")
        if member_id not in packages_by_id:
            raise ReleaseMetadataError(f"workspace member {member_id} is not a package")
        workspace_members.add(member_id)

    packages_by_id = {
        package_id: packages_by_id[package_id]
        for package_id in reachable
    }
    nodes_by_id = {
        package_id: nodes_by_id[package_id]
        for package_id in reachable
    }
    return (
        packages_by_id,
        nodes_by_id,
        root_id,
        workspace_members,
        source_identities,
        checksums,
    )


def _package_sort_key(
    package_id: str,
    packages_by_id: dict[str, dict[str, Any]],
    source_identities: dict[str, str],
) -> tuple[str, str, str]:
    package = packages_by_id[package_id]
    return (
        package["name"],
        package["version"],
        source_identities[package_id],
    )


def _license_fields(package: dict[str, Any]) -> tuple[str | None, str | None]:
    expression = package.get("license")
    if expression is not None:
        expression = _require_string(
            expression, f"package {package['name']} {package['version']} license"
        )
    license_file = package.get("license_file")
    if license_file is not None:
        license_file = _require_string(
            license_file,
            f"package {package['name']} {package['version']} license_file",
        )
    return expression, _normalize_license_file(package, license_file)


def _cyclonedx_licenses(package: dict[str, Any]) -> list[dict[str, Any]]:
    expression, license_file = _license_fields(package)
    if expression is not None:
        if LEGACY_LICENSE_PATTERN.fullmatch(expression):
            expression = " OR ".join(expression.split("/"))
        return [{"expression": expression}]
    assert license_file is not None
    return [{"license": {"name": f"See {license_file}"}}]


def _component(
    package: dict[str, Any],
    component_type: str,
    source_identity: str,
    checksum: str | None,
) -> dict[str, Any]:
    name = package["name"]
    version = package["version"]
    component: dict[str, Any] = {
        "type": component_type,
        "bom-ref": _bom_ref(name, version, source_identity),
        "name": name,
        "version": version,
        "licenses": _cyclonedx_licenses(package),
        "purl": _package_url(name, version),
        "properties": [{"name": "cargo:source", "value": source_identity}],
    }
    if checksum is not None:
        component["hashes"] = [{"alg": "SHA-256", "content": checksum}]

    description = package.get("description")
    if description is not None:
        component["description"] = _require_string(
            description, f"package {name} {version} description"
        )

    repository = package.get("repository")
    if repository is not None:
        repository = _require_string(
            repository, f"package {name} {version} repository"
        )
        component["externalReferences"] = [{"type": "vcs", "url": repository}]
    return component


def _direct_dependency_kinds(root_node: dict[str, Any]) -> dict[str, list[str]]:
    result: dict[str, set[str]] = {}
    for position, raw_dependency in enumerate(root_node.get("deps", [])):
        dependency = _require_mapping(
            raw_dependency, f"root resolve dependency[{position}]"
        )
        package_id = _require_string(
            dependency.get("pkg"), f"root resolve dependency[{position}].pkg"
        )
        kinds = result.setdefault(package_id, set())
        for raw_kind in _require_list(
            dependency.get("dep_kinds"),
            f"root resolve dependency {package_id} dep_kinds",
        ):
            kind = _require_mapping(
                raw_kind, f"root resolve dependency {package_id} kind"
            ).get("kind")
            if kind is None:
                kinds.add("normal")
            else:
                kinds.add(_require_string(kind, f"dependency {package_id} kind"))
    return {package_id: sorted(kinds) for package_id, kinds in result.items()}


def generate_documents(
    metadata: dict[str, Any],
    lockfile: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    (
        packages_by_id,
        nodes_by_id,
        root_id,
        workspace_members,
        source_identities,
        checksums,
    ) = _resolved_packages(metadata, lockfile)

    refs = {
        package_id: _bom_ref(
            packages_by_id[package_id]["name"],
            packages_by_id[package_id]["version"],
            source_identities[package_id],
        )
        for package_id in packages_by_id
    }
    package_ids = sorted(
        packages_by_id,
        key=lambda package_id: _package_sort_key(
            package_id, packages_by_id, source_identities
        ),
    )

    root_package = packages_by_id[root_id]
    root_component = _component(
        root_package,
        "application",
        source_identities[root_id],
        checksums[root_id],
    )
    components = [
        _component(
            packages_by_id[package_id],
            "library",
            source_identities[package_id],
            checksums[package_id],
        )
        for package_id in package_ids
        if package_id != root_id
    ]

    dependencies = []
    for package_id in sorted(package_ids, key=lambda item: refs[item]):
        dependency_refs = sorted(
            refs[dependency_id]
            for dependency_id in nodes_by_id[package_id]["dependencies"]
        )
        dependencies.append({"ref": refs[package_id], "dependsOn": dependency_refs})

    sbom = {
        "$schema": (
            f"http://cyclonedx.org/schema/bom-{CYCLONEDX_SPEC_VERSION}.schema.json"
        ),
        "bomFormat": "CycloneDX",
        "specVersion": CYCLONEDX_SPEC_VERSION,
        "version": 1,
        "metadata": {"component": root_component},
        "components": components,
        "dependencies": dependencies,
    }

    direct_dependency_ids = set(nodes_by_id[root_id]["dependencies"])
    direct_kinds = _direct_dependency_kinds(nodes_by_id[root_id])
    inventory_packages = []
    for package_id in package_ids:
        if package_id in workspace_members:
            continue
        package = packages_by_id[package_id]
        expression, license_file = _license_fields(package)
        entry: dict[str, Any] = {
            "name": package["name"],
            "version": package["version"],
            "bomRef": refs[package_id],
            "purl": _package_url(package["name"], package["version"]),
            "source": source_identities[package_id],
            "licenseExpression": expression,
            "licenseFile": license_file,
            "isDirectDependency": package_id in direct_dependency_ids,
            "dependencyKinds": direct_kinds.get(package_id, []),
        }
        checksum = checksums[package_id]
        if checksum is not None:
            entry["checksum"] = {"algorithm": "SHA-256", "value": checksum}
        repository = package.get("repository")
        if repository is not None:
            entry["repository"] = _require_string(
                repository,
                f"package {package['name']} {package['version']} repository",
            )
        inventory_packages.append(entry)

    license_inventory = {
        "documentType": "third-party-license-inventory",
        "formatVersion": LICENSE_INVENTORY_FORMAT_VERSION,
        "rootComponent": {
            "name": root_package["name"],
            "version": root_package["version"],
            "bomRef": refs[root_id],
            "purl": _package_url(root_package["name"], root_package["version"]),
        },
        "packages": inventory_packages,
    }
    return sbom, license_inventory


def _json_bytes(document: dict[str, Any]) -> bytes:
    text = json.dumps(
        document,
        allow_nan=False,
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    )
    return f"{text}\n".encode("utf-8")


def _write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        dir=path.parent,
        prefix=f".{path.name}.",
    )
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary_path, 0o644)
        os.replace(temporary_path, path)
    finally:
        try:
            temporary_path.unlink()
        except FileNotFoundError:
            pass


def write_release_metadata(
    metadata_path: Path,
    lockfile_path: Path,
    output_directory: Path,
) -> tuple[Path, Path]:
    metadata = _load_json(metadata_path)
    lockfile = _load_lockfile(lockfile_path)
    sbom, license_inventory = generate_documents(metadata, lockfile)

    sbom_path = output_directory / SBOM_FILENAME
    inventory_path = output_directory / LICENSE_INVENTORY_FILENAME
    _write_atomic(sbom_path, _json_bytes(sbom))
    _write_atomic(inventory_path, _json_bytes(license_inventory))
    return sbom_path, inventory_path


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Generate deterministic CycloneDX and third-party license metadata "
            "from precomputed Cargo metadata and Cargo.lock."
        )
    )
    parser.add_argument(
        "--metadata",
        required=True,
        type=Path,
        help="path to JSON from `cargo metadata --format-version 1`",
    )
    parser.add_argument(
        "--cargo-lock",
        required=True,
        type=Path,
        help="path to Cargo.lock",
    )
    parser.add_argument(
        "--output-dir",
        required=True,
        type=Path,
        help="directory that will receive the generated JSON files",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _argument_parser().parse_args(argv)
    try:
        write_release_metadata(
            arguments.metadata,
            arguments.cargo_lock,
            arguments.output_dir,
        )
    except ReleaseMetadataError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
