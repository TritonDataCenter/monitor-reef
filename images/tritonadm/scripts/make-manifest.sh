#!/usr/bin/env bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# Generate an IMGAPI v2 manifest (type "other") for a tritonadm tarball.
# Emitted on stdout. Matches the convention sdcadm uses for its own releases.
#
# The UUID is passed in (not generated here) because the build pipeline
# bakes the same UUID into the tarball's etc/version, so embedded-mode
# installs can preserve image identity round-trip.
#
# Usage:
#   make-manifest.sh --tarball <path> --uuid <uuid> --version <stamp> \
#                    --branch <branch>
#

set -o errexit
set -o pipefail
set -o nounset

TARBALL=""
UUID=""
VERSION=""
BRANCH=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tarball) TARBALL="$2"; shift 2 ;;
        --uuid)    UUID="$2";    shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --branch)  BRANCH="$2";  shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$TARBALL" || -z "$UUID" || -z "$VERSION" || -z "$BRANCH" ]]; then
    echo "Usage: $0 --tarball <path> --uuid <uuid> --version <stamp> --branch <branch>" >&2
    exit 2
fi

if [[ ! -f "$TARBALL" ]]; then
    echo "tarball not found: $TARBALL" >&2
    exit 1
fi

#
# Portable helpers — these tools all behave the same on illumos and macOS
# build hosts. Use sha1sum where available, openssl as a fallback.
#
if command -v sha1sum >/dev/null 2>&1; then
    SHA1=$(sha1sum "$TARBALL" | awk '{print $1}')
elif command -v digest >/dev/null 2>&1; then
    SHA1=$(digest -a sha1 "$TARBALL")
else
    SHA1=$(openssl dgst -sha1 "$TARBALL" | awk '{print $NF}')
fi

if command -v stat >/dev/null 2>&1; then
    if stat -c%s "$TARBALL" >/dev/null 2>&1; then
        SIZE=$(stat -c%s "$TARBALL")
    else
        SIZE=$(stat -f%z "$TARBALL")
    fi
else
    SIZE=$(wc -c < "$TARBALL" | tr -d ' ')
fi

PUBLISHED_AT=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")

cat <<EOF
{
  "v": 2,
  "uuid": "$UUID",
  "owner": "00000000-0000-0000-0000-000000000000",
  "name": "tritonadm",
  "version": "$VERSION",
  "state": "active",
  "disabled": false,
  "public": false,
  "published_at": "$PUBLISHED_AT",
  "type": "other",
  "os": "smartos",
  "files": [
    {
      "sha1": "$SHA1",
      "size": $SIZE,
      "compression": "gzip"
    }
  ],
  "description": "Triton admin CLI (Rust successor to sdcadm)",
  "homepage": "https://github.com/EdgecastCloud/monitor-reef",
  "tags": {
    "smartdc_service": "true",
    "branch": "$BRANCH"
  }
}
EOF
