#!/usr/bin/env python3
"""Focused tests for the deterministic release metadata generator."""

from __future__ import annotations

import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("generate_release_metadata.py")
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
ROOT_ID = "path+file:///workspace/release-app#1.2.3"
DEP_ID = f"{REGISTRY}#dep-one@2.0.0"
LOCAL_DEP_ID = "path+file:///workspace/vendor/local-dep#0.4.0"
CHECKSUM = "ab" * 32


def package(
    package_id: str,
    name: str,
    version: str,
    *,
    source: str | None,
    license_expression: str | None,
    license_file: str | None,
    manifest_path: str,
    repository: str | None = None,
) -> dict[str, object]:
    return {
        "id": package_id,
        "name": name,
        "version": version,
        "source": source,
        "license": license_expression,
        "license_file": license_file,
        "manifest_path": manifest_path,
        "description": f"{name} package",
        "repository": repository,
    }


def fixture_metadata() -> dict[str, object]:
    return {
        "version": 1,
        "workspace_root": "/workspace/release-app",
        "workspace_members": [ROOT_ID],
        "packages": [
            package(
                ROOT_ID,
                "release-app",
                "1.2.3",
                source=None,
                license_expression="Apache-2.0",
                license_file=None,
                manifest_path="/workspace/release-app/Cargo.toml",
                repository="https://example.invalid/release-app",
            ),
            package(
                DEP_ID,
                "dep-one",
                "2.0.0",
                source=REGISTRY,
                license_expression="MIT/Apache-2.0",
                license_file=None,
                manifest_path="/cargo/registry/dep-one-2.0.0/Cargo.toml",
                repository="https://example.invalid/dep-one",
            ),
            package(
                LOCAL_DEP_ID,
                "local-dep",
                "0.4.0",
                source=None,
                license_expression=None,
                license_file="/workspace/vendor/local-dep/LICENSE-THIRD-PARTY",
                manifest_path="/workspace/vendor/local-dep/Cargo.toml",
            ),
        ],
        "resolve": {
            "root": ROOT_ID,
            "nodes": [
                {
                    "id": ROOT_ID,
                    "dependencies": [DEP_ID, LOCAL_DEP_ID],
                    "deps": [
                        {
                            "name": "dep_one",
                            "pkg": DEP_ID,
                            "dep_kinds": [{"kind": None, "target": None}],
                        },
                        {
                            "name": "local_dep",
                            "pkg": LOCAL_DEP_ID,
                            "dep_kinds": [{"kind": "build", "target": None}],
                        },
                    ],
                    "features": [],
                },
                {
                    "id": DEP_ID,
                    "dependencies": [LOCAL_DEP_ID],
                    "deps": [],
                    "features": [],
                },
                {
                    "id": LOCAL_DEP_ID,
                    "dependencies": [],
                    "deps": [],
                    "features": [],
                },
            ],
        },
    }


def fixture_lockfile() -> str:
    return f"""version = 4

[[package]]
name = "release-app"
version = "1.2.3"
dependencies = [
 "dep-one",
 "local-dep",
]

[[package]]
name = "dep-one"
version = "2.0.0"
source = "{REGISTRY}"
checksum = "{CHECKSUM}"
dependencies = [
 "local-dep",
]

[[package]]
name = "local-dep"
version = "0.4.0"
"""


class ReleaseMetadataTest(unittest.TestCase):
    def run_generator(
        self,
        temporary_directory: Path,
        metadata: dict[str, object],
        output_name: str,
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        metadata_path = temporary_directory / f"{output_name}-metadata.json"
        lockfile_path = temporary_directory / "Cargo.lock"
        output_path = temporary_directory / output_name
        metadata_path.write_text(
            json.dumps(metadata, sort_keys=True),
            encoding="utf-8",
        )
        lockfile_path.write_text(fixture_lockfile(), encoding="utf-8")
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--metadata",
                str(metadata_path),
                "--cargo-lock",
                str(lockfile_path),
                "--output-dir",
                str(output_path),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        return result, output_path

    def test_documents_are_complete_and_byte_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_name:
            temporary_directory = Path(temporary_name)
            first, first_output = self.run_generator(
                temporary_directory, fixture_metadata(), "first"
            )
            second, second_output = self.run_generator(
                temporary_directory, fixture_metadata(), "second"
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first.stdout, "")
            self.assertEqual(second.stdout, "")

            first_sbom_bytes = (first_output / "sbom.cdx.json").read_bytes()
            second_sbom_bytes = (second_output / "sbom.cdx.json").read_bytes()
            first_license_bytes = (
                first_output / "third-party-licenses.json"
            ).read_bytes()
            second_license_bytes = (
                second_output / "third-party-licenses.json"
            ).read_bytes()
            self.assertEqual(first_sbom_bytes, second_sbom_bytes)
            self.assertEqual(first_license_bytes, second_license_bytes)

            sbom = json.loads(first_sbom_bytes)
            root = sbom["metadata"]["component"]
            self.assertEqual(root["name"], "release-app")
            self.assertEqual(root["version"], "1.2.3")
            self.assertEqual(root["licenses"], [{"expression": "Apache-2.0"}])

            components = {
                component["name"]: component for component in sbom["components"]
            }
            self.assertEqual(set(components), {"dep-one", "local-dep"})
            self.assertEqual(
                components["dep-one"]["licenses"],
                [{"expression": "MIT OR Apache-2.0"}],
            )
            self.assertEqual(
                components["local-dep"]["licenses"],
                [{"license": {"name": "See LICENSE-THIRD-PARTY"}}],
            )
            self.assertEqual(
                components["dep-one"]["hashes"],
                [{"alg": "SHA-256", "content": CHECKSUM}],
            )

            inventory = json.loads(first_license_bytes)
            self.assertEqual(
                inventory["rootComponent"]["name"],
                "release-app",
            )
            inventory_packages = {
                item["name"]: item for item in inventory["packages"]
            }
            self.assertEqual(
                inventory_packages["dep-one"]["licenseExpression"],
                "MIT/Apache-2.0",
            )
            self.assertEqual(
                inventory_packages["dep-one"]["checksum"],
                {"algorithm": "SHA-256", "value": CHECKSUM},
            )
            self.assertEqual(
                inventory_packages["local-dep"]["licenseFile"],
                "LICENSE-THIRD-PARTY",
            )
            self.assertEqual(
                inventory_packages["dep-one"]["dependencyKinds"],
                ["normal"],
            )
            self.assertEqual(
                inventory_packages["local-dep"]["dependencyKinds"],
                ["build"],
            )

    def test_missing_license_is_rejected(self) -> None:
        metadata = copy.deepcopy(fixture_metadata())
        dependency = metadata["packages"][1]
        dependency["license"] = None
        dependency["license_file"] = None

        with tempfile.TemporaryDirectory() as temporary_name:
            result, output_path = self.run_generator(
                Path(temporary_name), metadata, "invalid"
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "package dep-one 2.0.0 must declare license or license_file",
                result.stderr,
            )
            self.assertFalse(output_path.exists())


if __name__ == "__main__":
    unittest.main()
