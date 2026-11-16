# Releasing killi

Users install from `releases/killi-arm64`, `releases/killi-linux-x64` or
`releases/killi-linux-arm64` through `install.sh` or `killi update`. Both read
`releases/latest.toml` for the version and the SHA-256 of each binary.

## Prepare and build

1. Start with a clean `main` and commit the changes to ship. Bump `[workspace.package] version`
   in `Cargo.toml` to a newer three-number version than `releases/latest.toml`, run
   `cargo update -p killi --offline`, and add a `## x.y.z — YYYY-MM-DD` heading under
   `## Unreleased` in `CHANGELOG.md`. This also applies when adding a missing platform: users
   already on the current version otherwise would not receive the replacement binary.
2. Push the source commit to `main` and wait for its CI run to pass. Record its full SHA:
   `git rev-parse HEAD`. The release workflow must be dispatched on `main` at this commit.
3. Run the **Release binaries** workflow on `main` using GitHub Actions or
   `gh workflow run release-binaries.yml --ref main`. Check that the run's source SHA is the
   recorded commit (`gh run view <run-id> --json headSha,status,conclusion`) and wait for it
   to succeed. If `main` advanced, do not use a bundle from a different commit.
4. The workflow runs the dependency audit, clippy, tests and native release builds for macOS
   arm64, Linux x64 and Linux arm64. Each runner checks its binary's version. The assembly job
   requires all three binaries from that run and verifies their source SHA, version and hashes
   before producing one `bundle` artifact. Linux builds use Ubuntu 22.04's glibc baseline.
   The workflow has read-only repository permissions and does not publish.

## Publish

1. From the clean, unchanged source commit, download `bundle` to an empty directory:
   `gh run download <run-id> -n bundle -D target/release-bundle`.
2. Review the bundle's `latest.toml`, recompute the SHA-256 of each binary, and confirm the
   manifest version matches `Cargo.toml`. Artifact downloads do not preserve executable bits;
   set them on all three binaries. Copy the binaries and manifest together into `releases/`.
3. Review `git diff` and confirm all three binaries and their hashes belong to the recorded
   source commit. Commit and push the four files in one commit. Never publish a manifest with
   an old binary or a missing platform. `scripts/release.sh` refuses single-host publication.
4. Check a fresh install and `killi update` on each architecture. A pinned
   `KILLI_RELEASES_URL` can verify a commit before using mutable `main` URLs. GitHub raw-file
   caches may take about five minutes to propagate; mismatched cached files fail the checksum
   check rather than install.
5. The push of `releases/latest.toml` runs the **GitHub release** workflow: it rechecks the
   binaries against the manifest and `Cargo.toml`, publishes them as release `vX.Y.Z` with that
   version's `CHANGELOG.md` notes (using the tag if one was pushed, otherwise tagging the
   publish commit), marks it latest and deletes every older release page; tags stay. Check
   that run succeeded; rerun it with `gh workflow run github-release.yml --ref main`.

To refresh the size, memory and capability charts and the README numbers, follow the
`refresh-metrics` skill. It needs `kimi` and `claude` installed and signed in.

## Bad release

Restoring the previous binaries and manifest stops new installations from taking the bad
release, but `killi update` does not downgrade an already updated installation. Ship a
higher-version fix or explicitly reinstall a pinned older commit. A `KILLI_RELEASES_URL`
pinned to a commit keeps serving that commit's files.
