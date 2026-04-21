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
# Bootstrap installer for tritonadm. The same tarball also ships the
# user-facing `triton` CLI (cli/triton-cli), so a successful install
# places both binaries in $INSTALL_DIR/bin and symlinks both into the
# same directory on PATH. Designed to run on a Triton headnode global
# zone. See docs/design/tritonadm-distribution.md for the full design.
#
# Three install modes:
#
#   * Network (default): resolve latest in --channel against the updates
#     server, download the shar, verify its SHA-1 against the IMGAPI
#     manifest, chmod +x, and exec it. The shar extracts itself to a
#     tempdir and re-invokes THIS script in embedded mode.
#
#   * Local shar: --shar <path>, chmod +x and exec the given shar.
#
#   * Embedded: when this script is sitting next to ./root/opt/triton/
#     tritonadm/ (i.e. it's running from inside an extracted shar), it
#     installs from the adjacent payload. This is the mode sdcadm
#     experimental get-tritonadm ends up in, and the mode the shar itself
#     ends up in after extracting its payload.
#
# The final etc/version file always has this shape, regardless of mode:
#
#   uuid=<image-uuid>            # baked into tarball at build time
#   version=<build-stamp>        # eng-style: <branch>-<UTC>-g<sha>
#   installed_at=<iso8601>       # set by this script
#   source=embedded|network|local
#
# Usage:
#   install-tritonadm.sh [--channel <name>] [--updates-url <url>]
#   install-tritonadm.sh --uuid <image-uuid> [--updates-url <url>]
#   install-tritonadm.sh --shar <path>        # local .sh, no network
#

set -o errexit
set -o pipefail
set -o nounset

# sdcadm sets TRACE=1 on its exec() of the installer; honor it so
# `/var/sdcadm/tritonadm-installs/*/install.log` is debuggable.
if [[ "${TRACE:-}" == "1" ]]; then
    export PS4='[\D{%FT%TZ}] ${BASH_SOURCE}:${LINENO}: ${FUNCNAME[0]:+${FUNCNAME[0]}(): }'
    set -o xtrace
fi

UPDATES_URL="${UPDATES_URL:-https://updates.tritondatacenter.com}"
CHANNEL="experimental"
INSTALL_DIR="/opt/triton/tritonadm"
# Match sdcadm's convention: /opt/smartdc/bin/<tool> -> installed bin.
# On a headnode /opt/smartdc/bin always exists and is on PATH; the old
# default of /opt/local/bin assumed pkgsrc was bootstrapped on the GZ,
# which isn't true on a plain headnode.
SYMLINK="/opt/smartdc/bin/tritonadm"
NO_SYMLINK=false
UUID=""
LOCAL_SHAR=""

usage() {
    cat <<EOF
Usage:
  $(basename "$0") [--channel <name>] [--updates-url <url>]
  $(basename "$0") --uuid <image-uuid> [--updates-url <url>]
  $(basename "$0") --shar <path>

Installs tritonadm (and the bundled `triton` CLI) into $INSTALL_DIR and
symlinks $SYMLINK plus $(dirname "$SYMLINK")/triton.

Options:
  --channel <name>         Updates channel (default: experimental)
  --updates-url <url>      Updates server (default: \$UPDATES_URL or
                             https://updates.tritondatacenter.com)
  --uuid <image-uuid>      Pin to a specific image UUID instead of "latest"
  --shar <path>            Install from a local tritonadm shar (skips network)
  --install-dir <path>     Override install dir (default: $INSTALL_DIR)
  --symlink <path>         Override symlink target (default: $SYMLINK)
  --no-symlink             Skip creating the symlink
  -h, --help               Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --channel)      CHANNEL="$2"; shift 2 ;;
        --updates-url)  UPDATES_URL="$2"; shift 2 ;;
        --uuid)         UUID="$2"; shift 2 ;;
        --shar)         LOCAL_SHAR="$2"; shift 2 ;;
        --install-dir)  INSTALL_DIR="$2"; shift 2 ;;
        --symlink)      SYMLINK="$2"; shift 2 ;;
        --no-symlink)   NO_SYMLINK=true; shift ;;
        -h|--help)      usage; exit 0 ;;
        *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
done

#
# Tool checks are mode-specific. Embedded mode (the common path — what
# sdcadm experimental get-tritonadm ends up in, and what operators hit
# when they extract the shar and run install.sh themselves) only needs
# base-system utilities (awk, mkdir, cp, chmod, ln, cat, date). Network
# and local-shar modes do their own checks right before they need the
# tools.
#

# Pick a sha1 helper that exists on this host. illumos GZ has
# /usr/bin/digest; most other systems have sha1sum or openssl.
sha1_of() {
    if command -v sha1sum >/dev/null 2>&1; then
        sha1sum "$1" | awk '{print $1}'
    elif command -v digest >/dev/null 2>&1; then
        digest -a sha1 "$1"
    else
        openssl dgst -sha1 "$1" | awk '{print $NF}'
    fi
}

# Read a JSON path from a file. Prefers `json` (shipped on Triton
# headnodes at /usr/bin/json), falls back to `jq` for dev hosts.
# Path syntax is the same for both modulo a leading dot (json:
# `files[0].sha1`, jq: `.files[0].sha1`).
json_get() {
    local file=$1 path=$2
    if command -v json >/dev/null 2>&1; then
        json -f "$file" "$path"
    elif command -v jq >/dev/null 2>&1; then
        jq -r ".$path" "$file"
    else
        echo "error: need 'json' (Triton) or 'jq' to parse JSON" >&2
        return 1
    fi
}

# Pick the uuid of the most recently published image from a JSON array.
# Uses published_at (ISO-8601, so lex-sortable). json's -ga emits
# space-separated fields per array element; jq's sort_by/reverse
# produces the same result.
pick_latest_uuid() {
    local file=$1
    if command -v json >/dev/null 2>&1; then
        json -ga uuid published_at -f "$file" \
            | sort -k2 | tail -1 | awk '{print $1}'
    elif command -v jq >/dev/null 2>&1; then
        jq -r 'sort_by(.published_at) | reverse | .[0].uuid // empty' "$file"
    else
        echo "error: need 'json' (Triton) or 'jq' to parse image list" >&2
        return 1
    fi
}

#
# Mode 1 — Embedded. Run from inside an extracted shar. Do the actual
# install from the adjacent payload.
#
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
EMBEDDED_PAYLOAD="$SCRIPT_DIR/root/opt/triton/tritonadm"
if [[ -d "$EMBEDDED_PAYLOAD" ]]; then
    echo "==> Installing from embedded payload at $SCRIPT_DIR"
    # uuid + version are baked into the tarball at build time. Preserve
    # them end-to-end in the installed etc/version.
    BAKED_VERSION="$EMBEDDED_PAYLOAD/etc/version"
    INSTALLED_UUID=$(awk -F= '$1=="uuid"{print $2; exit}' "$BAKED_VERSION" 2>/dev/null || echo "")
    INSTALLED_VERSION=$(awk -F= '$1=="version"{print $2; exit}' "$BAKED_VERSION" 2>/dev/null || echo "")
    if [[ -z "$INSTALLED_UUID" || -z "$INSTALLED_VERSION" ]]; then
        echo "error: embedded payload missing uuid/version in etc/version" >&2
        exit 1
    fi
    INSTALL_SOURCE="embedded"

    # Atomic-swap staging: build $INSTALL_DIR.new/, rotate the live
    # dir to $INSTALL_DIR.old/, then rename the staging dir into
    # place. A failure BEFORE the rotation leaves the live install
    # untouched; AFTER the rotation, .old holds the previous version
    # for manual rollback. Matches sdcadm's install-sdcadm.sh
    # DESTDIR/.new/.old convention.
    NEW_DIR="${INSTALL_DIR}.new"
    OLD_DIR="${INSTALL_DIR}.old"
    rm -rf "$NEW_DIR"
    mkdir -p "$NEW_DIR"
    # cp -R preserves mode bits without depending on rsync (not on
    # every GZ). Trailing /. copies contents, not the directory itself.
    cp -R "$EMBEDDED_PAYLOAD/." "$NEW_DIR/"

    # Write the unified etc/version INTO the staging dir so the whole
    # swap is one rename.
    mkdir -p "$NEW_DIR/etc"
    cat > "$NEW_DIR/etc/version" <<EOF
uuid=$INSTALLED_UUID
version=$INSTALLED_VERSION
installed_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
source=$INSTALL_SOURCE
EOF

    # Rotate. rm any stale .old (previous rollback target), then:
    #   live → .old (if live existed)
    #   new  → live
    # There's a ~millisecond window between the two mv calls where
    # $INSTALL_DIR doesn't exist; the alternative (using rsync
    # in-place) loses atomicity entirely.
    rm -rf "$OLD_DIR"
    if [[ -d "$INSTALL_DIR" ]]; then
        mv "$INSTALL_DIR" "$OLD_DIR"
    fi
    mv "$NEW_DIR" "$INSTALL_DIR"

    # Symlink so 'tritonadm' is on PATH. Default /opt/smartdc/bin matches
    # where sdcadm's symlink lives on a headnode. mkdir -p so the create
    # works even if the parent dir is missing (common on dev hosts); if
    # that still fails, we skip with an actionable message rather than
    # trying to second-guess the operator's layout.
    #
    # The same tarball also carries `triton` (the user-facing CLI from
    # cli/triton-cli). Drop a sibling symlink next to tritonadm's so both
    # commands are on PATH. `--symlink` controls the tritonadm path; the
    # triton symlink lives in the same directory and can't be renamed
    # independently (operators who want a custom location can symlink by
    # hand — this is a convenience, not policy).
    SYMLINK_OK=false
    TRITON_SYMLINK_OK=false
    TRITON_SYMLINK="$(dirname "$SYMLINK")/triton"
    if [[ "$NO_SYMLINK" != "true" ]]; then
        mkdir -p "$(dirname "$SYMLINK")" 2>/dev/null || true
        if [[ -d "$(dirname "$SYMLINK")" ]] \
                && ln -sf "$INSTALL_DIR/bin/tritonadm" "$SYMLINK" 2>/dev/null; then
            SYMLINK_OK=true
        else
            echo "==> Skipping symlink: couldn't create $SYMLINK"
            echo "    Override with --symlink <path> or use --no-symlink to silence."
        fi
        if [[ "$SYMLINK_OK" == "true" ]] \
                && ln -sf "$INSTALL_DIR/bin/triton" "$TRITON_SYMLINK" 2>/dev/null; then
            TRITON_SYMLINK_OK=true
        fi
    fi

    echo
    echo "==> Installed: $INSTALLED_VERSION ($INSTALLED_UUID)"
    echo "    Binaries: $INSTALL_DIR/bin/tritonadm"
    echo "              $INSTALL_DIR/bin/triton"
    if [[ "$SYMLINK_OK" == "true" ]]; then
        echo "    Symlink:  $SYMLINK"
        if [[ "$TRITON_SYMLINK_OK" == "true" ]]; then
            echo "              $TRITON_SYMLINK"
        fi
        echo
        "$SYMLINK" --version || true
    else
        echo "    Run with: $INSTALL_DIR/bin/tritonadm"
        echo "              $INSTALL_DIR/bin/triton"
        echo
        "$INSTALL_DIR/bin/tritonadm" --version || true
    fi
    exit 0
fi

#
# Modes 2 and 3 collapse onto a single path: get a shar file, chmod +x,
# exec it with the install-configuration args forwarded. The shar
# extracts itself and re-invokes THIS script in embedded mode above.
#
FORWARDED_ARGS=()
[[ "$INSTALL_DIR" != "/opt/triton/tritonadm"    ]] && FORWARDED_ARGS+=(--install-dir "$INSTALL_DIR")
[[ "$SYMLINK"     != "/opt/smartdc/bin/tritonadm" ]] && FORWARDED_ARGS+=(--symlink "$SYMLINK")
[[ "$NO_SYMLINK"  == "true"                     ]] && FORWARDED_ARGS+=(--no-symlink)

if [[ -n "$LOCAL_SHAR" ]]; then
    # Mode 2 — local shar. Trust the file the operator handed us; skip SHA
    # verification (they're holding the bits).
    if [[ ! -f "$LOCAL_SHAR" ]]; then
        echo "error: --shar path not found: $LOCAL_SHAR" >&2
        exit 1
    fi
    echo "==> Installing from local shar: $LOCAL_SHAR"
    chmod +x "$LOCAL_SHAR"
    exec "$LOCAL_SHAR" "${FORWARDED_ARGS[@]}"
fi

# Mode 3 — network. Resolve latest/uuid on the updates server, download,
# verify SHA against the manifest, exec.
command -v curl >/dev/null 2>&1 \
    || { echo "error: network mode needs curl" >&2; exit 1; }
command -v json >/dev/null 2>&1 || command -v jq >/dev/null 2>&1 \
    || { echo "error: network mode needs 'json' (Triton) or 'jq'" >&2; exit 1; }

WORKDIR=$(mktemp -d -t tritonadm-install.XXXXXX)
trap 'rm -rf "$WORKDIR"' EXIT

echo "==> Resolving tritonadm image (channel=$CHANNEL)"
if [[ -z "$UUID" ]]; then
    list_url="$UPDATES_URL/images?name=tritonadm&channel=$CHANNEL&state=active"
    curl -sSf -o "$WORKDIR/images.json" "$list_url"
    UUID=$(pick_latest_uuid "$WORKDIR/images.json")
    if [[ -z "$UUID" ]]; then
        echo "error: no active tritonadm images on channel=$CHANNEL at $UPDATES_URL" >&2
        exit 1
    fi
    echo "    Latest: $UUID"
else
    echo "    Pinned: $UUID"
fi

manifest_url="$UPDATES_URL/images/$UUID?channel=$CHANNEL"
file_url="$UPDATES_URL/images/$UUID/file?channel=$CHANNEL"

echo "==> Fetching manifest"
curl -sSf -o "$WORKDIR/tritonadm.imgmanifest" "$manifest_url"

echo "==> Fetching shar"
curl -sSf -o "$WORKDIR/tritonadm.sh" "$file_url"

EXPECTED=$(json_get "$WORKDIR/tritonadm.imgmanifest" 'files[0].sha1')
if [[ -z "$EXPECTED" ]]; then
    echo "error: manifest missing files[0].sha1" >&2
    exit 1
fi
ACTUAL=$(sha1_of "$WORKDIR/tritonadm.sh")
if [[ "$EXPECTED" != "$ACTUAL" ]]; then
    echo "error: SHA-1 mismatch (expected $EXPECTED, got $ACTUAL)" >&2
    exit 1
fi
echo "==> SHA-1 verified ($ACTUAL)"

chmod +x "$WORKDIR/tritonadm.sh"
exec "$WORKDIR/tritonadm.sh" "${FORWARDED_ARGS[@]}"
