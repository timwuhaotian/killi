#!/bin/sh
# Installs the latest killi into ~/.local/bin:
#   curl -fsSL https://github.com/timwuhaotian/killi/raw/main/install.sh | sh
set -eu

base="${KILLI_RELEASES_URL:-https://github.com/timwuhaotian/killi/raw/main/releases}"
dest="${KILLI_INSTALL_DIR:-$HOME/.local/bin}"

[ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] || { echo "killi runs on Apple Silicon macOS only" >&2; exit 1; }

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL --connect-timeout 10 --max-time 300 "$base/killi-arm64" -o "$tmp"
expected=$(curl -fsSL --connect-timeout 10 --max-time 300 "$base/latest.toml" | sed -n 's/^sha256 = "\(.*\)"/\1/p')
actual=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
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
  *) echo "Add it to your PATH: echo 'export PATH=\"$dest:\$PATH\"' >> ~/.zshrc" ;;
esac
[ -f "$HOME/.killi/config.toml" ] || echo "Next: run \`killi init\` to write ~/.killi/config.toml and add your API keys, or \`killi import-kimi\` to copy Kimi Code's"
