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
# Build:    docker build -t tritond:dev .
# Run:      docker run --rm -p 8080:8080 tritond:dev
# Compose:  docker compose up

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

# Copy only what cargo needs to resolve and build the workspace. The
# .dockerignore keeps target/, cache/, deps/, .git/, and docs out of
# the build context entirely.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY apis ./apis
COPY services ./services
COPY clients ./clients
COPY libs ./libs
COPY cli ./cli
COPY client-generator ./client-generator
COPY openapi-manager ./openapi-manager
COPY openapi-specs ./openapi-specs

# Build tritond with the foundationdb feature; cache cargo state.
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
