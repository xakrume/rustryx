#!/usr/bin/env bash
# OPS-70: one version, one source, the tag. The release fails before anything is
# built when the tag, the workspace version and the chart disagree, because an
# image tagged 1.2.3 that contains 1.2.2 is found out by users, not by CI.
#
# Usage: ci/release-check.sh vX.Y.Z   (from the repository root)
set -euo pipefail

tag=${1:?usage: release-check.sh vX.Y.Z}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
status=0

if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release-check: tag '$tag' is not vX.Y.Z (pre-release suffixes are not published)" >&2
  exit 1
fi
version=${tag#v}

check() { # description actual
  if [ "$2" = "$version" ]; then
    echo "ok   $1 = $2"
  else
    echo "FAIL $1 is '$2', the tag says '$version'" >&2
    status=1
  fi
}

check "workspace.package.version" "$("$root/ci/workspace-version.sh" 2>/dev/null || echo missing)"

chart="$root/deploy/helm/rustryx/Chart.yaml"
if [ -f "$chart" ]; then
  check "Chart.yaml version" "$(sed -n 's/^version: *"\{0,1\}\([^"]*\)"\{0,1\} *$/\1/p' "$chart" | head -n 1)"
  check "Chart.yaml appVersion" "$(sed -n 's/^appVersion: *"\{0,1\}\([^"]*\)"\{0,1\} *$/\1/p' "$chart" | head -n 1)"
fi

# Cargo.lock must already carry the workspace crates at this version, otherwise
# `cargo build --locked` of the release would rewrite it and fail.
if ! grep -q "^version = \"$version\"" "$root/Cargo.lock"; then
  echo "FAIL Cargo.lock has no crate at version $version" >&2
  status=1
fi

exit "$status"
