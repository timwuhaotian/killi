import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("github_release", Path(__file__).resolve().with_name("github-release.py"))
github_release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(github_release)

CHANGELOG = """# Changelog

## Unreleased

## 1.2.3 — 2026-11-16

- Newest change,
  wrapped.

## 1.2.2 — 2026-11-15

- Older change.
"""


class NotesTests(unittest.TestCase):
    def test_takes_only_that_versions_section(self):
        body = github_release.notes(CHANGELOG, "1.2.3")
        self.assertTrue(body.startswith("- Newest change,\n  wrapped.\n\nInstall: "))
        self.assertNotIn("Older change", body)

    def test_last_section_runs_to_the_end(self):
        self.assertTrue(github_release.notes(CHANGELOG, "1.2.2").startswith("- Older change.\n\nInstall: "))

    def test_missing_or_empty_section_is_refused(self):
        for version in ("9.9.9", "Unreleased", "1.2"):
            with self.assertRaises(ValueError):
                github_release.notes(CHANGELOG, version)


class Fixture(unittest.TestCase):
    """A repo root with releases/ whose binaries match latest.toml."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "releases").mkdir()
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        lines = ['version = "1.2.3"']
        for binary, key in github_release.BINARIES:
            content = binary.encode()
            (self.root / "releases" / binary).write_bytes(content)
            lines.append(f'{key} = "{hashlib.sha256(content).hexdigest()}"')
        (self.root / "releases/latest.toml").write_text("\n".join(lines) + "\n")


class AssetTests(Fixture):
    def test_matching_release_lists_binaries_and_manifest(self):
        version, assets = github_release.verified_assets(self.root)
        self.assertEqual(version, "1.2.3")
        self.assertEqual([a.name for a in assets], ["killi-arm64", "killi-linux-x64", "killi-linux-arm64", "latest.toml"])

    def test_stale_binary_is_refused(self):
        (self.root / "releases/killi-linux-x64").write_bytes(b"old")
        with self.assertRaisesRegex(ValueError, "killi-linux-x64"):
            github_release.verified_assets(self.root)

    def test_cargo_ahead_of_releases_still_publishes_releases(self):
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.4"\n')
        self.assertEqual(github_release.verified_assets(self.root)[0], "1.2.3")

    def test_check_needs_notes_for_cargo_version(self):
        (self.root / "CHANGELOG.md").write_text(CHANGELOG)
        github_release.check_notes(self.root)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.4"\n')
        with self.assertRaisesRegex(ValueError, "1.2.4"):
            github_release.check_notes(self.root)


class PublishTests(Fixture):
    """`publish` against a stand-in for `gh` that answers from `self.live`."""

    def setUp(self):
        super().setUp()
        (self.root / "CHANGELOG.md").write_text(CHANGELOG)
        _, assets = github_release.verified_assets(self.root)
        self.live = {
            "list": [{"tagName": "v1.2.3", "isLatest": True}],
            "view": {
                "name": "killi 1.2.3",
                "body": github_release.notes(CHANGELOG, "1.2.3"),
                "assets": [{"name": p.name, "digest": f"sha256:{d}"} for p, d in assets.items()],
            },
        }
        self.calls = []
        original = github_release.gh
        self.addCleanup(setattr, github_release, "gh", original)
        github_release.gh = self.fake_gh

    def fake_gh(self, *args, capture=False):
        self.calls.append(args[:2])
        if args[:2] == ("release", "list"):
            return json.dumps(self.live["list"])
        if args[:2] == ("release", "view"):
            return json.dumps(self.live["view"])
        return ""

    def writes(self):
        return [c for c in self.calls if c[1] not in ("list", "view")]

    def test_up_to_date_release_is_left_alone(self):
        github_release.publish(self.root, "abc")
        self.assertEqual(self.writes(), [])

    def test_corrected_notes_are_pushed_without_reuploading(self):
        self.live["view"]["body"] = "old notes"
        github_release.publish(self.root, "abc")
        self.assertEqual(self.writes(), [("release", "edit")])

    def test_changed_binary_is_reuploaded(self):
        self.live["view"]["assets"][0]["digest"] = "sha256:0"
        github_release.publish(self.root, "abc")
        self.assertEqual(self.writes(), [("release", "upload")])

    def test_new_version_is_created_before_older_pages_go(self):
        self.live["list"] = [{"tagName": "v1.2.2", "isLatest": True}, {"tagName": "v1.0.0", "isLatest": False}]
        github_release.publish(self.root, "abc")
        self.assertEqual(self.writes(), [("release", "create"), ("release", "delete"), ("release", "delete")])

    def test_an_extra_older_release_is_deleted(self):
        self.live["list"].append({"tagName": "v1.2.2", "isLatest": False})
        github_release.publish(self.root, "abc")
        self.assertEqual(self.writes(), [("release", "delete")])


if __name__ == "__main__":
    unittest.main()
