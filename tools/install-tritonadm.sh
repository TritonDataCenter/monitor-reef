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
# global zone. Fetches the latest tritonadm tarball from an updates server
# (default: updates.tritondatacenter.com), verifies its SHA-1 against the
# IMGAPI manifest, and installs to /opt/triton/tritonadm/.
#
# See docs/design/tritonadm-distribution.md for the full design.
#
# Three install modes:
#
#   * Network (default): resolve latest in --channel, download, verify SHA,
#     extract, install.
#   * Local tarball: --tarball + --manifest, skips download but still
#     verifies SHA before extract.
#   * Embedded: when this script is run from inside an extracted tarball
#     (i.e. an adjacent ./root/opt/triton/tritonadm/ exists), reuse that
#     payload. This is the path sdcadm experimental takes after it
#     downloads + extracts the image — sdcadm has already verified the
#     SHA against IMGAPI, so we trust the surrounding tree.
#
# All modes write the same /opt/triton/tritonadm/etc/version file:
#
#   uuid=<image-uuid>           # baked into tarball at build time
#   version=<build-stamp>       # eng-style: <branch>-<UTC>-g<sha>
#   installed_at=<iso8601>      # set by this script
#   source=embedded|network|local
#
# Usage:
#   install-tritonadm.sh [--channel <name>] [--updates-url <url>]
#   install-tritonadm.sh --uuid <image-uuid> [--updates-url <url>]
#   install-tritonadm.sh --tarball <path> --manifest <path>   # air-gapped
#

set -o errexit
set -o pipefail
set -o nounset

UPDATES_URL="${UPDATES_URL:-https://updates.tritondatacenter.com}"
CHANNEL="experimental"
INSTALL_DIR="/opt/triton/tritonadm"
SYMLINK="/opt/local/bin/tritonadm"
NO_SYMLINK=false
UUID=""
LOCAL_TARBALL=""
LOCAL_MANIFEST=""

usage() {
    cat <<EOF
Usage:
  $(basename "$0") [--channel <name>] [--updates-url <url>]
  $(basename "$0") --uuid <image-uuid> [--updates-url <url>]
  $(basename "$0") --tarball <path> --manifest <path>

Installs tritonadm into $INSTALL_DIR and symlinks $SYMLINK.

Options:
  --channel <name>         Updates channel (default: experimental)
  --updates-url <url>      Updates server (default: \$UPDATES_URL or
                             https://updates.tritondatacenter.com)
  --uuid <image-uuid>      Pin to a specific image UUID instead of "latest"
  --tarball <path>         Use a local tarball (skips network)
  --manifest <path>        Use a local manifest (required with --tarball)
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
        --tarball)      LOCAL_TARBALL="$2"; shift 2 ;;
        --manifest)     LOCAL_MANIFEST="$2"; shift 2 ;;
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
# Embedded mode: this script can be run from inside an extracted tarball,
# in which case the install payload is sitting next to it and we skip the
# fetch/verify dance. sdcadm experimental and operators who manually
# extract the tarball both end up here.
#
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
EMBEDDED_PAYLOAD="$SCRIPT_DIR/root/opt/triton/tritonadm"
EMBEDDED_MODE=false
if [[ -z "$LOCAL_TARBALL" && -z "$UUID" && -d "$EMBEDDED_PAYLOAD" ]]; then
    EMBEDDED_MODE=true
fi

if [[ "$EMBEDDED_MODE" == "true" ]]; then
    echo "==> Installing from embedded payload at $SCRIPT_DIR"
    # uuid + version are baked into the tarball at build time. Read them out
    # so the installed etc/version preserves the IMGAPI identity end-to-end.
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
else

WORKDIR=$(mktemp -d -t tritonadm-install.XXXXXX)
trap 'rm -rf "$WORKDIR"' EXIT

if [[ -n "$LOCAL_TARBALL" ]]; then
    #
    # Air-gapped path: caller already has the bits.
    #
    if [[ -z "$LOCAL_MANIFEST" ]]; then
        echo "error: --tarball requires --manifest" >&2
        exit 2
    fi
    cp "$LOCAL_TARBALL" "$WORKDIR/tritonadm.tgz"
    cp "$LOCAL_MANIFEST" "$WORKDIR/tritonadm.imgmanifest"
    INSTALL_SOURCE="local"
else
    INSTALL_SOURCE="network"
    #
    # Network path: resolve UUID against the updates server, then download.
    #
    echo "==> Resolving tritonadm image (channel=$CHANNEL)"

    if [[ -z "$UUID" ]]; then
        # IMGAPI: list active tritonadm images, sorted by published_at desc,
        # take the first.
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

    echo "==> Fetching tarball"
    curl -sSf -o "$WORKDIR/tritonadm.tgz" "$file_url"
fi

#
# Verify SHA-1 from manifest.
#
EXPECTED=$(jq -r '.files[0].sha1' "$WORKDIR/tritonadm.imgmanifest")
if [[ -z "$EXPECTED" || "$EXPECTED" == "null" ]]; then
    echo "error: manifest missing files[0].sha1" >&2
    exit 1
fi
ACTUAL=$(sha1_of "$WORKDIR/tritonadm.tgz")
if [[ "$EXPECTED" != "$ACTUAL" ]]; then
    echo "error: SHA-1 mismatch (expected $EXPECTED, got $ACTUAL)" >&2
    exit 1
fi
echo "==> SHA-1 verified ($ACTUAL)"

#
# Extract over INSTALL_DIR. Tarball layout: root/opt/triton/tritonadm/...
# We strip 'root/opt/triton/tritonadm/' so contents land directly under
# $INSTALL_DIR — that lets operators relocate via --install-dir.
#
echo "==> Installing to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
tar -xzf "$WORKDIR/tritonadm.tgz" \
    -C "$INSTALL_DIR" \
    --strip-components=4 \
    root/opt/triton/tritonadm

#
# Pull image identity from the manifest for the unified etc/version write
# below.
#
INSTALLED_UUID=$(jq -r '.uuid' "$WORKDIR/tritonadm.imgmanifest")
INSTALLED_VERSION=$(jq -r '.version' "$WORKDIR/tritonadm.imgmanifest")

fi  # end of !EMBEDDED_MODE branch

#
# Unified etc/version write — same shape regardless of install source. The
# 'source' field tells future self-updates how this install got here.
#
mkdir -p "$INSTALL_DIR/etc"
cat > "$INSTALL_DIR/etc/version" <<EOF
uuid=$INSTALLED_UUID
version=$INSTALLED_VERSION
installed_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
source=$INSTALL_SOURCE
EOF

#
# Symlink so 'tritonadm' is on PATH. The default target is /opt/local/bin/
# which exists on an illumos GZ. On other hosts (or if the parent dir isn't
# writable) we skip with a warning and tell the operator how to invoke it.
#
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
