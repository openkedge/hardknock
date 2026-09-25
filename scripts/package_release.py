#!/usr/bin/env python3
"""Create deterministic gzip-compressed tar release archives."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import gzip
import os
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import unicodedata

if sys.version_info < (3, 11):
    raise SystemExit("package_release.py requires Python 3.11 or newer")


MAX_GZIP_MTIME = (1 << 32) - 1
REGULAR_MODE = 0o644
EXECUTABLE_MODE = 0o755
DIRECTORY_MODE = 0o755
ARCHIVE_UID = 0
ARCHIVE_GID = 0
ARCHIVE_UNAME = "root"
ARCHIVE_GNAME = "root"


class PackageReleaseError(ValueError):
    """Raised when release archive input is unsafe or inconsistent."""


@dataclass(frozen=True)
class _SourceEntry:
    path: Path
    relative_name: str
    is_directory: bool
    executable: bool
    device: int
    inode: int
    size: int


def _normalize_archive_root(value: str) -> str:
    if not value:
        raise PackageReleaseError("archive root name must not be empty")
    if "/" in value or "\\" in value:
        raise PackageReleaseError(
            "archive root name must be a single relative path component"
        )
    normalized = unicodedata.normalize("NFC", value)
    if normalized in {".", ".."} or "\0" in normalized:
        raise PackageReleaseError("archive root name contains path traversal")
    return normalized


def _normalize_relative_path(relative_path: Path) -> str:
    if relative_path.is_absolute():
        raise PackageReleaseError("archive member path must be relative")

    normalized_parts = []
    for part in relative_path.parts:
        if part in {"", ".", ".."} or "/" in part or "\\" in part or "\0" in part:
            raise PackageReleaseError(
                f"archive member path is unsafe: {relative_path}"
            )
        normalized_part = unicodedata.normalize("NFC", part)
        if normalized_part in {"", ".", ".."}:
            raise PackageReleaseError(
                f"archive member path is unsafe: {relative_path}"
            )
        normalized_parts.append(normalized_part)

    if not normalized_parts:
        raise PackageReleaseError("archive member path must not be empty")
    return "/".join(normalized_parts)


def _relative_name(source_root: Path, candidate: Path) -> str:
    try:
        relative_path = candidate.relative_to(source_root)
    except ValueError as error:
        raise PackageReleaseError(
            f"source entry is outside the source directory: {candidate}"
        ) from error
    return _normalize_relative_path(relative_path)


def _ensure_resolved_within_root(source_root: Path, candidate: Path) -> None:
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot resolve source entry {candidate}: {error}"
        ) from error
    try:
        resolved.relative_to(source_root)
    except ValueError as error:
        raise PackageReleaseError(
            f"source entry resolves outside the source directory: {candidate}"
        ) from error


def _ensure_unique_archive_names(names: list[str]) -> None:
    seen: set[str] = set()
    for name in names:
        if name in seen:
            raise PackageReleaseError(f"duplicate archive member name: {name}")
        seen.add(name)


def _source_entry(
    path: Path,
    relative_name: str,
    metadata: os.stat_result,
) -> _SourceEntry:
    if stat.S_ISDIR(metadata.st_mode):
        return _SourceEntry(
            path=path,
            relative_name=relative_name,
            is_directory=True,
            executable=True,
            device=metadata.st_dev,
            inode=metadata.st_ino,
            size=0,
        )
    if stat.S_ISREG(metadata.st_mode):
        return _SourceEntry(
            path=path,
            relative_name=relative_name,
            is_directory=False,
            executable=bool(metadata.st_mode & 0o111),
            device=metadata.st_dev,
            inode=metadata.st_ino,
            size=metadata.st_size,
        )
    if stat.S_ISLNK(metadata.st_mode):
        raise PackageReleaseError(f"symbolic links are not allowed: {path}")
    raise PackageReleaseError(
        f"unsupported source entry type; only directories and regular files are allowed: {path}"
    )


def _collect_entries(source_root: Path) -> list[_SourceEntry]:
    entries: list[_SourceEntry] = []
    pending = [source_root]

    while pending:
        directory = pending.pop()
        try:
            children = list(os.scandir(directory))
        except OSError as error:
            raise PackageReleaseError(
                f"cannot read source directory {directory}: {error}"
            ) from error

        for child in children:
            path = Path(child.path)
            try:
                metadata = child.stat(follow_symlinks=False)
            except OSError as error:
                raise PackageReleaseError(
                    f"cannot inspect source entry {path}: {error}"
                ) from error

            relative_name = _relative_name(source_root, path)
            if stat.S_ISLNK(metadata.st_mode):
                raise PackageReleaseError(f"symbolic links are not allowed: {path}")
            _ensure_resolved_within_root(source_root, path)
            entry = _source_entry(path, relative_name, metadata)
            entries.append(entry)
            if entry.is_directory:
                pending.append(path)

    entries.sort(key=lambda entry: entry.relative_name)
    if not any(not entry.is_directory for entry in entries):
        raise PackageReleaseError(
            "release archive source must contain at least one regular file"
        )
    return entries


def _validate_source_directory(source_directory: Path) -> Path:
    try:
        metadata = os.lstat(source_directory)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot inspect source directory {source_directory}: {error}"
        ) from error
    if stat.S_ISLNK(metadata.st_mode):
        raise PackageReleaseError(
            f"source directory must not be a symbolic link: {source_directory}"
        )
    if not stat.S_ISDIR(metadata.st_mode):
        raise PackageReleaseError(
            f"source directory is not a directory: {source_directory}"
        )
    try:
        return source_directory.resolve(strict=True)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot resolve source directory {source_directory}: {error}"
        ) from error


def _validate_epoch(source_date_epoch: int) -> int:
    if isinstance(source_date_epoch, bool) or not isinstance(
        source_date_epoch, int
    ):
        raise PackageReleaseError("source-date-epoch must be an integer")
    if not 0 <= source_date_epoch <= MAX_GZIP_MTIME:
        raise PackageReleaseError(
            f"source-date-epoch must be between 0 and {MAX_GZIP_MTIME}"
        )
    return source_date_epoch


def _tar_info(
    name: str,
    entry_type: bytes,
    mode: int,
    size: int,
    source_date_epoch: int,
) -> tarfile.TarInfo:
    information = tarfile.TarInfo(name)
    information.type = entry_type
    information.mode = mode
    information.size = size
    information.mtime = source_date_epoch
    information.uid = ARCHIVE_UID
    information.gid = ARCHIVE_GID
    information.uname = ARCHIVE_UNAME
    information.gname = ARCHIVE_GNAME
    information.pax_headers = {}
    return information


def _revalidate_entry(source_root: Path, entry: _SourceEntry) -> os.stat_result:
    try:
        metadata = os.lstat(entry.path)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot re-read source entry {entry.path}: {error}"
        ) from error

    expected_type = stat.S_ISDIR if entry.is_directory else stat.S_ISREG
    if not expected_type(metadata.st_mode):
        raise PackageReleaseError(
            f"source entry changed type while packaging: {entry.path}"
        )
    if (metadata.st_dev, metadata.st_ino) != (entry.device, entry.inode):
        raise PackageReleaseError(
            f"source entry changed while packaging: {entry.path}"
        )
    if not entry.is_directory and metadata.st_size != entry.size:
        raise PackageReleaseError(
            f"source file changed size while packaging: {entry.path}"
        )
    _ensure_resolved_within_root(source_root, entry.path)
    return metadata


def _open_regular_file(source_root: Path, entry: _SourceEntry):
    _revalidate_entry(source_root, entry)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(entry.path, flags)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot open source file {entry.path}: {error}"
        ) from error

    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise PackageReleaseError(
                f"source entry is no longer a regular file: {entry.path}"
            )
        if (metadata.st_dev, metadata.st_ino) != (entry.device, entry.inode):
            raise PackageReleaseError(
                f"source file changed while packaging: {entry.path}"
            )
        if metadata.st_size != entry.size:
            raise PackageReleaseError(
                f"source file changed size while packaging: {entry.path}"
            )
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def _write_archive(
    descriptor: int,
    source_root: Path,
    archive_root: str,
    entries: list[_SourceEntry],
    source_date_epoch: int,
) -> None:
    with os.fdopen(descriptor, "wb") as raw_output:
        with gzip.GzipFile(
            filename="",
            mode="wb",
            compresslevel=9,
            fileobj=raw_output,
            mtime=source_date_epoch,
        ) as compressed_output:
            with tarfile.open(
                fileobj=compressed_output,
                mode="w|",
                format=tarfile.PAX_FORMAT,
            ) as archive:
                archive.addfile(
                    _tar_info(
                        archive_root,
                        tarfile.DIRTYPE,
                        DIRECTORY_MODE,
                        0,
                        source_date_epoch,
                    )
                )
                for entry in entries:
                    member_name = f"{archive_root}/{entry.relative_name}"
                    if entry.is_directory:
                        _revalidate_entry(source_root, entry)
                        archive.addfile(
                            _tar_info(
                                member_name,
                                tarfile.DIRTYPE,
                                DIRECTORY_MODE,
                                0,
                                source_date_epoch,
                            )
                        )
                        continue

                    mode = EXECUTABLE_MODE if entry.executable else REGULAR_MODE
                    information = _tar_info(
                        member_name,
                        tarfile.REGTYPE,
                        mode,
                        entry.size,
                        source_date_epoch,
                    )
                    with _open_regular_file(source_root, entry) as source_file:
                        archive.addfile(information, source_file)
        raw_output.flush()
        os.fsync(raw_output.fileno())


def create_release_archive(
    source_directory: Path,
    archive_path: Path,
    archive_root_name: str,
    source_date_epoch: int,
) -> Path:
    """Create and atomically replace a deterministic release archive."""

    source_root = _validate_source_directory(Path(source_directory))
    archive_root = _normalize_archive_root(archive_root_name)
    epoch = _validate_epoch(source_date_epoch)
    entries = _collect_entries(source_root)
    _ensure_unique_archive_names(
        [archive_root]
        + [f"{archive_root}/{entry.relative_name}" for entry in entries]
    )

    requested_archive_path = Path(archive_path)
    if not requested_archive_path.name:
        raise PackageReleaseError("archive output path must name a file")
    try:
        resolved_archive_path = requested_archive_path.resolve(strict=False)
    except OSError as error:
        raise PackageReleaseError(
            f"cannot resolve archive output path {requested_archive_path}: {error}"
        ) from error
    try:
        resolved_archive_path.relative_to(source_root)
    except ValueError:
        pass
    else:
        raise PackageReleaseError(
            "archive output path must be outside the source directory"
        )

    try:
        resolved_archive_path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary_name = tempfile.mkstemp(
            dir=resolved_archive_path.parent,
            prefix=f".{resolved_archive_path.name}.",
            suffix=".tmp",
        )
    except OSError as error:
        raise PackageReleaseError(
            f"cannot create temporary archive beside {resolved_archive_path}: {error}"
        ) from error

    temporary_path = Path(temporary_name)
    try:
        try:
            _write_archive(
                descriptor,
                source_root,
                archive_root,
                entries,
                epoch,
            )
        except PackageReleaseError:
            raise
        except (OSError, tarfile.TarError, ValueError) as error:
            raise PackageReleaseError(
                f"cannot create release archive {resolved_archive_path}: {error}"
            ) from error
        try:
            os.chmod(temporary_path, REGULAR_MODE)
            os.replace(temporary_path, resolved_archive_path)
        except OSError as error:
            raise PackageReleaseError(
                f"cannot atomically replace release archive "
                f"{resolved_archive_path}: {error}"
            ) from error
    finally:
        try:
            temporary_path.unlink()
        except FileNotFoundError:
            pass
    return resolved_archive_path


def _argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Create a deterministic gzip-compressed release tar archive."
    )
    parser.add_argument(
        "--source-dir",
        required=True,
        type=Path,
        help="directory containing the files to package",
    )
    parser.add_argument(
        "--archive",
        required=True,
        type=Path,
        help="output path for the .tar.gz archive",
    )
    parser.add_argument(
        "--archive-root",
        required=True,
        help="single top-level directory name inside the archive",
    )
    parser.add_argument(
        "--source-date-epoch",
        required=True,
        type=int,
        help="integer timestamp used for every tar member and the gzip header",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = _argument_parser().parse_args(argv)
    try:
        create_release_archive(
            arguments.source_dir,
            arguments.archive,
            arguments.archive_root,
            arguments.source_date_epoch,
        )
    except PackageReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
