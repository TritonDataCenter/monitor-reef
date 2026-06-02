#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# package.sh - build the vNext components and publish them to the signed
# channel so `tcadm update <name>` can pull them.
#
# The lockstep trio (tritond, tritonagent, tcadm) is ALWAYS built together
# in one cargo invocation — they share the blueprint postcard wire and
# must move in lockstep. adminui (admin-backend) builds alongside them
# (it tracks tritond/tritonagent). They are PUBLISHED individually so they
# update individually:
#
#   tritond, admin-backend  -> services/   (binary-swap; `tcadm update`)
#   tritonagent             -> agents/      (GZ tarball)
#   tcadm                   -> tcadm/
#
# Everything is cross-built on the illumos build host (never on the Mac)
# and published via `tritoncloud-publish` (minisign-signed channel JSON).
#
# Usage:
#   tools/package.sh [--channel edge|stable] [--no-publish] [--stamp S] [component...]
#     components: trio adminui  (or: tritond tritonagent tcadm admin-backend)
#     default: trio + adminui
#
# Env (defaults shown):
#   BUILD_HOST=142.147.4.194        illumos cross-build + publish host
#   BUILD_SSH="ssh root@$BUILD_HOST"
#   MR_DIR=/root/monitor-reef       monitor-reef checkout on the build host
#   ADMIN_DIR=/root/admin           admin checkout on the build host
#   The publish step needs Manta creds (MANTA_USER/MANTA_KEY_ID/MANTA_URL)
#   + MINISIGN_KEY/MINISIGN_PASSWORD present on the BUILD_HOST.

set -euo pipefail

# ── args ──────────────────────────────────────────────────────────────
CHANNEL=edge
PUBLISH=1
STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
COMPONENTS=()
while [ $# -gt 0 ]; do
    case "$1" in
        --channel) CHANNEL="$2"; shift 2 ;;
        --no-publish) PUBLISH=0; shift ;;
        --stamp) STAMP="$2"; shift 2 ;;
        -h|--help) sed -n '11,33p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        trio) COMPONENTS+=(tritond tritonagent tcadm); shift ;;
        adminui) COMPONENTS+=(admin-backend); shift ;;
        tritond|tritonagent|tcadm|admin-backend) COMPONENTS+=("$1"); shift ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done
# Default: the trio + adminui.
[ ${#COMPONENTS[@]} -eq 0 ] && COMPONENTS=(tritond tritonagent tcadm admin-backend)

# Always build the WHOLE trio if any of it is requested (lockstep).
want() { printf '%s\n' "${COMPONENTS[@]}" | grep -qx "$1"; }
if want tritond || want tritonagent || want tcadm; then
    for c in tritond tritonagent tcadm; do want "$c" || COMPONENTS+=("$c"); done
fi

BUILD_HOST=${BUILD_HOST:-142.147.4.194}
BUILD_SSH=${BUILD_SSH:-ssh root@$BUILD_HOST}
MR_DIR=${MR_DIR:-/root/monitor-reef}
ADMIN_DIR=${ADMIN_DIR:-/root/admin}

TOP=$(cd "$(dirname "$0")/.." && pwd)             # monitor-reef/
ADMIN_SRC=${ADMIN_SRC:-$(cd "$TOP/../admin" 2>/dev/null && pwd || true)}

echo "== package vNext components =="
echo "  stamp:     $STAMP"
echo "  channel:   $CHANNEL"
echo "  build:     $BUILD_HOST  ($MR_DIR, $ADMIN_DIR)"
echo "  build:     ${COMPONENTS[*]}"
echo "  publish:   $([ $PUBLISH -eq 1 ] && echo yes || echo no)"

run() { echo "+ $*"; $BUILD_SSH "$@"; }

# ── 1. frontend (Mac) — adminui embeds frontend/dist ──────────────────
if want admin-backend; then
    if [ -n "$ADMIN_SRC" ] && [ -d "$ADMIN_SRC/frontend" ]; then
        echo "== build frontend (local) =="
        ( cd "$ADMIN_SRC/frontend" && ./node_modules/.bin/vite build )
        echo "== sync admin tree -> $BUILD_HOST:$ADMIN_DIR =="
        rsync -az --exclude=target --exclude=node_modules --exclude=.git \
            "$ADMIN_SRC/" "root@$BUILD_HOST:$ADMIN_DIR/"
        run "touch $ADMIN_DIR/backend/src/assets.rs"
    else
        echo "WARN: admin tree not found ($TOP/../admin); skipping adminui" >&2
        COMPONENTS=("${COMPONENTS[@]/admin-backend/}")
    fi
fi

# ── 2. sync monitor-reef + cross-build ────────────────────────────────
echo "== sync monitor-reef -> $BUILD_HOST:$MR_DIR =="
rsync -az --exclude=target --exclude=node_modules --exclude=.git --exclude=rust --exclude=.cargo \
    "$TOP/" "root@$BUILD_HOST:$MR_DIR/"

echo "== cross-build the trio + tritoncloud-publish =="
# Two invocations on purpose. tritond needs the `foundationdb` feature, but
# tritonagent + tcadm also depend on tritond-store, so building them in the
# SAME invocation would unify `tritond-store/foundationdb` into them (cargo
# feature unification) and leave the agent linked against libfdb_c — which
# the GZ can't satisfy, producing a broken agent. Build the fdb side and the
# no-fdb side separately; the trio still ships in lockstep, just compiled apart.
run "cd $MR_DIR && . /opt/tritoncloud/build-env.sh && \
     cargo build --release -p tritond -p tritoncloud-publish \
         --features tritond/foundationdb"
run "cd $MR_DIR && . /opt/tritoncloud/build-env.sh && \
     cargo build --release -p tritonagent -p tcadm"

if want admin-backend; then
    echo "== cross-build admin-backend (adminui) =="
    run "cd $ADMIN_DIR && \
         CARGO_HOME=/opt/cargo-home CARGO_TARGET_DIR=/opt/cargo-target-admin \
         RUSTC_WRAPPER=/opt/local/bin/sccache SCCACHE_DIR=/opt/sccache \
         PATH=/root/.cargo/bin:/opt/local/bin:/usr/bin:/usr/sbin \
         SSL_CERT_FILE=/opt/local/share/mozilla-rootcerts/cacert.pem \
         cargo build --release -p admin-backend"
fi

if [ $PUBLISH -eq 0 ]; then
    echo "== build done (--no-publish) =="
    echo "binaries on $BUILD_HOST:"
    echo "  $MR_DIR/../../opt/cargo-target/release/{tritond,tritonagent,tcadm}  (CARGO_TARGET_DIR)"
    echo "  /opt/cargo-target-admin/release/admin-backend"
    exit 0
fi

# ── 3. publish each as its update artifact (on the build host) ────────
# CARGO_TARGET_DIR from build-env.sh is /opt/cargo-target.
TGT=/opt/cargo-target/release
PUB="$TGT/tritoncloud-publish"
PFX="cd $MR_DIR && PATH=/opt/local/bin:\$PATH $PUB --channel $CHANNEL"

publish_service() { # name zone bin_path smf binary
    run "$PFX service --name $1 --stamp $STAMP --zone $2 --bin-path $3 --smf $4 --binary $5"
}

if want tritond; then
    echo "== publish tritond (service) =="
    publish_service tritond triton-tritond /opt/triton/tritond/bin/tritond \
        site/triton-tritond "$TGT/tritond"
fi

if want admin-backend; then
    echo "== publish admin-backend / adminui (service) =="
    publish_service admin-backend triton-tritond /opt/triton/admin-backend/bin/admin-backend \
        site/admin-backend /opt/cargo-target-admin/release/admin-backend
fi

if want tritonagent; then
    echo "== publish tritonagent (agent tarball) =="
    # Drop the freshly-built binary into the agent proto so build.sh tars
    # it instead of fetching the stale one from sources/.
    run "cp -p $TGT/tritonagent $MR_DIR/agents/tritonagent/proto/opt/triton/tritonagent/bin/tritonagent && \
         STAMP=$STAMP OUTPUT_DIR=/var/tmp bash $MR_DIR/agents/tritonagent/build.sh >/var/tmp/agent-build.log 2>&1 && \
         $PFX agent --name tritonagent --stamp $STAMP --tarball /var/tmp/tritonagent-$STAMP.tar.gz"
fi

if want tcadm; then
    echo "== publish tcadm =="
    run "cd $MR_DIR && rm -rf /var/tmp/tcadm-stage && mkdir -p /var/tmp/tcadm-stage && \
         cp -p $TGT/tcadm /var/tmp/tcadm-stage/tcadm && chmod 0755 /var/tmp/tcadm-stage/tcadm && \
         (cd /var/tmp/tcadm-stage && tar -czf /var/tmp/tcadm-$STAMP.tar.gz tcadm) && \
         PATH=/opt/local/bin:\$PATH $PUB --channel $CHANNEL tcadm \
             --stamp $STAMP --target x86_64-unknown-illumos --tarball /var/tmp/tcadm-$STAMP.tar.gz"
fi

echo ""
echo "== published to channel '$CHANNEL' at stamp $STAMP =="
echo "On the headnode GZ, pull the updates:"
echo "  tcadm update --check"
echo "  tcadm update tritond adminui tritonagent     # or: tcadm update --all"
