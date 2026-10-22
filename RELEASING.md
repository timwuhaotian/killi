# Releasing killi

Releases are built locally on an Apple silicon Mac; CI only checks. Users install from
`releases/killi-arm64` through `install.sh` or `killi update`, which both read
`releases/latest.toml` for the version and SHA-256.

1. **Start clean and green.** Everything to ship is committed on `main`.
2. **Bump the version** in the root `Cargo.toml` (`[workspace.package] version`).
3. **Changelog.** In `CHANGELOG.md`, put a `## x.y.z — YYYY-MM-DD` heading under
   `## Unreleased`, so the release takes its entries and `Unreleased` starts empty.
4. **Build:** `scripts/release.sh`. It runs clippy and the tests, builds the release binary,
   and overwrites `releases/killi-arm64` and `releases/latest.toml`.
5. **Commit and push** everything at once: version, lock file, changelog and `releases/`.
6. **Check the update** from an installed copy: `killi update` should report the new version.
   GitHub serves raw files through a cache for about five minutes after a push, so until then
   `killi update` may still see the previous version.

To refresh the size, memory and capability charts and the README numbers, follow the
`refresh-metrics` skill (`.agents/skills/refresh-metrics`); it needs `kimi` and `claude`
installed and signed in.
