#!/usr/bin/env bash
# Installs one pinned tool from ci/tools.lock after verifying its SHA-256.
# Why not curl | tar: a release asset can be replaced after the fact, and the
# checksum file next to it would be replaced together with it. The expected hash
# is in git, reviewed with the version bump.
#
# Usage: ci/install-tool.sh <tool> [<destination-directory>]
# Default destination: $RUNNER_TEMP/bin on GitHub Actions (added to PATH), else
# ~/.local/bin. Linux only (x86_64 and aarch64).
set -euo pipefail

tool=${1:?usage: install-tool.sh <tool> [destination]}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dest=${2:-${RUNNER_TEMP:+$RUNNER_TEMP/bin}}
dest=${dest:-$HOME/.local/bin}

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "install-tool: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

line=$(grep -E "^${tool} [^ ]+ ${arch} " "$here/tools.lock" | head -n 1 || true)
[ -n "$line" ] || { echo "install-tool: $tool for $arch is not in ci/tools.lock" >&2; exit 1; }
read -r _ version _ sha url member <<<"$line"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
file="$work/asset"
curl --fail --silent --show-error --location --retry 3 --output "$file" "$url"
actual=$(sha256sum "$file" | cut -d' ' -f1)
if [ "$actual" != "$sha" ]; then
  echo "install-tool: checksum mismatch for $tool $version" >&2
  echo "  expected $sha" >&2
  echo "  actual   $actual" >&2
  exit 1
fi

mkdir -p "$dest"
if [ -n "${member:-}" ]; then
  case "$url" in
    *.tar.xz) tar -xJf "$file" -C "$work" "$member" ;;
    *) tar -xzf "$file" -C "$work" "$member" ;;
  esac
  install -m 0755 "$work/$member" "$dest/$tool"
else
  install -m 0755 "$file" "$dest/$tool"
fi
echo "install-tool: $tool $version installed to $dest/$tool"
if [ -n "${GITHUB_PATH:-}" ]; then echo "$dest" >>"$GITHUB_PATH"; fi
