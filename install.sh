#!/bin/sh
# Installs the latest killi into ~/.local/bin:
#   curl -fsSL https://raw.githubusercontent.com/timwuhaotian/killi/main/install.sh | sh
set -eu

base="${KILLI_RELEASES_URL:-https://raw.githubusercontent.com/timwuhaotian/killi/main/releases}"
dest="${KILLI_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) binary=killi-arm64; key=sha256-arm64 ;;
  Linux-x86_64) binary=killi-linux-x64; key=sha256-linux-x64 ;;
  Linux-aarch64|Linux-arm64) binary=killi-linux-arm64; key=sha256-linux-arm64 ;;
  *) echo "killi runs on Apple Silicon macOS and Linux (x64, arm64) only" >&2; exit 1 ;;
esac

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL --retry 3 --retry-all-errors --retry-delay 1 --connect-timeout 10 --max-time 300 "$base/$binary" -o "$tmp"
toml=$(curl -fsSL --retry 3 --retry-all-errors --retry-delay 1 --connect-timeout 10 --max-time 300 "$base/latest.toml")
expected=$(printf '%s\n' "$toml" | sed -n "s/^$key = \"\(.*\)\"/\1/p")
if [ "$binary" = "killi-arm64" ] && [ -z "$expected" ]; then
  expected=$(printf '%s\n' "$toml" | sed -n 's/^sha256 = "\(.*\)"/\1/p')
fi
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
else
  actual=$(sha256sum "$tmp" | cut -d' ' -f1)
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] || { echo "checksum mismatch; not installing" >&2; exit 1; }

mkdir -p "$dest"
# Skill and MCP installers (npx skills, add-mcp) detect killi by this folder.
mkdir -p "$HOME/.killi"
chmod 755 "$tmp"
mv "$tmp" "$dest/killi"
trap - EXIT
echo "Installed $("$dest/killi" --version) to $dest/killi"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) case "${SHELL:-}" in
    *zsh*) rc='~/.zshrc' ;;
    *) rc='~/.bashrc' ;;
  esac
  echo "Add it to your PATH: echo 'export PATH=\"$dest:\$PATH\"' >> $rc" ;;
esac
[ -f "$HOME/.killi/config.toml" ] || echo "Next: run \`killi init\` to write ~/.killi/config.toml and add your API keys, or \`killi import\` to copy another agent's"
