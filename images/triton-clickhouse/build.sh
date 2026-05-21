#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# build.sh - build the triton-clickhouse zone image bundle.
#
# Same shape as triton-fdb / triton-tritond. Fetches the clickhouse
# multi-call binary tarball from Manta on demand and assembles a
# zone-dataset image.

set -euo pipefail

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
OUTPUT_DIR=${OUTPUT_DIR:-/var/tmp}
PROTO_DIR=${PROTO_DIR:-$(cd "$(dirname "$0")" && pwd)/proto}
BASE_IMAGE_UUID=${BASE_IMAGE_UUID:-502eeef2-8267-489f-b19c-a206906f57ef}
ZONES_POOL=${ZONES_POOL:-zones}

CH_BITS_URL=${CH_BITS_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/clickhouse-26.3.10-illumos.tar.gz}

IMAGE_NAME=triton-clickhouse
IMAGE_DESC="ClickHouse 26.3 zone (triton-clickhouse)"
IMAGE_VERSION=1.0.0
DATA_FORMAT_VERSION=1

IMAGE_UUID=$(uuid -v4)
WORK_DS="${ZONES_POOL}/triton-clickhouse-build-${STAMP}"
CONTENT_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.zfs.gz"
MANIFEST_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.json"

[ -d "$PROTO_DIR" ] || { echo "ERROR: proto dir $PROTO_DIR missing" >&2; exit 2; }

# SmartOS curl needs an explicit trust store (see triton-fdb/build.sh
# for the same hack).
if [ -z "${CURL_CA_BUNDLE:-}" ] \
        && [ -f /opt/tools/share/mozilla-rootcerts/cacert.pem ]; then
    export CURL_CA_BUNDLE=/opt/tools/share/mozilla-rootcerts/cacert.pem
fi

# Fetch the clickhouse binary tarball on demand. Too large for git
# (667 MB multi-call binary; ~160 MB gzipped). The tarball contains
# clickhouse/bin/clickhouse; we extract + create the standard tool
# symlinks ourselves.
if [ ! -x "$PROTO_DIR/opt/triton/clickhouse/bin/clickhouse" ]; then
    echo "==> fetching clickhouse from $CH_BITS_URL"
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT
    curl -fsSL "$CH_BITS_URL" -o "$TMP/ch.tar.gz"
    ( cd "$TMP" && gtar -xzf ch.tar.gz )
    mkdir -p "$PROTO_DIR/opt/triton/clickhouse/bin"
    cp "$TMP/clickhouse/bin/clickhouse" "$PROTO_DIR/opt/triton/clickhouse/bin/clickhouse"
    chmod 0755 "$PROTO_DIR/opt/triton/clickhouse/bin/clickhouse"
    # Standard multi-call symlinks. clickhouse-server is the only
    # one we actually need; the rest are for operator convenience
    # (clickhouse-client inside the zone for ad-hoc queries).
    ( cd "$PROTO_DIR/opt/triton/clickhouse/bin" && \
        for tool in server client local benchmark format keeper compressor extract-from-config; do
            ln -sf clickhouse "clickhouse-${tool}"
        done
    )
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
[ -d "$WORK_ROOT" ] || { echo "ERROR: $WORK_ROOT missing" >&2; exit 2; }

cleanup() {
    echo "==> cleaning up $WORK_DS"
    zfs destroy -r "$WORK_DS" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> rsync proto -> $WORK_ROOT"
rsync -aH "$PROTO_DIR/" "$WORK_ROOT/"

chown -R root:root "$WORK_ROOT/opt" "$WORK_ROOT/var/svc/method"
chmod 0755 "$WORK_ROOT/var/svc/method/triton-clickhouse"
chmod 0755 "$WORK_ROOT/opt/triton/clickhouse/bin/clickhouse"
chmod 0644 "$WORK_ROOT/opt/triton/clickhouse/smf/triton-clickhouse.xml"
chmod 0644 "$WORK_ROOT/opt/triton/clickhouse/etc/config.xml.tmpl"
chmod 0644 "$WORK_ROOT/opt/triton/clickhouse/etc/users.xml"

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
