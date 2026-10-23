#!/usr/bin/env bash
# One compiler version, several places that must agree (OPS-01, DSO-18):
# rust-toolchain.toml, the builder tag and RUSTUP_TOOLCHAIN of every Dockerfile,
# and the builder image of .gitlab-ci.yml. The Dockerfile cannot read the
# toolchain file in a FROM line, so this check is what keeps them equal. Also
# fails when a FROM is not pinned by digest, and when the scanner versions of the
# GitHub workflow and ci/tools.lock (used by the GitLab pipeline) drift apart.
#
# Usage: ci/check-toolchain-pin.sh   (from the repository root)
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
channel=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$root/rust-toolchain.toml")
[ -n "$channel" ] || { echo "check-toolchain-pin: no channel in rust-toolchain.toml" >&2; exit 1; }
status=0

for file in "$root"/Dockerfile "$root"/Dockerfile.musl; do
  name=$(basename "$file")
  if ! grep -Eq "^FROM rust:${channel}-[a-z0-9.]+@sha256:[0-9a-f]{64} AS builder" "$file"; then
    echo "check-toolchain-pin: $name builder is not rust:${channel}-<os>@sha256:<digest>" >&2
    status=1
  fi
  if ! grep -Eq "^ENV RUSTUP_TOOLCHAIN=${channel}\$" "$file"; then
    echo "check-toolchain-pin: $name RUSTUP_TOOLCHAIN is not ${channel}" >&2
    status=1
  fi
  while IFS= read -r line; do
    case "$line" in
      "FROM scratch"*) ;;
      *"@sha256:"*) ;;
      *) echo "check-toolchain-pin: $name has a FROM without a digest: $line" >&2; status=1 ;;
    esac
  done < <(grep '^FROM ' "$file")
done

# The GitLab pipeline runs the same builder image as the Dockerfile.
builder=$(sed -n 's/^FROM \(rust:[^ ]*\) AS builder/\1/p' "$root/Dockerfile")
if ! grep -qF "RUST_IMAGE: $builder" "$root/.gitlab-ci.yml"; then
  echo "check-toolchain-pin: .gitlab-ci.yml RUST_IMAGE is not $builder" >&2
  status=1
fi

# Scanner versions: ci.yml (taiki-e/install-action) and tools.lock (GitLab).
for tool in trivy cosign; do
  locked=$(awk -v t="$tool" '$1 == t {print $2; exit}' "$root/ci/tools.lock")
  if ! grep -q "$tool@$locked" "$root/.github/workflows/ci.yml"; then
    echo "check-toolchain-pin: $tool is $locked in ci/tools.lock but not in .github/workflows/ci.yml" >&2
    status=1
  fi
done

[ "$status" -eq 0 ] && echo "check-toolchain-pin: ok (rust $channel, every FROM pinned by digest, GitLab and scanner pins equal)"
exit "$status"
