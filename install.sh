#!/bin/sh
# Installs or updates reaper from the latest release:
#   curl -fsSL https://github.com/LeeJMorel/agent-reaper/releases/latest/download/install.sh | sh
set -eu

release=https://github.com/LeeJMorel/agent-reaper/releases/latest/download
bin="$HOME/.local/bin"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
# A shell under Rosetta reports x86_64 on Apple silicon.
if [ "$os" = darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then arch=arm64; fi
case "$os-$arch" in
    linux-arm64) arch=aarch64 ;;
    darwin-aarch64) arch=arm64 ;;
esac
case "$os-$arch" in
    linux-x86_64 | linux-aarch64 | darwin-x86_64 | darwin-arm64) ;;
    *)
        echo "reaper: no build for $os/$arch. Build it with Rust instead:" >&2
        echo "  cargo install --locked --git https://github.com/LeeJMorel/agent-reaper agent-reaper" >&2
        exit 1
        ;;
esac

asset="agent-reaper-$os-$arch.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
curl -fsSL "$release/$asset" -o "$asset"
curl -fsSL "$release/SHA256SUMS" -o SHA256SUMS
if command -v sha256sum >/dev/null 2>&1; then sum=sha256sum; else sum="shasum -a 256"; fi
if ! grep " $asset\$" SHA256SUMS | $sum -c - >/dev/null 2>&1; then
    echo "reaper: $asset failed its checksum; nothing was installed" >&2
    exit 1
fi
tar -xzf "$asset"

# Moved into place in one step, so a reaper that's running keeps working.
mkdir -p "$bin"
cp reaper "$bin/.reaper-new"
chmod 755 "$bin/.reaper-new"
mv "$bin/.reaper-new" "$bin/reaper"

echo "$("$bin/reaper" --version) is in $bin"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Add it to your PATH to run it: export PATH=\"$bin:\$PATH\"" ;;
esac
