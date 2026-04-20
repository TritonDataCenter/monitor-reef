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
# Wrap a tritonadm tarball into a self-extracting shell archive. The
# resulting .sh is what gets published to updates.tritondatacenter.com as
# the IMGAPI image file — sdcadm experimental get-tritonadm (and the
# network mode of tools/install-tritonadm.sh) downloads it, chmods +x,
# and execs it directly. sdcadm itself ships the same way (see
# sdcadm/tools/mk-shar), so matching this shape keeps the installer
# contract uniform across both tools.
#
# Usage: make-shar.sh --tarball <path> --output <path>
#

set -o errexit
set -o pipefail
set -o nounset

TARBALL=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tarball) TARBALL="$2"; shift 2 ;;
        --output)  OUTPUT="$2";  shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$TARBALL" || -z "$OUTPUT" ]]; then
    echo "Usage: $0 --tarball <path> --output <path>" >&2
    exit 2
fi
if [[ ! -f "$TARBALL" ]]; then
    echo "tarball not found: $TARBALL" >&2
    exit 1
fi

cat > "$OUTPUT" <<'SHAR_HEADER'
#!/usr/bin/env bash
#
# tritonadm installer — self-extracting shell archive. The payload after
# __ARCHIVE_BELOW__ is a gzipped tarball; this header extracts it to a
# tempdir and execs the extracted install.sh. Forwarded args go through
# to install.sh (so e.g. `--install-dir /tmp/foo --no-symlink` works for
# dev testing without editing the shar).
#

set -o errexit
set -o pipefail

if [[ "${TRACE:-}" == "1" ]]; then
    export PS4='[\D{%FT%TZ}] ${BASH_SOURCE}:${LINENO}: '
    set -o xtrace
fi

TMPDIR=$(mktemp -d -t tritonadm-shar.XXXXXX)
trap 'rm -rf "$TMPDIR"' EXIT

ARCHIVE_LINE=$(awk '/^__ARCHIVE_BELOW__$/ { print NR + 1; exit 0; }' "$0")
if [[ -z "$ARCHIVE_LINE" ]]; then
    echo "error: self-extracting installer is corrupt (archive marker missing)" >&2
    exit 1
fi

tail -n +"$ARCHIVE_LINE" "$0" | tar -xzf - -C "$TMPDIR"

exec "$TMPDIR/install.sh" "$@"
# shellcheck disable=SC2317
exit 1
__ARCHIVE_BELOW__
SHAR_HEADER

cat "$TARBALL" >> "$OUTPUT"
chmod 755 "$OUTPUT"

echo "Wrote $OUTPUT"
