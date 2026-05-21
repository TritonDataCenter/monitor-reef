#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# build.sh - assemble the proteusadm GZ agent tarball.
#
# proteusadm is a CLI tool, not a daemon. The tarball ships only
# the binary + a version file; no SMF service. Invoked on demand
# by tritonagent (port lifecycle) or by an operator at the prompt.
#

set -euo pipefail

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
OUTPUT_DIR=${OUTPUT_DIR:-/tmp}
PROTO_DIR=${PROTO_DIR:-$(cd "$(dirname "$0")" && pwd)/proto}
BIN_URL=${BIN_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/proteusadm-illumos.bin}

NAME=proteusadm
TARBALL="$OUTPUT_DIR/${NAME}-${STAMP}.tar.gz"

[ -d "$PROTO_DIR" ] || { echo "ERROR: $PROTO_DIR missing" >&2; exit 2; }

BIN_DEST="$PROTO_DIR/opt/triton/proteusadm/bin/proteusadm"
if [ ! -x "$BIN_DEST" ]; then
    echo "==> fetching proteusadm from $BIN_URL"
    mkdir -p "$(dirname "$BIN_DEST")"
    curl -fsSL "$BIN_URL" -o "$BIN_DEST"
    chmod 0755 "$BIN_DEST"
fi

printf '%s\n' "$STAMP" > "$PROTO_DIR/opt/triton/proteusadm/etc/version"
chmod 0644 "$PROTO_DIR/opt/triton/proteusadm/etc/version"

TAR=$(command -v gtar 2>/dev/null || echo tar)
"$TAR" -C "$PROTO_DIR" -czf "$TARBALL" .
SHA256=$(shasum -a 256 "$TARBALL" 2>/dev/null | awk '{print $1}')
[ -z "$SHA256" ] && SHA256=$(digest -a sha256 "$TARBALL" 2>/dev/null)
SIZE=$(stat -f %z "$TARBALL" 2>/dev/null || stat -c %s "$TARBALL")

echo "==> wrote $TARBALL"
echo "==> sha256:  $SHA256"
echo "==> size:    $SIZE bytes"
echo ""
echo "next: tritoncloud-publish --channel edge agent --name $NAME --stamp $STAMP --tarball $TARBALL"
