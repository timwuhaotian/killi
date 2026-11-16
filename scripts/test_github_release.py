import hashlib
import importlib.util
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


class AssetTests(unittest.TestCase):
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

    def test_matching_release_lists_binaries_and_manifest(self):
        version, assets = github_release.verified_assets(self.root)
        self.assertEqual(version, "1.2.3")
        self.assertEqual([a.name for a in assets], ["killi-arm64", "killi-linux-x64", "killi-linux-arm64", "latest.toml"])

    def test_stale_binary_is_refused(self):
        (self.root / "releases/killi-linux-x64").write_bytes(b"old")
        with self.assertRaisesRegex(ValueError, "killi-linux-x64"):
            github_release.verified_assets(self.root)

    def test_version_mismatch_with_cargo_is_refused(self):
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.4"\n')
        with self.assertRaisesRegex(ValueError, "Cargo.toml"):
            github_release.verified_assets(self.root)


if __name__ == "__main__":
    unittest.main()
