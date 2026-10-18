# syntax=docker/dockerfile:1.7
#
# MeiSync server image.
#
#   docker build -t meisync .
#   docker run -p 8080:8080 -v meisync-data:/data meisync
#
# The data directory (SQLite + YDoc snapshots/update logs) lives in the /data volume.
# Everything is configurable through the CLI flags or the MEISYNC_* environment variables.

# ---------------------------------------------------------------------------------------------
# build
# ---------------------------------------------------------------------------------------------
FROM rust:1-bookworm AS builder

WORKDIR /src

# Manifests first: a source-only change then reuses the cached dependency build.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server

# BuildKit cache mounts keep the cargo registry and target dir between builds; the binary is
# copied out inside the same layer because a cache mount is not part of the image.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked --bin meisync \
 && install -D /src/target/release/meisync /out/meisync

# ---------------------------------------------------------------------------------------------
# runtime
# ---------------------------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 meisync \
 && useradd --system --uid 10001 --gid meisync --home-dir /data --shell /usr/sbin/nologin meisync \
 && mkdir -p /data \
 && chown meisync:meisync /data

COPY --from=builder /out/meisync /usr/local/bin/meisync

USER meisync
WORKDIR /data

ENV MEISYNC_DATA_DIR=/data \
    MEISYNC_BIND=0.0.0.0:8080 \
    RUST_LOG=meisync=info,tower_http=info,warn

EXPOSE 8080
VOLUME ["/data"]

ENTRYPOINT ["meisync"]
