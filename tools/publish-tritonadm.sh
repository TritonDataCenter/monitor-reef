#!/bin/bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

#
# publish-tritonadm.sh - cross-build tritonadm for illumos on the SmartOS build
# host, package it, and publish to Manta via tritoncloud-publish.
#
# Requires (on the local workstation):
#   - $MANTA_USER / $MANTA_KEY_ID / $MANTA_URL set
#   - $MINISIGN_PASSWORD set (or signed interactively at the prompt)
#   - $TRITONCLOUD_SECRET_KEY pointing at the publisher key
#   - target/release/tritoncloud-publish built
#
# Requires (on the build host, defaults below):
#   - cargo + rustc on /opt/tools/bin
#   - CARGO_HOME and CARGO_TARGET_DIR writable
#
# Usage:
#   bash tools/publish-tritonadm.sh [--channel edge|stable]
#
# Channel defaults to edge.
#

set -euo pipefail

CHANNEL=edge
while [ $# -gt 0 ]; do
    case "$1" in
        --channel) CHANNEL="$2"; shift 2 ;;
        -h|--help)
            sed -n '1,/^set -euo/p' "$0" | head -n -1 | sed 's/^# //; s/^#//'
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

BUILD_HOST=${BUILD_HOST:-root@10.199.199.10}
BUILD_KEY=${BUILD_KEY:-$HOME/.ssh/sdc.id_rsa}
BUILD_DIR=${BUILD_DIR:-/opt/tritonadm-build}
TARGET_TRIPLE=${TARGET_TRIPLE:-x86_64-unknown-illumos}

TOP=$(cd "$(dirname "$0")/.." && pwd)
PUB=$TOP/target/release/tritoncloud-publish
[ -x "$PUB" ] || { echo "tritoncloud-publish missing; cargo build -p tritoncloud-publish --release" >&2; exit 1; }

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
TARBALL=$TOP/target/release/tritonadm-$STAMP-$TARGET_TRIPLE.tar.gz

echo "==> stamp: $STAMP"
echo "==> target: $TARGET_TRIPLE"
echo "==> channel: $CHANNEL"

echo "==> rsync workspace -> $BUILD_HOST:$BUILD_DIR"
rsync -az \
    --exclude='target/' \
    --exclude='.git/' \
    --exclude='.claude/' \
    --exclude='.beads/' \
    --exclude='.DS_Store' \
    --exclude='cache/' \
    -e "ssh -i $BUILD_KEY" \
    "$TOP/" "$BUILD_HOST:$BUILD_DIR/"

echo "==> cross-build on $BUILD_HOST"
ssh -i "$BUILD_KEY" "$BUILD_HOST" \
    "cd $BUILD_DIR && \
     export PATH=/opt/tools/bin:\$PATH \
            CARGO_TARGET_DIR=$BUILD_DIR/target \
            CARGO_HOME=/opt/cargo-home \
            LIBRARY_PATH=/opt/fdb/lib \
            TRITONADM_BUILD_STAMP=$STAMP && \
     cargo build -p tritonadm --release -j 2"

echo "==> fetch binary back"
scp -i "$BUILD_KEY" "$BUILD_HOST:$BUILD_DIR/target/release/tritonadm" \
    "$TOP/target/release/tritonadm-illumos-$STAMP"

echo "==> package"
STAGE=$(mktemp -d)
cp "$TOP/target/release/tritonadm-illumos-$STAMP" "$STAGE/tritonadm"
chmod 0755 "$STAGE/tritonadm"
tar -C "$STAGE" -czf "$TARBALL" tritonadm
rm -rf "$STAGE"
ls -la "$TARBALL"

echo "==> publish to Manta channel '$CHANNEL'"
"$PUB" --channel "$CHANNEL" tritonadm \
    --stamp "$STAMP" \
    --target "$TARGET_TRIPLE" \
    --tarball "$TARBALL"

echo "==> done. install on a fresh CN with:"
echo "    curl -fsSL https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/install.sh | sh"
