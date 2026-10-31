# syntax=docker/dockerfile:1.7

# Multi-stage build that keeps the build logic in `cargo xtask` and uses Docker
# layers only for cache boundaries:
#
#   1. toolchain    → OS packages and the pinned Rust toolchain
#   2. rust-build   → Cargo dependency warm-up, then the real application build
#   3. runtime tree  → the shipped binaries
#   4. runtime       → a distroless production image
#   5. harness       → a minimal Ubuntu image for live protocol validation
#
# The Rust toolchain bootstrap comes from scripts/dev/bootstrap.sh and the
# application build from `cargo xtask release app-native`. The engine links no media
# library: FFmpeg is a separate executable (the ffmpeg-fetch stage below).

# ── Stage 1: toolchain ───────────────────────────────────────────────────────
#
# Everything here changes rarely:
#   - fresh-Ubuntu bootstrap packages
#   - pinned Rust toolchain
#
# Docker intentionally does not duplicate package or toolchain logic here.
# scripts/dev/bootstrap.sh is the source of truth for what a fresh Ubuntu 24.04
# machine needs, and Docker consumes that script directly.
FROM ubuntu:24.04 AS toolchain

ENV DEBIAN_FRONTEND=noninteractive

WORKDIR /workspace
ENV RESTREAM_REPO_ROOT=/workspace

# Only copy inputs that affect bootstrap so app source edits do not invalidate
# this stage.
COPY package.json package-lock.json ./
COPY rust-toolchain.toml rust-toolchain.toml
# Copy only the bootstrap owners: copying all of scripts/ would make unrelated
# helper edits rebuild the toolchain.
COPY scripts/data/debian-packages.tsv scripts/data/
COPY scripts/ci/install-system-deps.sh scripts/ci/
COPY scripts/dev/bootstrap.sh scripts/dev/bootstrap-runtime.sh scripts/dev/

# bootstrap owns the fresh-Ubuntu dependency contract, including Node/npm
# plus npm ci for the committed frontend toolchain dependencies.
RUN scripts/dev/bootstrap.sh --skip-mediamtx --skip-harness-host-check

ENV PATH="/root/.cargo/bin:${PATH}"

# ── Stage 2: frontend-build ──────────────────────────────────────────────────
#
# Frontend edits should have their own cache boundary. This stage reuses the
# Node/npm + node_modules state prepared by bootstrap.sh, then rebuilds the
# generated browser assets under public/.
FROM toolchain AS frontend-build

WORKDIR /workspace

COPY web/ web/
COPY scripts/dev/frontend/prepare-assets.mjs scripts/dev/frontend/prepare-assets.mjs
COPY tsconfig.json tsconfig.json
COPY vite.config.ts vite.config.ts

RUN npm run build:frontend

# ── Stage 3: rust-build ──────────────────────────────────────────────────────
#
# This stage is split into two cache boundaries:
#   - manifest/config only + dummy src/main.rs → compile Cargo dependencies
#   - real src/ + built public/ assets         → relink just the app crate
#
# The warm layer is a plain dependency build; the real build runs
# `cargo xtask release app-native` so Docker never re-implements the linkage
# verification.

FROM toolchain AS rust-build

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

# Warm the release dependency graph without copying the real application code.
# The dummy main compiles the full dependency set into target/
# so ordinary src/ edits only need to rebuild our crate in the next layer.
COPY Cargo.toml Cargo.lock build.rs build_identity.rs ./
COPY .cargo/ .cargo/
# `[patch.crates-io]` path dependencies must exist before Cargo can resolve the
# graph (tests/docker_build_recipe.rs enforces this).
COPY vendor/ vendor/
# Path workspace members: stage each manifest with a dummy lib so the warm
# layer's workspace loads (tests/docker_build_recipe.rs enforces this).
COPY crates/xtask/Cargo.toml crates/xtask/Cargo.toml
COPY crates/restream-agent/Cargo.toml crates/restream-agent/Cargo.toml
COPY crates/restream-agent-contract/Cargo.toml crates/restream-agent-contract/Cargo.toml
COPY crates/restream-core/Cargo.toml crates/restream-core/Cargo.toml
COPY crates/restream-domain/Cargo.toml crates/restream-domain/Cargo.toml
COPY crates/restream-domain/Cargo.toml crates/restream-domain/Cargo.toml
COPY crates/restream-engine/Cargo.toml crates/restream-engine/Cargo.toml
COPY crates/restream-mcp/Cargo.toml crates/restream-mcp/Cargo.toml
COPY crates/restream-health/Cargo.toml crates/restream-health/Cargo.toml
COPY crates/restream-sync/Cargo.toml crates/restream-sync/Cargo.toml
COPY crates/restream-mpegts/Cargo.toml crates/restream-mpegts/Cargo.toml
COPY crates/restream-rtmp/Cargo.toml crates/restream-rtmp/Cargo.toml
COPY crates/restream-moq/Cargo.toml crates/restream-moq/Cargo.toml
COPY crates/restream-srt/Cargo.toml crates/restream-srt/Cargo.toml
COPY crates/test_harness/Cargo.toml crates/test_harness/Cargo.toml
RUN mkdir -p benches src crates/xtask/src crates/restream-agent/src crates/restream-agent-contract/src crates/restream-core/src crates/restream-domain/src crates/restream-engine/src crates/restream-engine/benches crates/restream-health/src crates/restream-mcp/src crates/restream-sync/src crates/restream-mpegts/src crates/restream-rtmp/src crates/restream-moq/src crates/restream-srt/src crates/test_harness/src \
    && awk '/^\[\[bench\]\]$/ { in_bench = 1; next } in_bench && /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); printf "fn main() {}\\n" > ("benches/" name ".rs"); in_bench = 0 }' Cargo.toml \
    && awk '/^\[\[bench\]\]$/ { in_bench = 1; next } in_bench && /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); printf "fn main() {}\\n" > ("crates/restream-engine/benches/" name ".rs"); in_bench = 0 }' crates/restream-engine/Cargo.toml \
    && printf 'fn main() {}\n' > src/main.rs \
    && : > crates/xtask/src/lib.rs \
    && : > crates/restream-agent/src/lib.rs \
    && : > crates/restream-agent-contract/src/lib.rs \
    && : > crates/restream-core/src/lib.rs \
    && : > crates/restream-domain/src/lib.rs \
    && : > crates/restream-domain/src/lib.rs \
    && : > crates/restream-engine/src/lib.rs \
    && : > crates/restream-health/src/lib.rs \
    && : > crates/restream-mcp/src/lib.rs \
    && printf 'fn main() {}\n' > crates/restream-mcp/src/main.rs \
    && : > crates/restream-sync/src/lib.rs \
    && : > crates/restream-mpegts/src/lib.rs \
    && : > crates/restream-rtmp/src/lib.rs \
    && : > crates/restream-moq/src/lib.rs \
    && : > crates/restream-srt/src/lib.rs \
    && : > crates/test_harness/src/lib.rs \
    && printf 'fn main() {}\n' > crates/test_harness/src/main.rs
RUN cargo build --release --bin restream && cargo build -p xtask

# Return to the application build stage for its runtime filesystem assembly.
FROM rust-build AS runtime-tree

# Inner-loop layer: copy the actual application sources, then bring in the
# built frontend assets from the frontend stage. Rust-only edits therefore skip
# frontend rebuilds, while frontend edits reuse the warmed Cargo dependency
# target directory above.
COPY src/ src/
COPY crates/xtask/ crates/xtask/
COPY crates/restream-agent/ crates/restream-agent/
COPY crates/restream-agent-contract/ crates/restream-agent-contract/
COPY crates/restream-core/ crates/restream-core/
COPY crates/restream-domain/ crates/restream-domain/
COPY crates/restream-domain/ crates/restream-domain/
COPY crates/restream-engine/ crates/restream-engine/
COPY crates/restream-health/ crates/restream-health/
COPY crates/restream-mcp/ crates/restream-mcp/
COPY crates/restream-sync/ crates/restream-sync/
COPY crates/restream-mpegts/ crates/restream-mpegts/
COPY crates/restream-rtmp/ crates/restream-rtmp/
COPY crates/restream-moq/ crates/restream-moq/
COPY crates/restream-srt/ crates/restream-srt/
COPY crates/test_harness/ crates/test_harness/
COPY --from=frontend-build /workspace/public public
# COPY keeps each file's original (checkout-time) mtime, which is OLDER than the
# dummy sources the warm layer compiled, so Cargo would treat the warmed dummy
# artifacts (a stub main, an empty member lib) as up to date. Touch the real
# sources so the final build compiles them.
RUN find crates src -type f -name '*.rs' -exec touch {} + \
    && RESTREAM_BUILD_PROFILE=release cargo xtask release app-native

# The harness image is an explicit target, so this extra bench build is paid
# only by `--target harness`, never by the production runtime image. It must
# derive from `runtime-tree`, which has the real source and generated frontend
# assets rather than the dummy dependency-warmup crate.
FROM runtime-tree AS harness-build

RUN cargo xtask build-bench

# Seed the state directory as a copyable path; WORKDIR alone did not make it
# writable by UID 1000 in rootless container engines.
FROM ubuntu:24.04 AS runtime-state
RUN mkdir -p /restream-state && touch /restream-state/.keep

# FFmpeg is not linked into the engine; images carry the BtbN build that
# `restream ffmpeg-fetch` installs: the same release and SHA-256 digests as
# crates/restream-engine/src/ffmpeg_binary.rs (tests/docker_build_recipe.rs keeps them equal).
FROM ubuntu:24.04 AS ffmpeg-fetch
ARG TARGETARCH
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
RUN case "${TARGETARCH:-amd64}" in \
        amd64) asset=ffmpeg-n9.0.2-17-g2a571b6068-linux64-gpl-9.0.tar.xz \
            sha256=68ee646831adaae2495618346f3bba94ff207ff83bbd34d643e7004730d66269 ;; \
        arm64) asset=ffmpeg-n9.0.2-17-g2a571b6068-linuxarm64-gpl-9.0.tar.xz \
            sha256=91afcd7695d4bbbb9e0057aa10baaf9875ec4e1a97303f9059ec971336a2ab46 ;; \
        *) echo "no FFmpeg build for ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && cd /tmp \
    && curl -fsSLO "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/$asset" \
    && echo "$sha256  $asset" | sha256sum -c - \
    && tar -xJf "$asset" --strip-components=2 --wildcards '*/bin/ffmpeg' '*/bin/ffprobe' \
    && install -m 0755 ffmpeg /usr/local/bin/ffmpeg \
    && install -m 0755 ffprobe /usr/local/bin/ffprobe \
    && rm -f "$asset" ffmpeg ffprobe

# ── Stage 4: distroless runtime ──────────────────────────────────────────────
#
# Runtime state lives under `/.restream`, seeded with UID 1000 so the app can
# create its data/logs/media/runtime children at startup.
#
# Example:
#   docker run -d \
#     -v restream-state:/.restream \
#     -p 3030:3030 -p 1935:1935 -p 10080:10080/udp \
#     restream:distroless
FROM gcr.io/distroless/cc-debian13:nonroot AS runtime-base

ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND Apache-2.0 AND GPL-3.0-or-later"

EXPOSE 3030 1935 10080/udp

COPY --from=runtime-state --chown=1000:1000 /restream-state /.restream
COPY --from=ffmpeg-fetch /usr/local/bin/ffmpeg /usr/local/bin/ffmpeg
COPY --from=ffmpeg-fetch /usr/local/bin/ffprobe /usr/local/bin/ffprobe
ENV FFMPEG_BIN_PATH=/usr/local/bin/ffmpeg FFPROBE_BIN_PATH=/usr/local/bin/ffprobe

USER 1000:1000

WORKDIR /

ENV RESTREAM_HTTP_BIND_ADDR=0.0.0.0

ENTRYPOINT ["/restream"]

FROM runtime-base AS runtime-distroless

COPY --from=runtime-tree /workspace/target/release/restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

# Release packaging uses the same final image recipe without rebuilding Rust:
# `cargo xtask release package-binaries` supplies the already-built restream
# binary as the `release_payload` build context.
FROM runtime-base AS runtime-artifact

COPY --from=release_payload /restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

# Keep the old distro runtime available only as an escape hatch for operators
# with an unusual NSS/DNS integration. The default production target below is
# the verified distroless runtime above.
FROM ubuntu:24.04 AS runtime-ubuntu

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates netbase tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY --from=ffmpeg-fetch /usr/local/bin/ffmpeg /usr/local/bin/ffmpeg
COPY --from=ffmpeg-fetch /usr/local/bin/ffprobe /usr/local/bin/ffprobe
ENV FFMPEG_BIN_PATH=/usr/local/bin/ffmpeg FFPROBE_BIN_PATH=/usr/local/bin/ffprobe

COPY --from=runtime-tree /workspace/target/release/restream /restream
COPY distribution/ /usr/share/doc/restream/distribution/

ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND Apache-2.0 AND GPL-3.0-or-later"

EXPOSE 3030 1935 10080/udp

COPY --from=runtime-state --chown=1000:1000 /restream-state /.restream

USER 1000:1000
WORKDIR /
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
COPY scripts/data/debian-packages.tsv scripts/data/
COPY scripts/ci/install-system-deps.sh scripts/ci/
COPY scripts/dev/bootstrap-runtime.sh scripts/dev/
RUN scripts/dev/bootstrap-runtime.sh --skip-harness-host-check

# ── Stage 6: live-harness image ──────────────────────────────────────────────
#
# Build explicitly with the same provenance args documented in README.md:
# `docker build --build-arg RESTREAM_BUILD_GIT_COMMIT=... --build-arg RESTREAM_BUILD_TIMESTAMP=... --target harness -t restream:harness .`.
# It contains every generated executable used in live validation (`restream`,
# bench-profile `test_harness`; FFmpeg comes from the runtime's ffmpeg package), the pinned
# MediaMTX peer, committed fixtures, and only the OS tools the harness invokes.
FROM ci-harness-runtime AS harness

WORKDIR /workspace
ARG RESTREAM_BUILD_GIT_COMMIT
ARG RESTREAM_BUILD_TIMESTAMP
LABEL org.opencontainers.image.source="https://github.com/krsna1729/restream" \
    org.opencontainers.image.revision="${RESTREAM_BUILD_GIT_COMMIT}" \
    org.opencontainers.image.created="${RESTREAM_BUILD_TIMESTAMP}" \
    org.opencontainers.image.licenses="MIT AND Apache-2.0 AND GPL-3.0-or-later"
COPY --from=harness-build /workspace/target/bench/restream /workspace/target/bench/restream
COPY --from=harness-build /workspace/target/bench/test_harness /workspace/target/bench/test_harness
COPY test/fixtures/ test/fixtures/
COPY test/harness/ test/harness/
COPY distribution/ /usr/share/doc/restream/distribution/

ENV PATH="/workspace/target/bench:${PATH}" \
    RESTREAM_REPO_ROOT=/workspace

# Make the explicitly selected target directly runnable. Individual harness
# modes remain ordinary arguments, including `--no-netns` for host networking.
ENTRYPOINT ["/workspace/target/bench/test_harness"]

# Keep the production distroless runtime as Docker's implicit final target. The
# harness remains an explicit `--target harness` validation image; otherwise a
# plain `docker build .` would silently ship the Ubuntu test environment.
FROM runtime-distroless AS runtime
