#!/usr/bin/env bash
# Acceptance checks of a built image (OPS-10..OPS-15, OPS-82). Everything here is
# a fact read from the image or from a container, not from the Dockerfile.
#
#   - runs as 10001:10001, has no HEALTHCHECK, entrypoint is the rustryx binary
#   - has no shell and no package manager
#   - the OCI labels are present (ci/check-image-labels.sh)
#   - the binary is static for the musl variant and dynamic for the glibc one
#   - runs with a read-only root filesystem, all capabilities dropped and only
#     /tmp writable
#   - optional: `serve` against a real database on a read-only filesystem
#
# Usage: ci/test-image.sh <image> {musl|glibc} [--with-serve]
# --with-serve needs RUSTRYX_TEST_DATABASE_URL, reachable from the container
# (on Linux CI: a service container, with --network host). Enable it in CI once
# `rustryx serve` exists; until then only `--version` is exercised.
# IMAGE_SIZE_BUDGET_MB, when set, fails the run if the image is larger (OPS-17).
set -euo pipefail

usage="usage: test-image.sh <image> <musl|glibc> [--with-serve]"
image=${1:?$usage}
variant=${2:?$usage}
with_serve=${3:-}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
failures=0

pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
expect() { # description command...
  local what=$1
  shift
  if "$@" >/dev/null 2>&1; then pass "$what"; else fail "$what"; fi
}
refuse() { # description command... : the command must FAIL
  local what=$1
  shift
  if "$@" >/dev/null 2>&1; then fail "$what"; else pass "$what"; fi
}
inspect() { docker inspect --format "$1" "$image"; }
check_eq() { # description actual expected
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi
}

[ "$variant" = musl ] || [ "$variant" = glibc ] || { echo "variant must be musl or glibc" >&2; exit 2; }

check_eq "runs as 10001:10001" "$(inspect '{{.Config.User}}')" "10001:10001"
check_eq "no HEALTHCHECK" "$(inspect '{{json .Config.Healthcheck}}')" "null"
check_eq "entrypoint is rustryx" "$(inspect '{{json .Config.Entrypoint}}')" '["/usr/local/bin/rustryx"]'
check_eq "default command is serve" "$(inspect '{{json .Config.Cmd}}')" '["serve"]'

if [ "$variant" = musl ]; then
  base='distroless/static'
else
  base='distroless/cc'
fi
expect "labels are present" "$here/check-image-labels.sh" "$image"
description=$(inspect '{{ index .Config.Labels "org.opencontainers.image.description" }}')
case "$description" in
  *"$variant"*) pass "label description names the $variant variant" ;;
  *) fail "label description does not name the $variant variant: $description" ;;
esac

# No shell and no package manager: executing any of them must fail.
for tool in /bin/sh /bin/bash /busybox/sh /usr/bin/apt /usr/bin/dpkg /sbin/apk; do
  refuse "no $tool in the image" docker run --rm --entrypoint "$tool" "$image" --version
done

# Binary linkage, read from the file itself.
cid=$(docker create "$image")
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true; rm -f "${tmp_bin:-}"' EXIT
tmp_bin=$(mktemp)
docker cp "$cid:/usr/local/bin/rustryx" "$tmp_bin"
if command -v file >/dev/null 2>&1; then
  linkage=$(file -b "$tmp_bin")
  if [ "$variant" = musl ]; then
    case "$linkage" in *"statically linked"*|*"static-pie linked"*) pass "binary is static" ;; *) fail "musl binary is not static: $linkage" ;; esac
  else
    case "$linkage" in *"dynamically linked"*) pass "binary is dynamically linked against glibc ($base)" ;; *) fail "glibc binary is not dynamic: $linkage" ;; esac
  fi
  case "$linkage" in *stripped*) pass "binary is stripped" ;; *) fail "binary is not stripped" ;; esac
else
  echo "skip linkage check: file(1) not installed"
fi

# Read-only root, no capabilities, no new privileges, only /tmp writable.
run_hardened() {
  docker run --rm --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m \
    --cap-drop ALL --security-opt no-new-privileges "$@"
}
expect "runs with --read-only and every capability dropped" run_hardened "$image" --version
out=$(run_hardened "$image" --version 2>&1 || true)
case "$out" in *"ead-only file system"*) fail "the process tried to write to the root filesystem" ;; *) pass "no write to the root filesystem" ;; esac

if [ "$with_serve" = "--with-serve" ]; then
  : "${RUSTRYX_TEST_DATABASE_URL:?--with-serve needs RUSTRYX_TEST_DATABASE_URL}"
  net=(--network host)
  if docker run --rm --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m --cap-drop ALL \
      "${net[@]}" -e RUSTRYX_ENVIRONMENT=development \
      -e RUSTRYX_DATABASE_URL="$RUSTRYX_TEST_DATABASE_URL" "$image" migrate; then
    pass "migrate runs on a read-only filesystem"
  else
    fail "migrate on a read-only filesystem"
  fi
  serve_id=$(docker run -d --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m --cap-drop ALL \
    "${net[@]}" -e RUSTRYX_ENVIRONMENT=development -e RUSTRYX_SERVER_LISTEN_ADDR=127.0.0.1:18081 \
    -e RUSTRYX_DATABASE_URL="$RUSTRYX_TEST_DATABASE_URL" "$image" serve)
  trap 'docker rm -f "$cid" "$serve_id" >/dev/null 2>&1 || true; rm -f "${tmp_bin:-}"' EXIT
  ready=0
  for _ in $(seq 1 30); do
    if curl -fsS http://127.0.0.1:18081/readyz >/dev/null 2>&1; then ready=1; break; fi
    sleep 1
  done
  if [ "$ready" = 1 ]; then
    pass "serve answers /readyz on a read-only filesystem"
  else
    fail "serve did not become ready"
    docker logs "$serve_id" 2>&1 | tail -20 >&2
  fi
  expect "serve answers /healthz" curl -fsS http://127.0.0.1:18081/healthz
fi

size_bytes=$(inspect '{{.Size}}')
size_mb=$((size_bytes / 1024 / 1024))
echo "image size: ${size_mb} MB (uncompressed, ${image})"
if [ -n "${IMAGE_SIZE_BUDGET_MB:-}" ]; then
  if [ "$size_mb" -le "$IMAGE_SIZE_BUDGET_MB" ]; then
    pass "size within budget of ${IMAGE_SIZE_BUDGET_MB} MB"
  else
    fail "size ${size_mb} MB exceeds budget ${IMAGE_SIZE_BUDGET_MB} MB"
  fi
fi

if [ "$failures" -eq 0 ]; then
  echo "test-image: all checks passed"
else
  echo "test-image: $failures check(s) failed" >&2
fi
[ "$failures" -eq 0 ]
