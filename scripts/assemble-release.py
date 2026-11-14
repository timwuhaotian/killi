#!/usr/bin/env python3
"""Validate release artifacts and assemble the release directory."""

import argparse
import hashlib
import json
import re
import shutil
import stat
from pathlib import Path

ARTIFACTS = (
    ("macos-arm64", "killi-arm64", "sha256-arm64"),
    ("linux-x64", "killi-linux-x64", "sha256-linux-x64"),
    ("linux-arm64", "killi-linux-arm64", "sha256-linux-arm64"),
)
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def metadata_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate metadata field: {key}")
        result[key] = value
    return result


def validate_versions(version, published):
    if not VERSION.fullmatch(version) or not VERSION.fullmatch(published):
        raise ValueError("versions must be three numeric components")
    if tuple(map(int, version.split("."))) <= tuple(map(int, published.split("."))):
        raise ValueError("release version must be newer than the published version")


def assemble(input_dir, output_dir, version, source_sha, published):
    validate_versions(version, published)
    if not re.fullmatch(r"[0-9a-fA-F]{40}", source_sha):
        raise ValueError("source_sha must be a full 40-hex SHA")
    if output_dir.exists() and (not output_dir.is_dir() or any(output_dir.iterdir())):
        raise ValueError(f"output directory is not empty: {output_dir}")

    sums = {}
    for directory, binary, key in ARTIFACTS:
        artifact = input_dir / directory
        metadata = json.loads((artifact / "metadata.json").read_text(), object_pairs_hook=metadata_object)
        if not isinstance(metadata, dict) or set(metadata) != {"version", "source_sha", "sha256"}:
            raise ValueError(f"invalid metadata fields: {artifact}")
        if metadata["version"] != version or metadata["source_sha"] != source_sha:
            raise ValueError(f"version or source SHA mismatch: {artifact}")
        expected = metadata["sha256"]
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", expected):
            raise ValueError(f"invalid SHA-256: {artifact}")
        path = artifact / binary
        if not path.is_file() or not path.stat().st_mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH):
            raise ValueError(f"binary missing or not executable: {path}")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        sums[key] = digest.hexdigest()
        if sums[key] != expected.lower():
            raise ValueError(f"SHA-256 mismatch: {path}")

    output_dir.mkdir(parents=True, exist_ok=True)
    for directory, binary, _ in ARTIFACTS:
        with (input_dir / directory / binary).open("rb") as source, (output_dir / binary).open("xb") as dest:
            shutil.copyfileobj(source, dest)
        (output_dir / binary).chmod(0o755)
    with (output_dir / "latest.toml").open("x") as manifest:
        manifest.write(f'version = "{version}"\n')
        for _, _, key in ARTIFACTS:
            manifest.write(f'{key} = "{sums[key]}"\n')
        manifest.write(f'sha256 = "{sums["sha256-arm64"]}"\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input_dir", type=Path)
    parser.add_argument("output_dir", type=Path)
    parser.add_argument("version")
    parser.add_argument("source_sha")
    parser.add_argument("published_version")
    args = parser.parse_args()
    try:
        assemble(args.input_dir, args.output_dir, args.version, args.source_sha, args.published_version)
    except (OSError, UnicodeError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
