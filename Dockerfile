# syntax=docker/dockerfile:1.7

# Multi-stage build that keeps the build logic in repo scripts and uses Docker
# layers only for cache boundaries:
#
#   1. native-deps  → OS packages, pinned Rust toolchain, static C/C++ prefix
#   2. rust-build   → Cargo dependency warm-up, then the real application build
#   3. runtime tree  → the shipped binaries plus their exact glibc closure
#   4. runtime       → a pure-scratch production image
#   5. harness       → a minimal Ubuntu image for live protocol validation
#
# The native/static artifacts come from scripts/build/native-deps.sh, the Rust
# toolchain bootstrap comes from scripts/dev/bootstrap.sh, and the final static
# binary build comes from scripts/build/app-native.sh.

# ── Stage 1: native-deps ─────────────────────────────────────────────────────
#
# Everything here changes rarely and is expensive to rebuild:
#   - fresh-Ubuntu bootstrap packages
#   - pinned Rust toolchain
#   - static SRT/FFmpeg/x264/x265 prefix under .local/build/static/
#
# Docker intentionally does not duplicate package or toolchain logic here.
# scripts/dev/bootstrap.sh is the source of truth for what a fresh Ubuntu 24.04
# machine needs, and Docker consumes that script directly.
FROM ubuntu:24.04 AS native-deps

ENV DEBIAN_FRONTEND=noninteractive

WORKDIR /workspace
ENV RESTREAM_REPO_ROOT=/workspace

# Only copy inputs that affect bootstrap/native compilation so app source edits
# do not invalidate this expensive stage.
COPY package.json package-lock.json ./
COPY rust-toolchain.toml rust-toolchain.toml
COPY scripts/lib/ scripts/lib/
# Keep this stage script-aware but not script-global: copying all of scripts/
# would make unrelated release/harness/frontend helper edits rebuild the static
# native prefix. Add only bootstrap/native-build owners here.
COPY scripts/dev/bootstrap.sh scripts/dev/harness-host-prereqs.sh scripts/dev/install-git-hooks.sh scripts/dev/
COPY scripts/build/resource-limit.sh scripts/build/native-deps.sh scripts/build/
COPY scripts/build/native/ scripts/build/native/
COPY scripts/native/ scripts/native/
COPY .githooks/ .githooks/
COPY test/native/srt-bond-client.c test/native/srt-bond-client.c
COPY test/native/srt-bond-server.c test/native/srt-bond-server.c
COPY test/native/ffmpeg-capabilities.c test/native/ffmpeg-capabilities.c

# bootstrap owns the fresh-Ubuntu dependency contract, including Node/npm
# plus npm ci for the committed frontend toolchain dependencies.
RUN scripts/dev/bootstrap.sh --skip-mediamtx --skip-harness-host-check

ENV PATH="/root/.cargo/bin:${PATH}"

# ── Stage 2: frontend-build ──────────────────────────────────────────────────
#
# Frontend edits should have their own cache boundary. This stage reuses the
# Node/npm + node_modules state prepared by bootstrap.sh, then rebuilds the
# generated browser assets under public/.
FROM native-deps AS frontend-build

WORKDIR /workspace

COPY web/ web/
COPY scripts/dev/frontend/prepare-assets.mjs scripts/dev/frontend/prepare-assets.mjs
COPY tsconfig.json tsconfig.json

RUN npm run build:frontend

# ── Stage 3: rust-build ──────────────────────────────────────────────────────
#
# This stage is split into two cache boundaries:
#   - manifest/config only + dummy src/main.rs → compile Cargo dependencies
#   - real src/ + built public/ assets         → relink just the app crate
#
# scripts/build/app-native.sh is used for both so Docker never re-implements the
# static-link flags or verification logic.

FROM native-deps AS rust-build

WORKDIR /workspace
# Release provenance is supplied by the caller because `.git/` is deliberately
# excluded from the build context. Keeping these arguments mandatory prevents a
# published image from silently claiming a synthetic commit or Unix epoch.
ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
RUN test -n "$RESTREAM_BUILD_GIT_COMMIT" \
    && test -n "$RESTREAM_BUILD_TIMESTAMP"
ENV RESTREAM_BUILD_GIT_COMMIT=${RESTREAM_BUILD_GIT_COMMIT} \
    RESTREAM_BUILD_TIMESTAMP=${RESTREAM_BUILD_TIMESTAMP} \
    RESTREAM_SKIP_SBOM=1

COPY scripts/build/app-native.sh scripts/build/bench-harness.sh scripts/build/emit-sbom.sh scripts/build/

# Warm the release dependency graph without copying the real application code.
# The dummy main compiles the full dependency set into .local/build/static/cargo-target
# so ordinary src/ edits only need to rebuild our crate in the next layer.
COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo/ .cargo/
RUN mkdir -p benches src \
    && awk '/^\[\[bench\]\]$/ { in_bench = 1; next } in_bench && /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); printf "fn main() {}\\n" > ("benches/" name ".rs"); in_bench = 0 }' Cargo.toml \
    && printf 'fn main() {}\n' > src/main.rs
RUN RESTREAM_BUILD_PROFILE=release scripts/build/resource-limit.sh ./scripts/build/app-native.sh

# Return to the application build stage for its runtime filesystem assembly.
FROM rust-build AS runtime-tree

# Inner-loop layer: copy the actual application sources, then bring in the
# built frontend assets from the frontend stage. Rust-only edits therefore skip
# frontend rebuilds, while frontend edits reuse the warmed Cargo dependency
# target directory above.
COPY src/ src/
COPY --from=frontend-build /workspace/public public
COPY --from=native-deps /workspace/public/bin/ffmpeg public/bin/ffmpeg
RUN RESTREAM_BUILD_PROFILE=release scripts/build/resource-limit.sh ./scripts/build/app-native.sh

RUN set -eux; \
    restream_home="/runtime/.restream"; \
    install -d -m 0755 -o 0 -g 0 \
        "$restream_home" \
        /runtime/etc/ssl/certs \
        /runtime/usr/share/zoneinfo; \
    install -d -m 0700 -o 1000 -g 1000 \
        "$restream_home/runtime" \
        "$restream_home/data" \
        "$restream_home/media" \
        "$restream_home/logs"; \
    cp -a /usr/share/zoneinfo/. /runtime/usr/share/zoneinfo/; \
    cp -a /etc/localtime /runtime/etc/localtime; \
    cp /etc/ssl/certs/ca-certificates.crt /runtime/etc/ssl/certs/ca-certificates.crt; \
    cp /etc/nsswitch.conf /runtime/etc/nsswitch.conf; \
    cp /etc/protocols /runtime/etc/protocols; \
    cp /etc/services /runtime/etc/services; \
    cp -L /etc/resolv.conf /runtime/etc/resolv.conf; \
    printf 'restream:x:1000:1000:restream:/nonexistent:/sbin/nologin\n' > /runtime/etc/passwd; \
    printf 'restream:x:1000:\n' > /runtime/etc/group; \
    ldd /workspace/target/release/restream \
        | awk '$3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' \
        | sort -u \
        | while IFS= read -r library; do cp --parents -L "$library" /runtime; done; \
    test -e /runtime/lib64/ld-linux-x86-64.so.2

# The harness image is an explicit target, so this extra bench build is paid
# only by `--target harness`, never by the production scratch image. It must
# derive from `runtime-tree`, which has the real source and generated frontend
# assets rather than the dummy dependency-warmup crate.
FROM runtime-tree AS harness-build

RUN scripts/build/bench-harness.sh

# ── Stage 4: pure-scratch runtime ────────────────────────────────────────────
#
# Runtime requirements:
#   /.restream/data     SQLite database persistence (including WAL/SHM sidecars)
#   /.restream/logs     rotated JSON process logs
#   /.restream/media    uploaded media and recordings
#   /.restream/runtime  internal embedded-FFmpeg cache; the runtime needs no `/tmp` directory
#
# Example:
#   docker run -d \
#     -v restream-state:/.restream \
#     -p 3030:3030 -p 1935:1935 -p 10080:10080/udp \
#     restream:scratch
FROM scratch AS runtime-scratch

# Stage-to-stage COPY preserves the rootfs ownership set above.
COPY --from=runtime-tree /runtime/ /
COPY --from=runtime-tree /workspace/target/release/restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later AND MPL-2.0 AND Apache-2.0"

EXPOSE 3030 1935 10080/udp

USER 1000:1000

ENV RESTREAM_HTTP_BIND_ADDR=0.0.0.0

ENTRYPOINT ["/restream"]

# Release packaging uses the same final image recipe without rebuilding Rust:
# scripts/release/package-runtime-image.sh supplies an already-built restream
# binary as the `release_payload` build context.
FROM ubuntu:24.04 AS runtime-artifact-rootfs

ENV DEBIAN_FRONTEND=noninteractive
WORKDIR /workspace
COPY --from=release_payload /restream /workspace/restream
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates netbase tzdata; \
    rm -rf /var/lib/apt/lists/*; \
    restream_home="/runtime/.restream"; \
    install -d -m 0755 -o 0 -g 0 \
        "$restream_home" \
        /runtime/etc/ssl/certs \
        /runtime/usr/share/zoneinfo; \
    install -d -m 0700 -o 1000 -g 1000 \
        "$restream_home/runtime" \
        "$restream_home/data" \
        "$restream_home/media" \
        "$restream_home/logs"; \
    cp -a /usr/share/zoneinfo/. /runtime/usr/share/zoneinfo/; \
    cp -a /etc/localtime /runtime/etc/localtime; \
    cp /etc/ssl/certs/ca-certificates.crt /runtime/etc/ssl/certs/ca-certificates.crt; \
    cp /etc/nsswitch.conf /runtime/etc/nsswitch.conf; \
    cp /etc/protocols /runtime/etc/protocols; \
    cp /etc/services /runtime/etc/services; \
    cp -L /etc/resolv.conf /runtime/etc/resolv.conf; \
    printf 'restream:x:1000:1000:restream:/nonexistent:/sbin/nologin\n' > /runtime/etc/passwd; \
    printf 'restream:x:1000:\n' > /runtime/etc/group; \
    ldd /workspace/restream \
        | awk '$3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' \
        | sort -u \
        | while IFS= read -r library; do cp --parents -L "$library" /runtime; done; \
    test -e /runtime/lib64/ld-linux-x86-64.so.2

FROM scratch AS runtime-artifact

# Stage-to-stage COPY preserves the rootfs ownership set above.
COPY --from=runtime-artifact-rootfs /runtime/ /
COPY --from=release_payload /restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later AND MPL-2.0 AND Apache-2.0"

EXPOSE 3030 1935 10080/udp

USER 1000:1000

ENV RESTREAM_HTTP_BIND_ADDR=0.0.0.0

ENTRYPOINT ["/restream"]

# Keep the old distro runtime available only as an escape hatch for operators
# with an unusual NSS/DNS integration. The default production target below is
# the verified scratch runtime above.
FROM ubuntu:24.04 AS runtime-ubuntu

COPY --from=runtime-tree /runtime/ /
COPY --from=runtime-tree /workspace/target/release/restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later AND MPL-2.0 AND Apache-2.0"

EXPOSE 3030 1935 10080/udp
USER 1000:1000
ENV RESTREAM_HTTP_BIND_ADDR=0.0.0.0
ENTRYPOINT ["/restream"]

# ── Stage 5: CI/live-harness runtime images ──────────────────────────────────
#
# This target is intentionally source-light: it carries only the host tools the
# live harness invokes (FFmpeg/ffprobe, MediaMTX, networking utilities, SQLite,
# certificates). GitHub release shards mount a checked-out repo plus prepared
# bench binaries into this image, so package installation happens once when the
# CI runtime image is published instead of once per matrix shard.
FROM ubuntu:24.04 AS ci-harness-runtime

WORKDIR /workspace
ENV RESTREAM_REPO_ROOT=/workspace
COPY scripts/lib/ scripts/lib/
COPY scripts/dev/bootstrap-runtime.sh scripts/dev/harness-host-prereqs.sh scripts/dev/
RUN scripts/dev/bootstrap-runtime.sh --skip-harness-host-check

# ── Stage 6: live-harness image ──────────────────────────────────────────────
#
# Build explicitly with the same provenance args documented in README.md:
# `docker build --build-arg RESTREAM_BUILD_GIT_COMMIT=... --build-arg RESTREAM_BUILD_TIMESTAMP=... --target harness -t restream:harness .`.
# It contains every generated executable used in live validation (`restream`,
# bench-profile `test_harness`, and the embedded static FFmpeg), the pinned
# MediaMTX peer, committed fixtures, and only the OS tools the harness invokes.
FROM ci-harness-runtime AS harness

WORKDIR /workspace
ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later AND MPL-2.0 AND Apache-2.0"
COPY --from=harness-build /workspace/target/bench/restream /workspace/target/bench/restream
COPY --from=harness-build /workspace/target/bench/test_harness /workspace/target/bench/test_harness
COPY --from=harness-build /workspace/public/bin/ffmpeg /workspace/public/bin/ffmpeg
COPY test/fixtures/ test/fixtures/
COPY test/harness/ test/harness/
COPY distribution/ /usr/share/doc/restream/distribution/

ENV PATH="/workspace/target/bench:${PATH}" \
    RESTREAM_REPO_ROOT=/workspace

# Make the explicitly selected target directly runnable. Individual harness
# modes remain ordinary arguments, including `--no-netns` for host networking.
ENTRYPOINT ["/workspace/target/bench/test_harness"]

# Keep the production scratch runtime as Docker's implicit final target. The
# harness remains an explicit `--target harness` validation image; otherwise a
# plain `docker build .` would silently ship the Ubuntu test environment.
FROM runtime-scratch AS runtime
