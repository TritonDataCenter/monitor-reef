# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

# syntax=docker/dockerfile:1.7

# tritond container image.
#
# Multi-stage build: Rust toolchain matching rust-toolchain.toml in the
# builder stage, debian-bookworm-slim runtime. The runtime image is
# intended to be the same artifact deployed inside SmartOS LX-branded
# zones; Linux Docker is a side benefit for development.
#
# Build context: the *parent* of this directory (triton-vnext/), so
# sibling repos with path-deps from monitor-reef's Cargo.toml
# (manta-storage/, proteus/) are reachable to cargo. The matching
# docker-compose.yml in this directory sets `build.context: ..` and
# `build.dockerfile: monitor-reef/Dockerfile`.
#
# Build via compose:  docker compose build tritond
# Build standalone:   docker build -f monitor-reef/Dockerfile \
#                                  -t tritond:dev \
#                                  /path/to/triton-vnext
# Run:                docker run --rm -p 8080:8080 tritond:dev
# Compose up:         docker compose up

FROM rust:1.92-bookworm AS builder

# FoundationDB client library version. Must match the FDB server in
# docker-compose. embedded-fdb-include lets the foundationdb crate
# build with only libfdb_c.so present (no headers needed).
ARG FDB_VERSION=7.3.27

RUN set -eux; \
    arch=$(dpkg --print-architecture); \
    apt-get update; \
    apt-get install -y --no-install-recommends \
      curl ca-certificates \
      clang libclang-dev; \
    curl -fsSL -o /tmp/fdb-clients.deb \
      "https://github.com/apple/foundationdb/releases/download/${FDB_VERSION}/foundationdb-clients_${FDB_VERSION}-1_${arch}.deb"; \
    dpkg -i /tmp/fdb-clients.deb; \
    rm /tmp/fdb-clients.deb; \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Workspace layout inside the builder image mirrors the host layout
# so the `../../../manta-storage/...` and `../../../proteus/...`
# path-deps in services/tritond/Cargo.toml resolve. The build
# context is the parent of monitor-reef; .dockerignore at the
# context root keeps target/, cache/, .git/, and docs out.
WORKDIR /build/monitor-reef
COPY monitor-reef/rust-toolchain.toml monitor-reef/Cargo.toml monitor-reef/Cargo.lock ./
COPY monitor-reef/apis ./apis
COPY monitor-reef/services ./services
COPY monitor-reef/clients ./clients
COPY monitor-reef/libs ./libs
COPY monitor-reef/cli ./cli
COPY monitor-reef/client-generator ./client-generator
COPY monitor-reef/openapi-manager ./openapi-manager
COPY monitor-reef/openapi-specs ./openapi-specs

# Sibling workspaces with path-deps from services/tritond/Cargo.toml:
#   mantad-client = { path = "../../../manta-storage/crates/mantad-client" }
#   proteus-api   = { path = "../../../proteus/crates/proteus-api", ... }
#   triton-vpc    = { path = "../../../proteus/plugins/triton-vpc", ... }
# These need to land inside the image at the same relative position
# so the `..`-anchored paths resolve.
COPY manta-storage /build/manta-storage
COPY proteus /build/proteus

# Build tritond with the foundationdb feature; cache cargo state.
# CARGO_TARGET_DIR points cargo at the cache-mounted location so the
# build artifacts land in the buildkit cache (faster rebuilds) rather
# than inside the workspace dir (which would be discarded with the
# layer). The `cp` runs inside the same RUN so the cache mount is
# still live when the binary is copied out.
ENV CARGO_TARGET_DIR=/build/target
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked -p tritond --features foundationdb \
    && cp /build/target/release/tritond /usr/local/bin/tritond

FROM debian:bookworm-slim AS runtime

ARG FDB_VERSION=7.3.27

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    arch=$(dpkg --print-architecture); \
    curl -fsSL -o /tmp/fdb-clients.deb \
      "https://github.com/apple/foundationdb/releases/download/${FDB_VERSION}/foundationdb-clients_${FDB_VERSION}-1_${arch}.deb"; \
    dpkg -i /tmp/fdb-clients.deb; \
    rm /tmp/fdb-clients.deb; \
    apt-get purge -y --auto-remove curl; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd --system --gid 1000 tritond; \
    useradd --system --uid 1000 --gid 1000 \
       --no-create-home --shell /usr/sbin/nologin tritond

COPY --from=builder /usr/local/bin/tritond /usr/local/bin/tritond

USER tritond:tritond

EXPOSE 8080

ENV TRITOND_BIND_ADDRESS=0.0.0.0:8080 \
    RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/tritond"]
