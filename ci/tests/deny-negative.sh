#!/usr/bin/env bash
# Negative tests of deny.toml (DSO-05): the gate must turn red for
#   - a dependency whose only licence is GPL,
#   - a git dependency,
#   - a crate named openssl-sys,
# and stay green for a plain MIT path dependency. A deny.toml that was loosened
# by accident would otherwise keep passing on the real lockfile, which contains
# none of these. Needs cargo, cargo-deny and git; no network (local path and
# file:// git sources only, and the advisories check is not run).
#
# Usage: ci/tests/deny-negative.sh   (from the repository root)
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

make_crate() { # name version licence [extra Cargo.toml lines]
  mkdir -p "$work/$1/src"
  : >"$work/$1/src/lib.rs"
  printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\nlicense = "%s"\npublish = false\n%s\n' \
    "$1" "$2" "$3" "${4:-}" >"$work/$1/Cargo.toml"
}

# Runs cargo-deny on a fresh app crate that depends on the given crates.
# $1 label, $2 expected exit (0 or non-zero as "fail"), rest: dependency lines.
check_case() {
  local label=$1 expect=$2
  shift 2
  mkdir -p "$work/app/src"
  : >"$work/app/src/lib.rs"
  {
    printf '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\nlicense = "MIT"\npublish = false\n[dependencies]\n'
    printf '%s\n' "$@"
  } >"$work/app/Cargo.toml"
  # Same file as production, except the one switch that only makes sense for the
  # real lockfile: the scratch crate does not use webpki-roots, so the exception
  # for it would be reported as unused.
  sed 's/^unused-license-exception = "deny"/unused-license-exception = "allow"/' "$root/deny.toml" >"$work/app/deny.toml"
  rm -f "$work/app/Cargo.lock"
  # Only path and file:// sources: the lockfile is written without the registry.
  (cd "$work/app" && cargo generate-lockfile --offline >/dev/null 2>&1) || (cd "$work/app" && cargo generate-lockfile >/dev/null 2>&1)
  local rc=0
  (cd "$work/app" && cargo deny check licenses bans sources >"$work/out.txt" 2>&1) || rc=$?
  if [ "$expect" = pass ] && [ "$rc" -ne 0 ]; then
    echo "FAIL $label: expected green, got rc=$rc"; sed -n 1,40p "$work/out.txt"; exit 1
  fi
  if [ "$expect" = fail ] && [ "$rc" -eq 0 ]; then
    echo "FAIL $label: expected red, deny.toml let it through"; exit 1
  fi
  echo "ok   $label"
}

make_crate clean-dep 0.1.0 MIT
make_crate gpl-dep 0.1.0 GPL-3.0-only
make_crate openssl-sys 0.9.0 MIT
make_crate git-dep 0.1.0 MIT
(cd "$work/git-dep" && git init -q && git add -A &&
  git -c user.email=ci@example.invalid -c user.name=ci -c commit.gpgsign=false commit -qm init)

check_case "MIT path dependency passes"      pass 'clean-dep = { path = "../clean-dep" }'
check_case "GPL-only dependency is rejected" fail 'gpl-dep = { path = "../gpl-dep" }'
check_case "openssl-sys is banned"           fail 'openssl-sys = { path = "../openssl-sys" }'
check_case "git dependency is rejected"      fail "git-dep = { git = \"file://$work/git-dep\" }"
echo "deny-negative: all cases behaved"
