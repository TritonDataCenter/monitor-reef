#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# build.sh - build the triton-fdb zone image bundle (imgadm format).
#
# Runs on a SmartOS host. Produces in OUTPUT_DIR:
#   triton-fdb-$STAMP.json      (imgadm manifest)
#   triton-fdb-$STAMP.zfs.gz    (image content, gzipped zfs send stream)
#
# Strategy:
#   1. imgadm import the BASE_IMAGE_UUID (minimal-64-lts) if not
#      already present locally.
#   2. zfs clone the base image's @final snapshot to a temp dataset.
#   3. rsync the per-image proto/ tree over the clone's mountpoint,
#      preserving mode + ownership (root:root for everything we own).
#   4. Snapshot the modified clone as @final.
#   5. zfs send | gzip the @final snapshot to OUTPUT_DIR.
#   6. Write the imgadm manifest with the right uuid, sha1, size,
#      origin reference.
#   7. Destroy the temp clone + snapshot. The base image stays.
#
# Environment:
#   STAMP             build stamp, default $(date -u +%Y%m%dT%H%M%SZ)
#   OUTPUT_DIR        where to write the bundle; default /var/tmp
#   PROTO_DIR         per-image proto tree; default $(dirname $0)/proto
#   BASE_IMAGE_UUID   minimal-64-lts; defaults below
#   ZONES_POOL        defaults to `zones`
#
# Usage:
#   STAMP=20260521T170000Z OUTPUT_DIR=/var/tmp bash build.sh
#

set -euo pipefail

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
OUTPUT_DIR=${OUTPUT_DIR:-/var/tmp}
PROTO_DIR=${PROTO_DIR:-$(cd "$(dirname "$0")" && pwd)/proto}
BASE_IMAGE_UUID=${BASE_IMAGE_UUID:-502eeef2-8267-489f-b19c-a206906f57ef}
ZONES_POOL=${ZONES_POOL:-zones}

# FDB binaries are too large to track in git (fdbserver is 110 MB,
# over GitHub's 100 MB limit). Fetched here on demand if proto/opt/fdb
# is not already populated. The default URL is a Manta-hosted copy of
# the binaries snapshotted from the working fdb zone on .10.
FDB_BITS_URL=${FDB_BITS_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/fdb-bits-7.3-illumos.tar.gz}

IMAGE_NAME=triton-fdb
IMAGE_DESC="FoundationDB single-process zone (triton-fdb)"
IMAGE_VERSION=1.0.0
DATA_FORMAT_VERSION=730

IMAGE_UUID=$(uuid -v4)
WORK_DS="${ZONES_POOL}/triton-fdb-build-${STAMP}"
CONTENT_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.zfs.gz"
MANIFEST_FILE="${OUTPUT_DIR}/${IMAGE_NAME}-${STAMP}.json"

# Sanity checks
[ -d "$PROTO_DIR" ] || { echo "ERROR: proto dir $PROTO_DIR missing" >&2; exit 2; }

# Populate proto/opt/fdb on demand from the Manta-hosted tarball.
if [ ! -d "$PROTO_DIR/opt/fdb/bin" ]; then
    echo "==> fetching FDB binaries from $FDB_BITS_URL"
    TMP_TARBALL=$(mktemp -t fdb-bits.XXXXXX)
    trap 'rm -f "$TMP_TARBALL"' EXIT
    curl -fsSL "$FDB_BITS_URL" -o "$TMP_TARBALL"
    mkdir -p "$PROTO_DIR/opt"
    ( cd "$PROTO_DIR/opt" && gtar -xzf "$TMP_TARBALL" )
    rm -f "$TMP_TARBALL"
    trap - EXIT
    [ -x "$PROTO_DIR/opt/fdb/bin/fdbserver" ] \
        || { echo "ERROR: tarball did not contain expected fdb/bin/fdbserver" >&2; exit 2; }
fi

echo "==> stamp:       $STAMP"
echo "==> image uuid:  $IMAGE_UUID"
echo "==> base image:  $BASE_IMAGE_UUID"
echo "==> proto:       $PROTO_DIR"
echo "==> work ds:     $WORK_DS"
echo "==> output:      $CONTENT_FILE + $MANIFEST_FILE"

# 1. Ensure base image is present locally.
if ! imgadm get "$BASE_IMAGE_UUID" >/dev/null 2>&1; then
    echo "==> importing base image $BASE_IMAGE_UUID"
    imgadm import "$BASE_IMAGE_UUID"
fi

# 2. Clone the base image's @final snapshot.
BASE_SNAP="${ZONES_POOL}/${BASE_IMAGE_UUID}@final"
[ -e "/dev/zvol/dsk/${BASE_IMAGE_UUID}" ] || \
zfs list -t snapshot "$BASE_SNAP" >/dev/null 2>&1 \
    || { echo "ERROR: $BASE_SNAP not found" >&2; exit 2; }

echo "==> cloning $BASE_SNAP -> $WORK_DS"
zfs clone "$BASE_SNAP" "$WORK_DS"

# Make sure the clone has a sane mountpoint that's writable.
# SmartOS zone-image datasets put the actual zone filesystem under
# a `root/` subdir of the dataset (the dataset top holds zone-private
# scratch like ccs/, cores/, etc.). proto/ must rsync into that
# `root/` subdir, not the dataset top, or the changes will be
# invisible to any zone provisioned from the resulting image.
WORK_MNT="/$WORK_DS"
WORK_ROOT="$WORK_MNT/root"
zfs set mountpoint="$WORK_MNT" "$WORK_DS"
zfs mount "$WORK_DS" 2>/dev/null || true   # may already be mounted
[ -d "$WORK_ROOT" ] || { echo "ERROR: $WORK_ROOT missing (base image layout unexpected)" >&2; exit 2; }

cleanup() {
    echo "==> cleaning up $WORK_DS"
    zfs destroy -r "$WORK_DS" 2>/dev/null || true
}
trap cleanup EXIT

# 3. rsync proto/ over the zone-root subdir of the clone.
echo "==> rsync proto -> $WORK_ROOT"
rsync -aH "$PROTO_DIR/" "$WORK_ROOT/"

# Set ownership + perms explicitly — SmartOS rsync does not honor
# --chown / --chmod across all versions, and we want predictable
# bits regardless.
chown -R root:root "$WORK_ROOT/opt" "$WORK_ROOT/var/svc/method"
chmod 0755 "$WORK_ROOT/var/svc/method/triton-fdb"
chmod 0755 "$WORK_ROOT/opt/fdb/bin/fdbserver"
chmod 0755 "$WORK_ROOT/opt/fdb/bin/fdbcli"
chmod 0644 "$WORK_ROOT/opt/triton/fdb/smf/triton-fdb.xml"

# 4. Snapshot the modified clone.
echo "==> snapshotting $WORK_DS@final"
zfs snapshot "$WORK_DS@final"

# 5. zfs send | gzip the snapshot, computing sha1 in the same pipe.
echo "==> sending + gzipping snapshot"
mkdir -p "$OUTPUT_DIR"
zfs send "$WORK_DS@final" | gzip -c > "$CONTENT_FILE"
SHA1=$(digest -a sha1 "$CONTENT_FILE")
SIZE=$(stat -c %s "$CONTENT_FILE" 2>/dev/null || stat -f %z "$CONTENT_FILE")
echo "==> content sha1:  $SHA1"
echo "==> content size:  $SIZE bytes"

# 6. Write the imgadm manifest.
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
