# Releasing killi

Releases are built natively on each host (Apple silicon Mac, Linux x64/arm64); CI only
checks. Users install from `releases/killi-arm64`, `releases/killi-linux-x64` or
`releases/killi-linux-arm64` through `install.sh` or `killi update`, which read
`releases/latest.toml` for the version and the SHA-256 of each binary.

1. **Start clean and green.** Everything to ship is committed on `main`.
2. **Bump the version** in the root `Cargo.toml` (`[workspace.package] version`), then
   `cargo update -p killi --offline` so `Cargo.lock` carries it; the release script builds
   with `--locked` and refuses a stale lock file.
3. **Changelog.** In `CHANGELOG.md`, put a `## x.y.z — YYYY-MM-DD` heading under
   `## Unreleased`, so the release takes its entries and `Unreleased` starts empty.
4. **Build:** `scripts/release.sh` on each host. It runs clippy and the tests, builds the
   release binary for that host, and writes it to `releases/` with its SHA-256 in
   `releases/latest.toml`.
5. **Commit and push** everything at once: version, lock file, changelog and `releases/`.
6. **Check the update** from an installed copy: `killi update` should report the new version.
   GitHub serves raw files through a cache for about five minutes after a push, so until then
   `killi update` may still see the previous version.
7. **Clean up:** `cargo clean` to drop the local `target/` directory the release build left behind.

To refresh the size, memory and capability charts and the README numbers, follow the
`refresh-metrics` skill (`.agents/skills/refresh-metrics`); it needs `kimi` and `claude`
installed and signed in.

## Bad release

If a pushed release turns out broken, roll the pointer back: find the previous release
commit (`git log -- releases/latest.toml`), restore the binaries and the pointer from it
(`git checkout <commit> -- releases/killi-arm64 releases/killi-linux-x64 releases/killi-linux-arm64 releases/latest.toml`), commit and push.
Installs and `killi update` then see the previous version again. A `KILLI_RELEASES_URL`
pinned to a commit keeps serving whatever that commit holds, so pinned installs never
pick up the bad binary in the first place.
