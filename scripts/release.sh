#!/bin/sh
# Builds the release binary and refreshes releases/: the binary and latest.toml (version and
# SHA-256), which install.sh and `killi update` read. Bump [workspace.package] version first.
set -eu
cd "$(dirname "$0")/.."

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) binary=killi-arm64; key=sha256-arm64 ;;
  Linux-x86_64) binary=killi-linux-x64; key=sha256-linux-x64 ;;
  Linux-aarch64|Linux-arm64) binary=killi-linux-arm64; key=sha256-linux-arm64 ;;
  *) echo "release builds run on Apple Silicon macOS and Linux (x64, arm64)" >&2; exit 1 ;;
esac

# Panic messages name source files; keep this machine's home folder out of them.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -z "$(git status --porcelain)" ] || { echo "refusing: the working tree is dirty" >&2; exit 1; }
[ "$(git branch --show-current)" = "main" ] || { echo "refusing: not on main" >&2; exit 1; }
released=$(sed -n 's/^version = "\(.*\)"/\1/p' releases/latest.toml 2>/dev/null || true)
if [ "$version" = "$released" ] && [ -f "releases/$binary" ] && [ "$(./releases/$binary --version 2>/dev/null || true)" = "killi $version" ]; then
  echo "refusing: $version is already released for $binary" >&2; exit 1
fi
grep -q "^## $version — " CHANGELOG.md || { echo "refusing: CHANGELOG.md has no '## $version — ' heading" >&2; exit 1; }
if command -v gh >/dev/null 2>&1; then
  conclusion=$(gh run list --commit "$(git rev-parse HEAD)" --limit 1 --json conclusion --jq '.[0].conclusion // empty' 2>/dev/null || true)
  case "$conclusion" in "") echo "warning: no CI run on HEAD yet" >&2;; success) ;; *) echo "refusing: the last CI run on HEAD is '$conclusion'" >&2; exit 1;; esac
fi

# The advisory database records yanked crates as well as vulnerabilities
# (e.g. RUSTSEC-2026-0384 for the yanked tokio 1.54.0).
if command -v cargo-audit >/dev/null 2>&1; then audit="cargo audit";
elif [ -x "$HOME/.cargo/bin/cargo-audit" ]; then audit="$HOME/.cargo/bin/cargo-audit audit";
else echo "WARNING: cargo-audit is not installed, skipping the dependency audit (cargo install cargo-audit)" >&2; audit=; fi
[ -z "${audit:-}" ] || $audit --deny warnings --deny yanked

cargo clippy --workspace --all-targets --locked -q -- -D warnings
cargo test --workspace --locked -q
cargo build --release --locked -p killi

mkdir -p releases
cp target/release/killi releases/$binary
if command -v shasum >/dev/null 2>&1; then
  sum=$(shasum -a 256 releases/$binary | cut -d' ' -f1)
else
  sum=$(sha256sum releases/$binary | cut -d' ' -f1)
fi
arm64=$(sed -n 's/^sha256-arm64 = "\(.*\)"/\1/p' releases/latest.toml 2>/dev/null || true)
linux_x64=$(sed -n 's/^sha256-linux-x64 = "\(.*\)"/\1/p' releases/latest.toml 2>/dev/null || true)
linux_arm64=$(sed -n 's/^sha256-linux-arm64 = "\(.*\)"/\1/p' releases/latest.toml 2>/dev/null || true)
legacy=$(sed -n 's/^sha256 = "\(.*\)"/\1/p' releases/latest.toml 2>/dev/null || true)
case "$key" in
  sha256-arm64) arm64=$sum; legacy=$sum ;;
  sha256-linux-x64) linux_x64=$sum ;;
  sha256-linux-arm64) linux_arm64=$sum ;;
esac
{
  printf 'version = "%s"\n' "$version"
  if [ -n "$arm64" ]; then printf 'sha256-arm64 = "%s"\n' "$arm64"; fi
  if [ -n "$linux_x64" ]; then printf 'sha256-linux-x64 = "%s"\n' "$linux_x64"; fi
  if [ -n "$linux_arm64" ]; then printf 'sha256-linux-arm64 = "%s"\n' "$linux_arm64"; fi
  if [ -n "$legacy" ]; then printf 'sha256 = "%s"\n' "$legacy"; fi
} > releases/latest.toml
echo "releases/$binary $version $sum"
