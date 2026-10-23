# Rustryx image, glibc variant: Debian 13 builder, gcr.io/distroless/cc runtime.
# The static musl variant is Dockerfile.musl; the two are published as separate
# images with separate tags and never share a tag (README 7, OPS-03).
#
# Build:
#   docker build -f Dockerfile \
#     --build-arg VERSION=1.2.3 --build-arg REVISION=$(git rev-parse HEAD) \
#     --build-arg CREATED=$(date -u +%Y-%m-%dT%H:%M:%SZ) -t rustryx:1.2.3-glibc .
# Debug symbols (separate artifact for core dump analysis, OPS-02):
#   docker build -f Dockerfile --target debug-symbols --output type=local,dest=out .
#
# Every FROM is pinned by digest (OPS-01, DSO-18). The tag next to the digest is
# for humans and for dependabot/renovate; the digest is what is pulled. The
# compiler version in the tag must equal rust-toolchain.toml, enforced by
# ci/check-toolchain-pin.sh.

FROM rust:1.99.0-trixie@sha256:81015672d6939b0265f09adf96d5de125482bb1330106076956b0518e74edc90 AS builder

# The image ships rustup and the pinned compiler. rust-toolchain.toml also asks
# for rustfmt, clippy and llvm-tools, which a release build does not need and
# which rustup would download on the first cargo call. Naming the toolchain in
# the environment overrides the file and keeps the build offline from rustup.
ENV RUSTUP_TOOLCHAIN=1.99.0
# Absolute paths of the build container must not leak into the binary, otherwise
# two builds on different runners differ (OPS-80). RUSTFLAGS replaces any
# rustflags of .cargo/config.toml, so none may be added there for release builds.
ENV RUSTFLAGS="--remap-path-prefix=/src=/rustryx --remap-path-prefix=/usr/local/cargo=/cargo"

# cargo-auditable writes the dependency list of the build into the binary (a
# linker section). Without it a scanner sees nothing of the Rust crates inside
# the image: Trivy reports only the handful of OS packages and the scan is green
# while checking none of the code we ship (DSO-00, OPS-25). It wraps cargo, so the
# build is still `cargo build --locked --release`.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install --locked --version 0.7.7 cargo-auditable

WORKDIR /src
COPY . .

# --locked: a Cargo.lock that does not match the manifests fails the build
# instead of being silently rewritten (OPS-01).
# Debug info is produced only to be split into rustryx.debug; the shipped binary
# is stripped afterwards, so the code is the same as with strip = "symbols".
# The cache mounts are per builder and per architecture, so x86_64 and aarch64
# caches never mix (OPS-05).
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    set -eu; \
    CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
      cargo auditable build --locked --release -p rustryx-server; \
    test -x target/release/rustryx || { echo "no target/release/rustryx: crates/rustryx-server needs [[bin]] name = \"rustryx\"" >&2; exit 1; }; \
    mkdir -p /out/data; \
    cp target/release/rustryx /out/rustryx.full; \
    objcopy --only-keep-debug /out/rustryx.full /out/rustryx.debug; \
    strip --strip-all -o /out/rustryx /out/rustryx.full; \
    objcopy --add-gnu-debuglink=/out/rustryx.debug /out/rustryx; \
    rm /out/rustryx.full

FROM scratch AS debug-symbols
COPY --from=builder /out/rustryx.debug /rustryx.debug

# distroless/cc: glibc, libgcc, libstdc++, CA certificates, tzdata. No shell, no
# package manager (OPS-10). The binary links glibc dynamically, which is why this
# variant needs cc and not static.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf AS runtime

# Provided by CI, deliberately WITHOUT defaults: an image built without them gets
# empty labels and ci/check-image-labels.sh fails (OPS-15).
ARG VERSION
ARG REVISION
ARG CREATED
ARG SOURCE=https://github.com/rustryx/rustryx

LABEL org.opencontainers.image.title="rustryx" \
      org.opencontainers.image.description="Rustryx artifact manager (glibc, distroless/cc)" \
      org.opencontainers.image.source="${SOURCE}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.vendor="Rustryx project"

COPY LICENSE /licenses/rustryx/LICENSE
COPY --from=builder /out/rustryx /usr/local/bin/rustryx
# The data directory exists and belongs to the runtime user so a local blob store
# mounted here works; every other path of the root filesystem is read-only and
# /tmp is an emptyDir in Kubernetes (OPS-12). It cannot be created later: the
# image has no shell.
COPY --from=builder --chown=10001:10001 /out/data /var/lib/rustryx

# Numeric UID and GID: Kubernetes checks runAsNonRoot against numbers, and the
# distroless passwd file has no entry for 10001.
USER 10001:10001
WORKDIR /var/lib/rustryx
# The single HTTP listener (API, formats, probes, metrics) of server.listen_addr.
EXPOSE 8081
# No HEALTHCHECK on purpose: probes belong to Kubernetes (OPS-14).
ENTRYPOINT ["/usr/local/bin/rustryx"]
CMD ["serve"]
