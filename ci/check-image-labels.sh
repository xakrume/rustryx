#!/usr/bin/env bash
# OPS-15: every image carries the eight OCI labels, with a value. A label that is
# absent or empty fails, because the Dockerfiles take version, revision and
# created from build arguments without defaults: a forgotten --build-arg shows up
# here and not in the registry.
#
# Usage: ci/check-image-labels.sh <image-reference>
# Optional: EXPECT_VERSION and EXPECT_REVISION compare the values with CI's own.
set -euo pipefail

image=${1:?usage: check-image-labels.sh <image-reference>}
prefix=org.opencontainers.image
missing=0

for key in title description source revision version created licenses vendor; do
  value=$(docker inspect --format "{{ index .Config.Labels \"$prefix.$key\" }}" "$image")
  if [ -z "$value" ]; then
    echo "check-image-labels: $prefix.$key is missing or empty on $image" >&2
    missing=$((missing + 1))
  else
    echo "$prefix.$key=$value"
  fi
done

check_equal() { # label expected
  local actual
  actual=$(docker inspect --format "{{ index .Config.Labels \"$prefix.$1\" }}" "$image")
  if [ "$actual" != "$2" ]; then
    echo "check-image-labels: $prefix.$1 is '$actual', CI expected '$2'" >&2
    missing=$((missing + 1))
  fi
}
[ -z "${EXPECT_VERSION:-}" ] || check_equal version "$EXPECT_VERSION"
[ -z "${EXPECT_REVISION:-}" ] || check_equal revision "$EXPECT_REVISION"

[ "$missing" -eq 0 ]
