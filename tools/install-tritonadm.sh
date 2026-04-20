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
# Bootstrap installer for tritonadm. Designed to run on a Triton headnode
# global zone. See docs/design/tritonadm-distribution.md for the full design.
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
SYMLINK="/opt/local/bin/tritonadm"
NO_SYMLINK=false
UUID=""
LOCAL_SHAR=""

usage() {
    cat <<EOF
Usage:
  $(basename "$0") [--channel <name>] [--updates-url <url>]
  $(basename "$0") --uuid <image-uuid> [--updates-url <url>]
  $(basename "$0") --shar <path>

Installs tritonadm into $INSTALL_DIR and symlinks $SYMLINK.

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
# Required tools. jq is on every modern Triton headnode via pkgsrc.
#
for tool in curl tar jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: required tool '$tool' not found in PATH" >&2
        exit 1
    fi
done

# Pick a sha1 helper that exists on this host.
sha1_of() {
    if command -v sha1sum >/dev/null 2>&1; then
        sha1sum "$1" | awk '{print $1}'
    elif command -v digest >/dev/null 2>&1; then
        digest -a sha1 "$1"
    else
        openssl dgst -sha1 "$1" | awk '{print $NF}'
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
    mkdir -p "$INSTALL_DIR"
    # cp -R preserves mode bits without depending on rsync (not on every
    # GZ). Trailing /. copies contents, not the directory itself.
    cp -R "$EMBEDDED_PAYLOAD/." "$INSTALL_DIR/"

    # Unified etc/version write.
    mkdir -p "$INSTALL_DIR/etc"
    cat > "$INSTALL_DIR/etc/version" <<EOF
uuid=$INSTALLED_UUID
version=$INSTALLED_VERSION
installed_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
source=$INSTALL_SOURCE
EOF

    # Symlink so 'tritonadm' is on PATH. Default /opt/local/bin exists on
    # an illumos GZ; on other hosts we skip with a warning.
    SYMLINK_OK=false
    if [[ "$NO_SYMLINK" == "true" ]]; then
        :
    elif [[ -d "$(dirname "$SYMLINK")" && -w "$(dirname "$SYMLINK")" ]]; then
        ln -sf "$INSTALL_DIR/bin/tritonadm" "$SYMLINK"
        SYMLINK_OK=true
    else
        echo "==> Skipping symlink: $(dirname "$SYMLINK") is missing or unwritable"
        echo "    Override with --symlink <path> or use --no-symlink to silence."
    fi

    echo
    echo "==> Installed: $INSTALLED_VERSION ($INSTALLED_UUID)"
    echo "    Binary:   $INSTALL_DIR/bin/tritonadm"
    if [[ "$SYMLINK_OK" == "true" ]]; then
        echo "    Symlink:  $SYMLINK"
        echo
        "$SYMLINK" --version || true
    else
        echo "    Run with: $INSTALL_DIR/bin/tritonadm"
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
[[ "$INSTALL_DIR" != "/opt/triton/tritonadm"     ]] && FORWARDED_ARGS+=(--install-dir "$INSTALL_DIR")
[[ "$SYMLINK"     != "/opt/local/bin/tritonadm" ]] && FORWARDED_ARGS+=(--symlink "$SYMLINK")
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
WORKDIR=$(mktemp -d -t tritonadm-install.XXXXXX)
trap 'rm -rf "$WORKDIR"' EXIT

echo "==> Resolving tritonadm image (channel=$CHANNEL)"
if [[ -z "$UUID" ]]; then
    list_url="$UPDATES_URL/images?name=tritonadm&channel=$CHANNEL&state=active"
    UUID=$(curl -sSf "$list_url" \
        | jq -r 'sort_by(.published_at) | reverse | .[0].uuid // empty')
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

EXPECTED=$(jq -r '.files[0].sha1' "$WORKDIR/tritonadm.imgmanifest")
if [[ -z "$EXPECTED" || "$EXPECTED" == "null" ]]; then
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
