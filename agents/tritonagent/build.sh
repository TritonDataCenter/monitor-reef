#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# build.sh - assemble the tritonagent GZ agent tarball.
#
# Fetches the binary from Manta on demand, lays out the per-agent
# proto/, writes etc/version with the stamp, and tars the result.
# Output is suitable for `tritonadm agent install tritonagent` (extract at /).
#

set -euo pipefail

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
OUTPUT_DIR=${OUTPUT_DIR:-/tmp}
PROTO_DIR=${PROTO_DIR:-$(cd "$(dirname "$0")" && pwd)/proto}
BIN_URL=${BIN_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/tritonagent-illumos.bin}

NAME=tritonagent
TARBALL="$OUTPUT_DIR/${NAME}-${STAMP}.tar.gz"

[ -d "$PROTO_DIR" ] || { echo "ERROR: $PROTO_DIR missing" >&2; exit 2; }

# Fetch the binary on demand (gitignored; too large for GitHub).
BIN_DEST="$PROTO_DIR/opt/triton/tritonagent/bin/tritonagent"
if [ ! -x "$BIN_DEST" ]; then
    echo "==> fetching tritonagent binary from $BIN_URL"
    mkdir -p "$(dirname "$BIN_DEST")"
    curl -fsSL "$BIN_URL" -o "$BIN_DEST"
    chmod 0755 "$BIN_DEST"
fi

# Stamp the version file so `tritonadm agent list` can show what is
# installed without re-reading the binary.
printf '%s\n' "$STAMP" > "$PROTO_DIR/opt/triton/tritonagent/etc/version"
chmod 0644 "$PROTO_DIR/opt/triton/tritonagent/etc/version"

# Set perms before tarring.
chmod 0755 "$PROTO_DIR/var/svc/method/tritonagent"
chmod 0644 "$PROTO_DIR/opt/triton/tritonagent/smf/tritonagent.xml"
chmod 0644 "$PROTO_DIR/opt/triton/tritonagent/etc/agent.env.example"

# Tar the proto. We use gtar on a SmartOS build host (BSD tar on
# macOS works too via stat fallback in the publisher's checks).
TAR=$(command -v gtar 2>/dev/null || echo tar)
"$TAR" -C "$PROTO_DIR" -czf "$TARBALL" .
SHA256=$(shasum -a 256 "$TARBALL" 2>/dev/null | awk '{print $1}')
[ -z "$SHA256" ] && SHA256=$(digest -a sha256 "$TARBALL" 2>/dev/null)
SIZE=$(stat -f %z "$TARBALL" 2>/dev/null || stat -c %s "$TARBALL")

echo "==> wrote $TARBALL"
echo "==> sha256:  $SHA256"
echo "==> size:    $SIZE bytes"
echo ""
echo "next: publish via tritoncloud-publish agent \\"
echo "          --channel edge \\"
echo "          --name $NAME \\"
echo "          --stamp $STAMP \\"
echo "          --tarball $TARBALL"
