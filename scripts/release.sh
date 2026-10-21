#!/bin/sh
# Builds the release binary and refreshes releases/: the binary and latest.toml (version and
# SHA-256), which install.sh and `killi update` read. Bump [workspace.package] version first.
set -eu
cd "$(dirname "$0")/.."

[ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] || { echo "release builds run on Apple Silicon macOS" >&2; exit 1; }

cargo clippy --workspace --all-targets --locked -q -- -D warnings
cargo test --workspace --locked -q
cargo build --release --locked -p killi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p releases
cp target/release/killi releases/killi-arm64
sum=$(shasum -a 256 releases/killi-arm64 | cut -d' ' -f1)
printf 'version = "%s"\nsha256 = "%s"\n' "$version" "$sum" > releases/latest.toml
echo "releases/killi-arm64 $version $sum"
