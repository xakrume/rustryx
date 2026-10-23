#!/usr/bin/env bash
# Prints [workspace.package] version of the root Cargo.toml. One implementation
# for the release check and for the version label of non-release images, so the
# two cannot read the version differently.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=$(awk '/^\[workspace\.package\]/{in_ws=1; next} /^\[/{in_ws=0} in_ws && /^version *=/{gsub(/"/, "", $3); print $3; exit}' "$root/Cargo.toml")
[ -n "$version" ] || { echo "workspace-version: no version in [workspace.package]" >&2; exit 1; }
echo "$version"
