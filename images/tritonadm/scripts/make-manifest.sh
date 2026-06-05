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
# Generate an IMGAPI v2 manifest (type "other") for a tritonadm build.
# Emitted on stdout. Mirrors sdcadm's own manifest convention
# (sdcadm/tools/mk-shar): `type: other`, `os: other`, `public: false`,
# compression absent (the file is a self-extracting bash script, not a
# compressed blob IMGAPI should wrap).
#
# The UUID is passed in rather than generated because the build pipeline
# bakes the same UUID into the tarball's etc/version, so embedded-mode
# installs can preserve image identity round-trip.
#
# Usage:
#   make-manifest.sh --file <path> --uuid <uuid> --version <stamp> \
#                    --branch <branch>
#
# --file points at whatever gets uploaded to the updates server. Today
# that's tritonadm-<stamp>.sh (the shar).
#

set -o errexit
set -o pipefail
set -o nounset

FILE=""
UUID=""
VERSION=""
BRANCH=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --file)    FILE="$2";    shift 2 ;;
        --uuid)    UUID="$2";    shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --branch)  BRANCH="$2";  shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$FILE" || -z "$UUID" || -z "$VERSION" || -z "$BRANCH" ]]; then
    echo "Usage: $0 --file <path> --uuid <uuid> --version <stamp> --branch <branch>" >&2
    exit 2
fi

if [[ ! -f "$FILE" ]]; then
    echo "file not found: $FILE" >&2
    exit 1
fi

#
# Portable helpers — these tools all behave the same on illumos and macOS
# build hosts. Use sha1sum where available, openssl as a fallback.
#
if command -v sha1sum >/dev/null 2>&1; then
    SHA1=$(sha1sum "$FILE" | awk '{print $1}')
elif command -v digest >/dev/null 2>&1; then
    SHA1=$(digest -a sha1 "$FILE")
else
    SHA1=$(openssl dgst -sha1 "$FILE" | awk '{print $NF}')
fi

if command -v stat >/dev/null 2>&1; then
    if stat -c%s "$FILE" >/dev/null 2>&1; then
        SIZE=$(stat -c%s "$FILE")
    else
        SIZE=$(stat -f%z "$FILE")
    fi
else
    SIZE=$(wc -c < "$FILE" | tr -d ' ')
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
  "os": "other",
  "files": [
    {
      "sha1": "$SHA1",
      "size": $SIZE,
      "compression": "none"
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
