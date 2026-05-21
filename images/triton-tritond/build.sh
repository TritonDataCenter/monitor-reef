#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# build.sh - build the triton-tritond zone image bundle.
#
# Mirrors images/triton-fdb/build.sh. Differs only in:
#   - the proto layout (tritond binary + libfdb_c.so)
#   - the on-demand fetches: tritond binary from
#     TRITON_TRITOND_BIN_URL (Manta-hosted, snapshot from .10 build),
#     libfdb_c.so from the same fdb-bits tarball used by triton-fdb.
#
# Environment:
#   STAMP                 build stamp, default $(date -u +%Y%m%dT%H%M%SZ)
#   OUTPUT_DIR            where to write the bundle; default /var/tmp
#   PROTO_DIR             per-image proto tree; default $(dirname $0)/proto
#   BASE_IMAGE_UUID       minimal-64-lts, default below
#   ZONES_POOL            defaults to `zones`
#   TRITOND_BIN_URL       Manta URL for the prebuilt tritond binary
#   FDB_BITS_URL          shared with triton-fdb; tarball contains
#                         lib/libfdb_c.so which we extract on demand
#

set -euo pipefail

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
OUTPUT_DIR=${OUTPUT_DIR:-/var/tmp}
PROTO_DIR=${PROTO_DIR:-$(cd "$(dirname "$0")" && pwd)/proto}
BASE_IMAGE_UUID=${BASE_IMAGE_UUID:-502eeef2-8267-489f-b19c-a206906f57ef}
ZONES_POOL=${ZONES_POOL:-zones}

TRITOND_BIN_URL=${TRITOND_BIN_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/tritond-illumos.bin}
FDB_BITS_URL=${FDB_BITS_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/fdb-bits-7.3-illumos.tar.gz}

IMAGE_NAME=triton-tritond
IMAGE_DESC="Triton Cloud control-plane daemon (triton-tritond)"
IMAGE_VERSION=1.0.0
DATA_FORMAT_VERSION=1

IMAGE_UUID=$(uuid -v4)
WORK_DS="${ZONES_POOL}/triton-tritond-build-${STAMP}"
CONTENT_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.zfs.gz"
MANIFEST_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.json"

[ -d "$PROTO_DIR" ] || { echo "ERROR: proto dir $PROTO_DIR missing" >&2; exit 2; }

# SmartOS curl built with a non-default trust-store path that does
# not exist (looks for Linux's /etc/ssl/certs/ca-certificates.crt).
# Point it at the pkgsrc Mozilla bundle when the env doesn't set it.
if [ -z "${CURL_CA_BUNDLE:-}" ] \
        && [ -f /opt/tools/share/mozilla-rootcerts/cacert.pem ]; then
    export CURL_CA_BUNDLE=/opt/tools/share/mozilla-rootcerts/cacert.pem
fi

# Fetch the tritond binary on demand. Too large for git.
if [ ! -x "$PROTO_DIR/opt/triton/tritond/bin/tritond" ]; then
    echo "==> fetching tritond binary from $TRITOND_BIN_URL"
    mkdir -p "$PROTO_DIR/opt/triton/tritond/bin"
    curl -fsSL "$TRITOND_BIN_URL" -o "$PROTO_DIR/opt/triton/tritond/bin/tritond"
    chmod 0755 "$PROTO_DIR/opt/triton/tritond/bin/tritond"
fi

# Fetch the full fdb/lib/ set from the shared FDB tarball. libfdb_c.so
# is the only direct dependency tritond knows about but it
# transitively requires libfmt, libstdc++, libexecinfo; ship them
# all so LD_LIBRARY_PATH=/opt/triton/tritond/lib resolves cleanly
# without needing /opt/fdb to also be installed.
if [ ! -f "$PROTO_DIR/opt/triton/tritond/lib/libfdb_c.so" ]; then
    echo "==> fetching fdb/lib/* from $FDB_BITS_URL"
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT
    curl -fsSL "$FDB_BITS_URL" -o "$TMP/fdb-bits.tar.gz"
    ( cd "$TMP" && gtar -xzf fdb-bits.tar.gz fdb/lib )
    mkdir -p "$PROTO_DIR/opt/triton/tritond/lib"
    cp "$TMP"/fdb/lib/*.so* "$PROTO_DIR/opt/triton/tritond/lib/"
    chmod 0755 "$PROTO_DIR/opt/triton/tritond/lib"/*.so*
    rm -rf "$TMP"
    trap - EXIT
fi

echo "==> stamp:       $STAMP"
echo "==> image uuid:  $IMAGE_UUID"
echo "==> base image:  $BASE_IMAGE_UUID"
echo "==> proto:       $PROTO_DIR"
echo "==> work ds:     $WORK_DS"
echo "==> output:      $CONTENT_FILE + $MANIFEST_FILE"

if ! imgadm get "$BASE_IMAGE_UUID" >/dev/null 2>&1; then
    echo "==> importing base image $BASE_IMAGE_UUID"
    imgadm import "$BASE_IMAGE_UUID"
fi

BASE_SNAP="${ZONES_POOL}/${BASE_IMAGE_UUID}@final"
zfs list -t snapshot "$BASE_SNAP" >/dev/null 2>&1 \
    || { echo "ERROR: $BASE_SNAP not found" >&2; exit 2; }

echo "==> cloning $BASE_SNAP -> $WORK_DS"
zfs clone "$BASE_SNAP" "$WORK_DS"

WORK_MNT="/$WORK_DS"
WORK_ROOT="$WORK_MNT/root"
zfs set mountpoint="$WORK_MNT" "$WORK_DS"
zfs mount "$WORK_DS" 2>/dev/null || true
[ -d "$WORK_ROOT" ] || { echo "ERROR: $WORK_ROOT missing (base image layout unexpected)" >&2; exit 2; }

cleanup() {
    echo "==> cleaning up $WORK_DS"
    zfs destroy -r "$WORK_DS" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> rsync proto -> $WORK_ROOT"
rsync -aH "$PROTO_DIR/" "$WORK_ROOT/"

chown -R root:root "$WORK_ROOT/opt" "$WORK_ROOT/var/svc/method"
chmod 0755 "$WORK_ROOT/var/svc/method/triton-tritond"
chmod 0755 "$WORK_ROOT/opt/triton/tritond/bin/tritond"
chmod 0755 "$WORK_ROOT/opt/triton/tritond/lib"/*.so*
chmod 0644 "$WORK_ROOT/opt/triton/tritond/smf/triton-tritond.xml"
chmod 0644 "$WORK_ROOT/opt/triton/tritond/etc/config.toml.tmpl"

echo "==> snapshotting $WORK_DS@final"
zfs snapshot "$WORK_DS@final"

echo "==> sending + gzipping snapshot"
mkdir -p "$OUTPUT_DIR"
zfs send "$WORK_DS@final" | gzip -c > "$CONTENT_FILE"
SHA1=$(digest -a sha1 "$CONTENT_FILE")
SIZE=$(stat -c %s "$CONTENT_FILE" 2>/dev/null || stat -f %z "$CONTENT_FILE")
echo "==> content sha1:  $SHA1"
echo "==> content size:  $SIZE bytes"

PUBLISHED_AT=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
cat > "$MANIFEST_FILE" <<EOF
{
    "v": 2,
    "uuid": "$IMAGE_UUID",
    "owner": "00000000-0000-0000-0000-000000000000",
    "name": "$IMAGE_NAME",
    "version": "$IMAGE_VERSION-$STAMP",
    "state": "active",
    "disabled": false,
    "public": true,
    "type": "zone-dataset",
    "os": "smartos",
    "description": "$IMAGE_DESC",
    "published_at": "$PUBLISHED_AT",
    "origin": "$BASE_IMAGE_UUID",
    "files": [
        {
            "sha1": "$SHA1",
            "size": $SIZE,
            "compression": "gzip"
        }
    ],
    "tags": {
        "smartdc_service": "true",
        "buildstamp": "$STAMP",
        "triton_image_kind": "system",
        "data_format_version": $DATA_FORMAT_VERSION
    },
    "requirements": {
        "min_platform": {
            "7.0": "20180816T210405Z"
        }
    }
}
EOF

echo "==> wrote $MANIFEST_FILE"
echo "==> wrote $CONTENT_FILE"
echo ""
echo "next: publish via tritoncloud-publish image \\"
echo "          --channel edge \\"
echo "          --name $IMAGE_NAME \\"
echo "          --stamp $STAMP \\"
echo "          --uuid $IMAGE_UUID \\"
echo "          --manifest $MANIFEST_FILE \\"
echo "          --content $CONTENT_FILE \\"
echo "          --data-format-version $DATA_FORMAT_VERSION \\"
echo "          --data-format-min-read $DATA_FORMAT_VERSION"
