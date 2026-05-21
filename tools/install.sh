#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

#
# Triton Cloud bootstrap installer.
#
# Downloads the latest `tcadm` binary for this host's OS+arch from the
# Manta-hosted release channel, verifies the channel manifest's
# minisign signature against the publisher pubkey embedded below, and
# verifies the binary's sha256 against the (signed) manifest entry.
#
# Usage:
#   curl -fsSL https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/install.sh | sh
#
# Or, to inspect first (recommended for production hosts):
#   curl -fsSL https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/install.sh -o install.sh
#   less install.sh   # eyeball the embedded pubkey + the channel URL
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

# Publisher minisign pubkey. Compared transitively against the
# corresponding file at monitor-reef/cli/tcadm/publisher.pub. If you are
# reviewing this script before piping to sh, confirm the key id below
# (635EA35A162FDAE0) matches the one you expect.
PUBLISHER_PUBKEY="$(cat <<'PUBKEY'
untrusted comment: minisign public key 635EA35A162FDAE0
RWTg2i8WWqNeY4OQ6NTvSuLBHJtjqJ2LhOENDwwoKpRH7/nFFXzrfTWw
PUBKEY
)"

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
# 2. preflight: required tools
#----------------------------------------------------------------------

for tool in curl tar sha256sum minisign; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        # GNU coreutils on SmartOS is `gsha256sum`; macOS uses `shasum -a 256`.
        case "$tool" in
            sha256sum)
                if command -v gsha256sum >/dev/null 2>&1; then
                    sha256sum() { gsha256sum "$@"; }
                    continue
                fi
                if command -v shasum >/dev/null 2>&1; then
                    sha256sum() { shasum -a 256 "$@"; }
                    continue
                fi ;;
        esac
        fatal "$tool not found in PATH (install it and retry)"
    fi
done

#----------------------------------------------------------------------
# 3. fetch and verify the channel manifest
#----------------------------------------------------------------------

TMPDIR="$(mktemp -d -t triton-install.XXXXXX)"
trap 'rm -rf "$TMPDIR"' EXIT

note "fetching channel manifest"
curl -fsSL "$CHANNEL_URL"         -o "$TMPDIR/channel.json"
curl -fsSL "$CHANNEL_URL.minisig" -o "$TMPDIR/channel.json.minisig"
printf '%s\n' "$PUBLISHER_PUBKEY" > "$TMPDIR/publisher.pub"

note "verifying channel signature"
minisign -V -p "$TMPDIR/publisher.pub" \
         -m "$TMPDIR/channel.json" \
         -x "$TMPDIR/channel.json.minisig" \
    >/dev/null 2>&1 \
    || fatal "channel signature did NOT verify against publisher pubkey"

#----------------------------------------------------------------------
# 4. parse the manifest for this target's tcadm entry
#
# We avoid a JSON parser dependency by using a small sed/awk pipeline.
# This script is intentionally simple; the canonical reader is the
# `triton-channel` crate used by tcadm itself.
#----------------------------------------------------------------------

extract_field() {
    # extract_field <target> <field>
    #
    # Pulls the value of $2 from the JSON object at
    # .tcadm["$1"].$2 in the manifest. Assumes the publisher emits
    # well-formed JSON with one-field-per-line indentation (which our
    # publisher tool guarantees). For richer parsing, install jq and
    # we'll use it.
    target="$1"
    field="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r --arg t "$target" --arg f "$field" '.tcadm[$t][$f] // ""' "$TMPDIR/channel.json"
    else
        awk -v target="$target" -v field="$field" '
            $0 ~ "\"" target "\":" { in_target = 1; next }
            in_target && $0 ~ "^    }" { in_target = 0; next }
            in_target && $0 ~ "\"" field "\":" {
                # crude: trim "field": " ... ",
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
[ -n "$URL" ] || fatal "channel has no tcadm entry for $TARGET"
[ -n "$SHA" ] || fatal "channel entry for $TARGET is missing sha256"

#----------------------------------------------------------------------
# 5. download, verify, extract
#----------------------------------------------------------------------

TARBALL="$TMPDIR/tcadm.tar.gz"
note "downloading tcadm from $URL"
curl -fsSL "$URL" -o "$TARBALL"

note "verifying tcadm sha256"
echo "$SHA  $TARBALL" | sha256sum -c - >/dev/null 2>&1 \
    || fatal "downloaded tcadm sha256 does NOT match channel manifest"

mkdir -p "$INSTALL_DIR"
note "extracting to $INSTALL_DIR"
tar -C "$INSTALL_DIR" -xzf "$TARBALL"

#----------------------------------------------------------------------
# 6. report
#----------------------------------------------------------------------

if [ -x "$INSTALL_DIR/tcadm" ]; then
    note "tcadm installed to $INSTALL_DIR/tcadm"
    note "next step: $INSTALL_DIR/tcadm setup"
    if ! echo ":$PATH:" | grep -q ":$INSTALL_DIR:"; then
        printf 'note: %s is not on your PATH; add it to your shell rc.\n' "$INSTALL_DIR" >&2
    fi
else
    fatal "tarball extracted but $INSTALL_DIR/tcadm is not present"
fi
