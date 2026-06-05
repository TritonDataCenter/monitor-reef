#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

#
# Triton Cloud bootstrap installer.
#
# Downloads the latest `tritonadm` binary for this host's OS+arch from the
# Manta-hosted release channel and verifies the binary's sha256
# against the manifest entry.
#
# Trust model: TLS to Manta + sha256 of the artifact against the
# (TLS-protected) channel JSON. `tritonadm` itself ships with the
# `minisign-verify` Rust crate built in and uses it on every
# self-update; the script-bootstrap path keeps things lean and does
# not require a minisign CLI on the target host (SmartOS PIs do not
# ship one). Operators who want stronger upstream integrity can do
# `tritonadm self-update --check` after installation; that path enforces
# the publisher signature.
#
# Usage:
#   curl -fsSL https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/install.sh | sh
#
# Or, to inspect first (recommended for production hosts):
#   curl -fsSL .../install.sh -o install.sh
#   less install.sh                  # eyeball the channel URL
#   sh install.sh
#
# Overrides via env:
#   TRITON_CHANNEL_URL   override the channel manifest URL
#                        (default: stable channel)
#   TRITON_INSTALL_DIR   override install dir
#                        (default: /opt/triton/bin on illumos,
#                                  $HOME/.local/bin elsewhere)
#

set -eu

CHANNEL_URL="${TRITON_CHANNEL_URL:-https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/channels/stable.json}"

#----------------------------------------------------------------------
# helpers
#----------------------------------------------------------------------

fatal() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '==> %s\n' "$*"
}

#----------------------------------------------------------------------
# 1. detect OS / arch, pick the target triple to look up in the manifest
#----------------------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS/$ARCH" in
    SunOS/i86pc)        TARGET=x86_64-unknown-illumos
                        DEFAULT_INSTALL_DIR=/opt/triton/bin ;;
    Darwin/arm64)       TARGET=aarch64-apple-darwin
                        DEFAULT_INSTALL_DIR="$HOME/.local/bin" ;;
    Darwin/x86_64)      TARGET=x86_64-apple-darwin
                        DEFAULT_INSTALL_DIR="$HOME/.local/bin" ;;
    Linux/x86_64)       TARGET=x86_64-unknown-linux-gnu
                        DEFAULT_INSTALL_DIR="$HOME/.local/bin" ;;
    *) fatal "unsupported platform: $OS/$ARCH" ;;
esac

INSTALL_DIR="${TRITON_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"

#----------------------------------------------------------------------
# 2. preflight: required tools, with portability fallbacks
#----------------------------------------------------------------------

# sha256: prefer sha256sum (GNU), then gsha256sum (SmartOS pkgsrc
# coreutils), then `shasum -a 256` (macOS / OpenBSD), then illumos
# native `digest -a sha256`. Wraps each in a uniform `sha256_hash`
# helper that prints just the lowercase hex digest of $1.
if command -v sha256sum >/dev/null 2>&1; then
    sha256_hash() { sha256sum "$1" | awk '{print $1}'; }
elif command -v gsha256sum >/dev/null 2>&1; then
    sha256_hash() { gsha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_hash() { shasum -a 256 "$1" | awk '{print $1}'; }
elif command -v digest >/dev/null 2>&1; then
    sha256_hash() { digest -a sha256 "$1"; }
else
    fatal "no sha256 tool found (need one of: sha256sum, gsha256sum, shasum, digest)"
fi

# tar: prefer GNU tar (gtar on SmartOS) because BSD tar / SunOS tar
# do not accept `-C dir`. Falls back to tar if it accepts `-C`.
if command -v gtar >/dev/null 2>&1; then
    untar_to() { gtar -C "$1" -xzf "$2"; }
elif command -v tar >/dev/null 2>&1; then
    # Probe for `tar -C` support; if it fails, fall back to cd + extract.
    if tar -C /tmp -tzf - </dev/null >/dev/null 2>&1; then
        untar_to() { tar -C "$1" -xzf "$2"; }
    else
        untar_to() { (cd "$1" && tar -xzf "$2"); }
    fi
else
    fatal "tar not found in PATH"
fi

for tool in curl; do
    command -v "$tool" >/dev/null 2>&1 \
        || fatal "$tool not found in PATH (install it and retry)"
done

#----------------------------------------------------------------------
# 3. fetch the channel manifest over TLS
#----------------------------------------------------------------------

TMPDIR="$(mktemp -d -t triton-install.XXXXXX)"
trap 'rm -rf "$TMPDIR"' EXIT

note "fetching channel manifest"
curl -fsSL "$CHANNEL_URL" -o "$TMPDIR/channel.json"

#----------------------------------------------------------------------
# 4. parse the manifest for this target's tritonadm entry
#
# We avoid a JSON parser dependency by using a small awk fallback when
# jq is not present. The publisher emits one-field-per-line indented
# JSON which awk can pick apart safely; richer parsing only matters
# for tritonadm itself (which uses the triton-channel crate).
#----------------------------------------------------------------------

extract_field() {
    target="$1"
    field="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r --arg t "$target" --arg f "$field" '.tritonadm[$t][$f] // ""' "$TMPDIR/channel.json"
    else
        awk -v target="$target" -v field="$field" '
            $0 ~ "\"" target "\":" { in_target = 1; next }
            in_target && $0 ~ "^    }" { in_target = 0; next }
            in_target && $0 ~ "\"" field "\":" {
                line = $0
                sub(/^[^:]*: *"?/, "", line)
                sub(/"?,?$/, "", line)
                print line
                exit
            }
        ' "$TMPDIR/channel.json"
    fi
}

URL="$(extract_field "$TARGET" url)"
SHA="$(extract_field "$TARGET" sha256)"
[ -n "$URL" ] || fatal "channel has no tritonadm entry for $TARGET"
[ -n "$SHA" ] || fatal "channel entry for $TARGET is missing sha256"

#----------------------------------------------------------------------
# 5. download, verify sha256, extract
#----------------------------------------------------------------------

TARBALL="$TMPDIR/tritonadm.tar.gz"
note "downloading tritonadm from $URL"
curl -fsSL "$URL" -o "$TARBALL"

note "verifying tritonadm sha256"
ACTUAL_SHA=$(sha256_hash "$TARBALL")
[ "$ACTUAL_SHA" = "$SHA" ] \
    || fatal "downloaded tritonadm sha256 does NOT match channel manifest (expected $SHA, got $ACTUAL_SHA)"

mkdir -p "$INSTALL_DIR"
note "extracting to $INSTALL_DIR"
untar_to "$INSTALL_DIR" "$TARBALL"

#----------------------------------------------------------------------
# 6. report
#----------------------------------------------------------------------

if [ -x "$INSTALL_DIR/tritonadm" ]; then
    note "tritonadm installed to $INSTALL_DIR/tritonadm"
    note "next step: $INSTALL_DIR/tritonadm setup"
    if ! echo ":$PATH:" | grep -q ":$INSTALL_DIR:"; then
        printf 'note: %s is not on your PATH; add it to your shell rc.\n' "$INSTALL_DIR" >&2
    fi
else
    fatal "tarball extracted but $INSTALL_DIR/tritonadm is not present"
fi
