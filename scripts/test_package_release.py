#!/usr/bin/env python3
"""Focused tests for deterministic release archive creation."""

from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unicodedata
import unittest
from unittest import mock

if sys.version_info < (3, 11):
    raise SystemExit("test_package_release.py requires Python 3.11 or newer")

sys.dont_write_bytecode = True
SCRIPT_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIRECTORY))
import package_release  # noqa: E402


SCRIPT = SCRIPT_DIRECTORY / "package_release.py"
ARCHIVE_ROOT = "hardknock-0.22.0-dev.1"
SOURCE_DATE_EPOCH = 1_700_000_123


class PackageReleaseTest(unittest.TestCase):
    def run_packager(
        self,
        source_directory: Path,
        archive_path: Path,
        *,
        archive_root: str = ARCHIVE_ROOT,
        source_date_epoch: int = SOURCE_DATE_EPOCH,
    ) -> subprocess.CompletedProcess[str]:
        environment = dict(os.environ)
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--source-dir",
                str(source_directory),
                "--archive",
                str(archive_path),
                "--archive-root",
                archive_root,
                "--source-date-epoch",
                str(source_date_epoch),
            ],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def populate_source(
        self,
        source_directory: Path,
        *,
        reverse_creation_order: bool,
        filesystem_mtime: int,
    ) -> None:
        source_directory.mkdir()
        files = [
            ("README.txt", b"Hardknock release\n", 0o600),
            ("bin/hardknock", b"#!/bin/sh\nexit 0\n", 0o700),
        ]
        if reverse_creation_order:
            files.reverse()

        (source_directory / "empty").mkdir()
        for relative_name, content, mode in files:
            path = source_directory / relative_name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            os.chmod(path, mode)

        for path in sorted(
            source_directory.rglob("*"),
            key=lambda item: len(item.parts),
            reverse=True,
        ):
            os.utime(path, (filesystem_mtime, filesystem_mtime))
        os.utime(
            source_directory,
            (filesystem_mtime, filesystem_mtime),
        )

    def test_archive_is_byte_deterministic_and_has_canonical_contents(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)
            first_source = temporary_directory / "first-source"
            second_source = temporary_directory / "second-source"
            self.populate_source(
                first_source,
                reverse_creation_order=False,
                filesystem_mtime=1_600_000_000,
            )
            self.populate_source(
                second_source,
                reverse_creation_order=True,
                filesystem_mtime=1_800_000_000,
            )
            first_archive = temporary_directory / "first.tar.gz"
            second_archive = temporary_directory / "second.tar.gz"
            first_archive.write_bytes(b"old archive")

            first = self.run_packager(first_source, first_archive)
            second = self.run_packager(second_source, second_archive)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first.stdout, "")
            self.assertEqual(second.stdout, "")

            first_bytes = first_archive.read_bytes()
            self.assertEqual(first_bytes, second_archive.read_bytes())
            self.assertEqual(first_bytes[:2], b"\x1f\x8b")
            self.assertEqual(
                int.from_bytes(first_bytes[4:8], byteorder="little"),
                SOURCE_DATE_EPOCH,
            )
            self.assertEqual(first_bytes[3] & 0x08, 0)
            self.assertEqual(first_bytes[9], 255)

            with tarfile.open(first_archive, mode="r:gz") as archive:
                members = archive.getmembers()
                names = [member.name for member in members]
                self.assertEqual(
                    names,
                    [
                        ARCHIVE_ROOT,
                        f"{ARCHIVE_ROOT}/README.txt",
                        f"{ARCHIVE_ROOT}/bin",
                        f"{ARCHIVE_ROOT}/bin/hardknock",
                        f"{ARCHIVE_ROOT}/empty",
                    ],
                )
                self.assertEqual(names[1:], sorted(names[1:]))
                for member in members:
                    self.assertEqual(member.uid, 0)
                    self.assertEqual(member.gid, 0)
                    self.assertEqual(member.uname, "root")
                    self.assertEqual(member.gname, "root")
                    self.assertEqual(member.mtime, SOURCE_DATE_EPOCH)
                    self.assertTrue(member.isdir() or member.isreg())

                by_name = {member.name: member for member in members}
                self.assertEqual(
                    stat.S_IMODE(by_name[ARCHIVE_ROOT].mode),
                    0o755,
                )
                self.assertEqual(
                    stat.S_IMODE(by_name[f"{ARCHIVE_ROOT}/README.txt"].mode),
                    0o644,
                )
                self.assertEqual(
                    stat.S_IMODE(
                        by_name[f"{ARCHIVE_ROOT}/bin/hardknock"].mode
                    ),
                    0o755,
                )
                self.assertEqual(
                    archive.extractfile(
                        f"{ARCHIVE_ROOT}/README.txt"
                    ).read(),
                    b"Hardknock release\n",
                )
                self.assertEqual(
                    archive.extractfile(
                        f"{ARCHIVE_ROOT}/bin/hardknock"
                    ).read(),
                    b"#!/bin/sh\nexit 0\n",
                )

    def test_traversal_is_rejected_without_replacing_existing_archive(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)
            source_directory = temporary_directory / "source"
            source_directory.mkdir()
            (source_directory / "hardknock").write_bytes(b"binary")
            archive_path = temporary_directory / "release.tar.gz"
            archive_path.write_bytes(b"existing archive")

            result = self.run_packager(
                source_directory,
                archive_path,
                archive_root="../release",
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("archive root name", result.stderr)
            self.assertEqual(archive_path.read_bytes(), b"existing archive")

    def test_empty_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)
            source_directory = temporary_directory / "source"
            source_directory.mkdir()
            (source_directory / "empty-directory").mkdir()
            archive_path = temporary_directory / "release.tar.gz"

            with self.assertRaisesRegex(
                package_release.PackageReleaseError,
                "at least one regular file",
            ):
                package_release.create_release_archive(
                    source_directory,
                    archive_path,
                    ARCHIVE_ROOT,
                    SOURCE_DATE_EPOCH,
                )
            self.assertFalse(archive_path.exists())

    def test_duplicate_normalized_names_are_rejected_atomically(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)
            source_directory = temporary_directory / "source"
            source_directory.mkdir()
            source_file = source_directory / "payload"
            source_file.write_bytes(b"payload")
            metadata = source_file.stat()
            composed = unicodedata.normalize("NFC", "e\u0301")
            decomposed = unicodedata.normalize("NFD", composed)
            first_name = package_release._normalize_relative_path(Path(composed))
            second_name = package_release._normalize_relative_path(Path(decomposed))
            self.assertEqual(first_name, second_name)

            first_entry = package_release._SourceEntry(
                path=source_file,
                relative_name=first_name,
                is_directory=False,
                executable=False,
                device=metadata.st_dev,
                inode=metadata.st_ino,
                size=metadata.st_size,
            )
            second_entry = package_release._SourceEntry(
                path=source_file,
                relative_name=second_name,
                is_directory=False,
                executable=False,
                device=metadata.st_dev,
                inode=metadata.st_ino,
                size=metadata.st_size,
            )
            archive_path = temporary_directory / "release.tar.gz"
            archive_path.write_bytes(b"existing archive")

            with mock.patch.object(
                package_release,
                "_collect_entries",
                return_value=[first_entry, second_entry],
            ):
                with self.assertRaisesRegex(
                    package_release.PackageReleaseError,
                    "duplicate archive member name",
                ):
                    package_release.create_release_archive(
                        source_directory,
                        archive_path,
                        ARCHIVE_ROOT,
                        SOURCE_DATE_EPOCH,
                    )
            self.assertEqual(archive_path.read_bytes(), b"existing archive")

    def test_entry_outside_source_root_is_rejected_atomically(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)
            source_directory = temporary_directory / "source"
            source_directory.mkdir()
            outside_file = temporary_directory / "outside"
            outside_file.write_bytes(b"outside")
            metadata = outside_file.stat()
            outside_entry = package_release._SourceEntry(
                path=outside_file,
                relative_name="outside",
                is_directory=False,
                executable=False,
                device=metadata.st_dev,
                inode=metadata.st_ino,
                size=metadata.st_size,
            )
            archive_path = temporary_directory / "release.tar.gz"
            archive_path.write_bytes(b"existing archive")

            with mock.patch.object(
                package_release,
                "_collect_entries",
                return_value=[outside_entry],
            ):
                with self.assertRaisesRegex(
                    package_release.PackageReleaseError,
                    "outside the source directory",
                ):
                    package_release.create_release_archive(
                        source_directory,
                        archive_path,
                        ARCHIVE_ROOT,
                        SOURCE_DATE_EPOCH,
                    )
            self.assertEqual(archive_path.read_bytes(), b"existing archive")

    def test_symlinks_and_special_files_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="hardknock-package-release-"
        ) as temporary_name:
            temporary_directory = Path(temporary_name)

            symlink_source = temporary_directory / "symlink-source"
            symlink_source.mkdir()
            outside_file = temporary_directory / "outside"
            outside_file.write_bytes(b"outside")
            try:
                (symlink_source / "link").symlink_to(outside_file)
            except (NotImplementedError, OSError) as error:
                self.skipTest(f"symbolic links are unavailable: {error}")
            with self.assertRaisesRegex(
                package_release.PackageReleaseError,
                "symbolic links are not allowed",
            ):
                package_release.create_release_archive(
                    symlink_source,
                    temporary_directory / "symlink.tar.gz",
                    ARCHIVE_ROOT,
                    SOURCE_DATE_EPOCH,
                )

            fifo_source = temporary_directory / "fifo-source"
            fifo_source.mkdir()
            if not hasattr(os, "mkfifo"):
                self.skipTest("FIFOs are unavailable")
            os.mkfifo(fifo_source / "pipe")
            with self.assertRaisesRegex(
                package_release.PackageReleaseError,
                "unsupported source entry type",
            ):
                package_release.create_release_archive(
                    fifo_source,
                    temporary_directory / "fifo.tar.gz",
                    ARCHIVE_ROOT,
                    SOURCE_DATE_EPOCH,
                )


if __name__ == "__main__":
    unittest.main()
