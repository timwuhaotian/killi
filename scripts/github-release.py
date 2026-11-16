#!/usr/bin/env python3
"""Mirror releases/ into one GitHub Release for its version and delete every older one.

Runs from the publish workflow after releases/latest.toml changes on main. The binaries are
uploaded only when they match the manifest's SHA-256s and the manifest matches Cargo.toml.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARIES = (
    ("killi-arm64", "sha256-arm64"),
    ("killi-linux-x64", "sha256-linux-x64"),
    ("killi-linux-arm64", "sha256-linux-arm64"),
)
REPO_URL = "https://github.com/timwuhaotian/killi"


def notes(changelog, version):
    """The changelog section for `version`, followed by the install line."""
    heading = re.compile(r"^## (\S+)", re.M)
    found = list(heading.finditer(changelog))
    for index, match in enumerate(found):
        if match.group(1) == version:
            end = found[index + 1].start() if index + 1 < len(found) else len(changelog)
            body = changelog[match.end():end].partition("\n")[2].strip()
            if not body:
                break
            return (
                f"{body}\n\nInstall: `curl -fsSL {REPO_URL}/raw/main/install.sh | sh` "
                f"(macOS Apple silicon, Linux x64 and arm64), or `killi update`. "
                f"Full history: [CHANGELOG.md]({REPO_URL}/blob/main/CHANGELOG.md)\n"
            )
    raise ValueError(f"CHANGELOG.md has no notes under ## {version}")


def verified_assets(root):
    """The manifest's version and the files to upload, after checking every hash."""
    manifest = tomllib.loads((root / "releases/latest.toml").read_text())
    version = manifest["version"]
    cargo = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if version != cargo:
        raise ValueError(f"releases/latest.toml is {version} but Cargo.toml is {cargo}")
    assets = []
    for binary, key in BINARIES:
        path = root / "releases" / binary
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != manifest.get(key, "").lower():
            raise ValueError(f"SHA-256 of {path} does not match {key} in latest.toml")
        assets.append(path)
    assets.append(root / "releases/latest.toml")
    return version, assets


def gh(*args, capture=False):
    result = subprocess.run(["gh", *args], check=True, text=True, capture_output=capture)
    return result.stdout


def publish(root, target):
    version, assets = verified_assets(root)
    tag = f"v{version}"
    body = notes((root / "CHANGELOG.md").read_text(), version)
    existing = {r["tagName"] for r in json.loads(gh("release", "list", "--limit", "1000", "--json", "tagName", capture=True))}
    files = [str(a) for a in assets]
    if tag in existing:
        gh("release", "upload", tag, *files, "--clobber")
        gh("release", "edit", tag, "--title", f"killi {version}", "--notes", body, "--latest")
    else:
        # An existing tag (pushed at the source commit) is kept; otherwise it is made at `target`.
        gh("release", "create", tag, *files, "--title", f"killi {version}", "--notes", body,
           "--latest", "--target", target)
    for old in sorted(existing - {tag}):
        # Tags stay so old commits remain addressable; only the release pages go.
        gh("release", "delete", old, "--yes")
    print(f"published {tag}; removed {len(existing - {tag})} older release(s)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", help="commit to tag when the version's tag does not exist yet")
    args = parser.parse_args()
    try:
        publish(ROOT, args.target)
    except (OSError, KeyError, ValueError, tomllib.TOMLDecodeError) as error:
        parser.exit(1, f"error: {error}\n")
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)


if __name__ == "__main__":
    main()
