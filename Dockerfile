# syntax=docker/dockerfile:1.7

# Multi-stage build that keeps the build logic in repo scripts and uses Docker
# layers only for cache boundaries:
#
#   1. native-deps  → OS packages, pinned Rust toolchain, static C/C++ prefix
#   2. rust-build   → Cargo dependency warm-up, then the real application build
#   3. runtime      → the shipped binary plus the runtime files it expects
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
COPY scripts/dev/bootstrap.sh scripts/dev/bootstrap.sh
COPY scripts/dev/install-git-hooks.sh scripts/dev/install-git-hooks.sh
COPY scripts/build/resource-limit.sh scripts/build/resource-limit.sh
COPY scripts/build/native-deps.sh scripts/build/native-deps.sh
COPY scripts/build/native/ scripts/build/native/
COPY scripts/native/ scripts/native/
COPY .githooks/ .githooks/
COPY test/native/srt-bond-client.c test/native/srt-bond-client.c
COPY test/native/srt-bond-server.c test/native/srt-bond-server.c
COPY test/native/ffmpeg-capabilities.c test/native/ffmpeg-capabilities.c

# bootstrap owns the fresh-Ubuntu dependency contract, including Node/npm
# plus npm ci for the committed frontend toolchain dependencies.
RUN scripts/dev/bootstrap.sh --skip-mediamtx

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
ENV RESTREAM_BUILD_GIT_COMMIT=source-distribution \
    RESTREAM_BUILD_TIMESTAMP=1970-01-01T00:00:00Z \
    RESTREAM_SKIP_SBOM=1

COPY scripts/build/app-native.sh scripts/build/app-native.sh

# Warm the release dependency graph without copying the real application code.
# The dummy main compiles the full dependency set into .local/build/static/cargo-target
# so ordinary src/ edits only need to rebuild our crate in the next layer.
COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo/ .cargo/
RUN mkdir -p benches src \
    && awk '/^\[\[bench\]\]$/ { in_bench = 1; next } in_bench && /^name = "/ { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); printf "fn main() {}\\n" > ("benches/" name ".rs"); in_bench = 0 }' Cargo.toml \
    && printf 'fn main() {}\n' > src/main.rs
RUN RESTREAM_BUILD_PROFILE=release scripts/build/resource-limit.sh ./scripts/build/app-native.sh

# Inner-loop layer: copy the actual application sources, then bring in the
# built frontend assets from the frontend stage. Rust-only edits therefore skip
# frontend rebuilds, while frontend edits reuse the warmed Cargo dependency
# target directory above.
COPY src/ src/
COPY --from=frontend-build /workspace/public public
COPY --from=native-deps /workspace/public/bin/ffmpeg public/bin/ffmpeg
RUN RESTREAM_BUILD_PROFILE=release scripts/build/resource-limit.sh ./scripts/build/app-native.sh

# Build the minimal filesystem tree that the shipped binary expects at runtime.
# /tmp must remain writable and executable because the embedded FFmpeg binary is
# extracted there on startup; operators should still prefer --tmpfs /tmp:exec.
RUN mkdir -p \
        /runtime/data \
        /runtime/etc/ssl/certs \
        /runtime/media \
        /runtime/tmp/logs \
        /runtime/usr/share/zoneinfo \
    && cp -a /usr/share/zoneinfo/. /runtime/usr/share/zoneinfo/ \
    && cp -a /etc/localtime /runtime/etc/localtime \
    && cp /etc/ssl/certs/ca-certificates.crt /runtime/etc/ssl/certs/ca-certificates.crt \
    && cp /etc/nsswitch.conf /runtime/etc/nsswitch.conf \
    && cp /etc/protocols /runtime/etc/protocols \
    && cp /etc/services /runtime/etc/services \
    && cp -L /etc/resolv.conf /runtime/etc/resolv.conf \
    && printf 'restream:x:1000:1000:restream:/nonexistent:/sbin/nologin\n' > /runtime/etc/passwd \
    && printf 'restream:x:1000:\n' > /runtime/etc/group \
    && chmod 1777 /runtime/tmp \
    && chown -R 1000:1000 /runtime/data /runtime/media /runtime/tmp

# ── Stage 4: minimal runtime ─────────────────────────────────────────────────
#
# Runtime requirements:
#   /tmp    exec-enabled writable tmpfs for embedded FFmpeg extraction
#   /data   SQLite database persistence
#   /media  HLS/media persistence
#
# Example:
#   docker run -d \
#     --tmpfs /tmp:exec,mode=1777 \
#     -v restream-db:/data \
#     -v restream-media:/media \
#     -p 3030:3030 -p 1935:1935 -p 10080:10080/udp \
#     restream:scratch
FROM ubuntu:24.04

COPY --from=rust-build /runtime/ /
COPY --from=rust-build /workspace/target/release/restream /restream

EXPOSE 3030 1935 10080/udp

USER 1000:1000

ENV RESTREAM_DB_PATH=/data/restream.db \
    RESTREAM_MEDIA_DIR=/media \
    RESTREAM_LOG_DIR=/tmp/logs \
    RESTREAM_HTTP_BIND_ADDR=0.0.0.0

ENTRYPOINT ["/restream"]
