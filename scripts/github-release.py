#!/usr/bin/env python3
"""Mirror releases/ into one GitHub Release for its version and delete every older one.

Runs from the GitHub release workflow on main. The binaries are uploaded only when they match
the manifest's SHA-256s, and a page that already matches is left alone, so reruns are safe.
`--check` runs before a build and refuses a Cargo.toml version with no CHANGELOG.md notes.
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
    """The manifest's version and the files to upload with their SHA-256, after checking every hash.

    Cargo.toml is not compared: it moves ahead of releases/ while the next version is prepared.
    """
    manifest = tomllib.loads((root / "releases/latest.toml").read_text())
    assets = {}
    for binary, key in BINARIES:
        path = root / "releases" / binary
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != manifest.get(key, "").lower():
            raise ValueError(f"SHA-256 of {path} does not match {key} in latest.toml")
        assets[path] = digest
    path = root / "releases/latest.toml"
    assets[path] = hashlib.sha256(path.read_bytes()).hexdigest()
    return manifest["version"], assets


def check_notes(root):
    """Refuse a version about to be built when CHANGELOG.md has no notes for it."""
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    notes((root / "CHANGELOG.md").read_text(), version)
    print(f"CHANGELOG.md has notes for {version}")


def gh(*args, capture=False):
    result = subprocess.run(["gh", *args], check=True, text=True, capture_output=capture)
    return result.stdout


def publish(root, target):
    """Make `v<version>` the only, latest release, with releases/ as its assets; a no-op when it is."""
    version, assets = verified_assets(root)
    tag, title = f"v{version}", f"killi {version}"
    body = notes((root / "CHANGELOG.md").read_text(), version)
    releases = json.loads(gh("release", "list", "--limit", "1000", "--json", "tagName,isLatest", capture=True))
    existing = {r["tagName"] for r in releases}
    files = [str(path) for path in assets]
    if tag in existing:
        live = json.loads(gh("release", "view", tag, "--json", "assets,body,name", capture=True))
        uploaded = {a["name"]: a.get("digest") for a in live["assets"]}
        wanted = {path.name: f"sha256:{digest}" for path, digest in assets.items()}
        latest = any(r["tagName"] == tag and r["isLatest"] for r in releases)
        stale_assets = uploaded != wanted
        stale_page = not latest or live["name"] != title or live["body"].strip() != body.strip()
        if stale_assets:
            gh("release", "upload", tag, *files, "--clobber")
        if stale_page:
            gh("release", "edit", tag, "--title", title, "--notes", body, "--latest")
        changed = stale_assets or stale_page
    else:
        # An existing tag (pushed at the source commit) is kept; otherwise it is made at `target`.
        gh("release", "create", tag, *files, "--title", title, "--notes", body, "--latest", "--target", target)
        changed = True
    for old in sorted(existing - {tag}):
        # Tags stay so old commits remain addressable; only the release pages go.
        gh("release", "delete", old, "--yes")
    removed = len(existing - {tag})
    print(f"{'published' if changed else 'up to date:'} {tag}; removed {removed} older release(s)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("target", nargs="?", help="commit to tag when the version's tag does not exist yet")
    group.add_argument("--check", action="store_true", help="only check CHANGELOG.md has notes for Cargo.toml's version")
    args = parser.parse_args()
    try:
        if args.check:
            check_notes(ROOT)
        else:
            publish(ROOT, args.target)
    except (OSError, KeyError, ValueError, tomllib.TOMLDecodeError) as error:
        parser.exit(1, f"error: {error}\n")
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)


if __name__ == "__main__":
    main()
