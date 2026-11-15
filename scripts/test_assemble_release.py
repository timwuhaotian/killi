import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().with_name("assemble-release.py")
VERSION = "1.2.3"
SOURCE_SHA = "0123456789abcdef0123456789abcdef01234567"
ARTIFACTS = (
    ("macos-arm64", "killi-arm64", b"macos binary\n", "8a8e715fc99994ca20e90d04d1927a41269dde0563abe217912747c102424297"),
    ("linux-x64", "killi-linux-x64", b"linux x64 binary\n", "dcb13bb7675e9e74700d515f5080992d62d982eb2d3d4e017fc8d25fa812debd"),
    ("linux-arm64", "killi-linux-arm64", b"linux arm64 binary\n", "cfbd9994cdd60f851fd09de976cf5aa84364c838315572f2565818a9a56f43ca"),
)


class AssembleReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.input = self.root / "input"
        self.output = self.root / "output"
        for directory, binary, content, digest in ARTIFACTS:
            artifact = self.input / directory
            artifact.mkdir(parents=True)
            path = artifact / binary
            path.write_bytes(content)
            path.chmod(0o755)
            self.write_metadata(directory, {"version": VERSION, "source_sha": SOURCE_SHA, "sha256": digest})

    def write_metadata(self, directory, metadata):
        (self.input / directory / "metadata.json").write_text(json.dumps(metadata))

    def run_assemble(self, version=VERSION, published="1.2.2"):
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.input), str(self.output), version, SOURCE_SHA, published],
            capture_output=True, text=True, check=False,
        )

    def assert_rejected(self, message):
        result = self.run_assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(message, result.stderr)
        self.assertFalse(self.output.exists())

    def test_assembles_three_artifacts_and_exact_manifest(self):
        result = self.run_assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(
            {path.name for path in self.output.iterdir()},
            {binary for _, binary, _, _ in ARTIFACTS} | {"latest.toml"},
        )
        for _, binary, content, _ in ARTIFACTS:
            path = self.output / binary
            self.assertEqual(path.read_bytes(), content)
            self.assertEqual(path.stat().st_mode & 0o777, 0o755)
        self.assertEqual(
            (self.output / "latest.toml").read_text(),
            'version = "1.2.3"\n'
            'sha256-arm64 = "8a8e715fc99994ca20e90d04d1927a41269dde0563abe217912747c102424297"\n'
            'sha256-linux-x64 = "dcb13bb7675e9e74700d515f5080992d62d982eb2d3d4e017fc8d25fa812debd"\n'
            'sha256-linux-arm64 = "cfbd9994cdd60f851fd09de976cf5aa84364c838315572f2565818a9a56f43ca"\n'
            'sha256 = "8a8e715fc99994ca20e90d04d1927a41269dde0563abe217912747c102424297"\n',
        )

    def test_rejects_non_numeric_or_non_increasing_versions(self):
        for version, published in (("1.2.3-rc.1", "1.2.2"), ("1.2.3", "1.2.3"),
                                   ("1.2.3", "1.2.4"), ("1.2.3", "invalid")):
            with self.subTest(version=version, published=published):
                result = self.run_assemble(version, published)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.output.exists())

    def test_missing_artifact(self):
        (self.input / "linux-arm64" / "killi-linux-arm64").unlink()
        self.assert_rejected("binary missing or not executable")

    def test_corrupted_binary(self):
        (self.input / "linux-x64" / "killi-linux-x64").write_bytes(b"corrupted\n")
        self.assert_rejected("SHA-256 mismatch")

    def test_mismatched_version_or_source_sha(self):
        directory, _, _, digest = ARTIFACTS[-1]
        for field, value in (("version", "2.0.0"), ("source_sha", "f" * 40)):
            with self.subTest(field=field):
                metadata = {"version": VERSION, "source_sha": SOURCE_SHA, "sha256": digest}
                metadata[field] = value
                self.write_metadata(directory, metadata)
                self.assert_rejected("version or source SHA mismatch")

    def test_malformed_metadata(self):
        directory, _, _, digest = ARTIFACTS[-1]
        path = self.input / directory / "metadata.json"
        cases = (
            ('{', "Expecting property name"),
            ('[]', "invalid metadata fields"),
            (json.dumps({"version": VERSION, "source_sha": SOURCE_SHA}), "invalid metadata fields"),
            (json.dumps({"version": VERSION, "source_sha": SOURCE_SHA, "sha256": "bad"}), "invalid SHA-256"),
            (f'{{"version": "{VERSION}", "version": "{VERSION}", "source_sha": "{SOURCE_SHA}", "sha256": "{digest}"}}', "duplicate metadata field"),
        )
        for content, error in cases:
            with self.subTest(content=content):
                path.write_text(content)
                self.assert_rejected(error)

    def test_preexisting_nonempty_output_is_rejected_without_modification(self):
        self.output.mkdir()
        existing = self.output / "existing.txt"
        existing.write_text("keep me")
        result = self.run_assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("output directory is not empty", result.stderr)
        self.assertEqual(existing.read_text(), "keep me")
        self.assertEqual(list(self.output.iterdir()), [existing])


if __name__ == "__main__":
    unittest.main()
