#!/bin/sh
# Builds the release binary and refreshes releases/: the binary and latest.toml (version and
# SHA-256), which install.sh and `killi update` read. Bump [workspace.package] version first.
set -eu
cd "$(dirname "$0")/.."

[ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] || { echo "release builds run on Apple Silicon macOS" >&2; exit 1; }

# Panic messages name source files; keep this machine's home folder out of them.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -z "$(git status --porcelain)" ] || { echo "refusing: the working tree is dirty" >&2; exit 1; }
[ "$(git branch --show-current)" = "main" ] || { echo "refusing: not on main" >&2; exit 1; }
[ "$version" != "$(sed -n 's/^version = "\(.*\)"/\1/p' releases/latest.toml)" ] || { echo "refusing: $version is already released" >&2; exit 1; }
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
cp target/release/killi releases/killi-arm64
sum=$(shasum -a 256 releases/killi-arm64 | cut -d' ' -f1)
printf 'version = "%s"\nsha256 = "%s"\n' "$version" "$sum" > releases/latest.toml
echo "releases/killi-arm64 $version $sum"
