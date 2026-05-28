#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

# Phase C deploy: unpack the bundle on the test box, restart tritond,
# stand up mantad as a sibling daemon, verify both are listening.
#
# Run this ON THE TEST BOX (192.168.1.182) as root.
#
# Usage:
#   sh phase-c-deploy.sh <bundle.tar.gz> [admin-token]
#
# If admin-token is omitted, a fresh random one is generated and
# written to /opt/triton/etc/mantad-admin-token (mode 600). The same
# token will need to be passed to `tcadm storage cluster create
# --admin-token <token>` afterwards.
#
# Side effects (in order):
#   1. SIGTERM the running tritond, wait up to 10s, SIGKILL if alive.
#   2. Back up the current /opt/triton/bin/tritond to .prev.
#   3. Unpack the bundle into /opt/triton/{bin,etc}.
#   4. Create /var/mantad/{meta,data} and /var/log if absent.
#   5. Render the mantad config from the example template.
#   6. Launch mantad under nohup, log to /var/log/mantad.log.
#   7. Launch tritond under nohup, log to /var/log/tritond.log.
#   8. Probe both ports until reachable or 30s timeout.
#
# SMF wrapping is intentionally out of scope. This is the verify
# rig, not the production deploy.

set -eu

usage() {
    printf 'usage: %s <bundle.tar.gz> [admin-token]\n' "$0" >&2
    exit 2
}

[ $# -ge 1 ] || usage

BUNDLE="$1"
ADMIN_TOKEN_ARG="${2:-}"

[ -f "$BUNDLE" ] || { printf 'bundle %s not found\n' "$BUNDLE" >&2; exit 1; }
[ "$(id -u)" -eq 0 ] || { printf 'must run as root\n' >&2; exit 1; }

note() { printf '==> %s\n' "$*"; }
fatal() { printf 'phase-c-deploy: %s\n' "$*" >&2; exit 1; }

BIN_DIR=/opt/triton/bin
ETC_DIR=/opt/triton/etc
TOKEN_FILE=$ETC_DIR/mantad-admin-token
MANTAD_LOG=/var/log/mantad.log
TRITOND_LOG=/var/log/tritond.log
FDB_CLUSTER_FILE="${FDB_CLUSTER_FILE:-/etc/foundationdb/fdb.cluster}"

mkdir -p "$BIN_DIR" "$ETC_DIR"
mkdir -p /var/mantad/meta /var/mantad/data

# ---------------------------------------------------------------------
# 1. Quiesce tritond (it's currently using port 8443 / API)
# ---------------------------------------------------------------------
if pgrep -x tritond > /dev/null 2>&1; then
    note "stopping existing tritond"
    pkill -x tritond || true
    i=0
    while pgrep -x tritond > /dev/null 2>&1 && [ "$i" -lt 10 ]; do
        sleep 1
        i=$((i+1))
    done
    if pgrep -x tritond > /dev/null 2>&1; then
        note "tritond still alive after 10s, sending SIGKILL"
        pkill -KILL -x tritond || true
    fi
fi
# Mantad may also be running from a prior verify; stop it too.
if pgrep -x mantad > /dev/null 2>&1; then
    note "stopping existing mantad"
    pkill -x mantad || true
    sleep 2
    pkill -KILL -x mantad 2>/dev/null || true
fi

# ---------------------------------------------------------------------
# 2. Back up the current binary
# ---------------------------------------------------------------------
if [ -f "$BIN_DIR/tritond" ]; then
    cp "$BIN_DIR/tritond" "$BIN_DIR/tritond.prev"
    note "previous tritond backed up to $BIN_DIR/tritond.prev"
fi

# ---------------------------------------------------------------------
# 3. Unpack the bundle
# ---------------------------------------------------------------------
note "unpacking $BUNDLE into /opt/triton"
tar -xzf "$BUNDLE" -C /opt/triton
chmod 755 "$BIN_DIR"/tritond "$BIN_DIR"/tcadm "$BIN_DIR"/mantad "$BIN_DIR"/mantad-adm

# ---------------------------------------------------------------------
# 4. Mantad admin token + config
# ---------------------------------------------------------------------
if [ -n "$ADMIN_TOKEN_ARG" ]; then
    ADMIN_TOKEN="$ADMIN_TOKEN_ARG"
elif [ -f "$TOKEN_FILE" ]; then
    note "reusing existing admin token at $TOKEN_FILE"
    ADMIN_TOKEN="$(cat "$TOKEN_FILE")"
else
    note "generating a fresh mantad admin token"
    ADMIN_TOKEN="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
fi
umask 077
printf '%s\n' "$ADMIN_TOKEN" > "$TOKEN_FILE"
chmod 600 "$TOKEN_FILE"

if [ ! -f "$FDB_CLUSTER_FILE" ]; then
    fatal "FDB cluster file not found at $FDB_CLUSTER_FILE. Set FDB_CLUSTER_FILE=<path> and rerun."
fi

# We don't use the toml config — mantad takes everything from CLI/env.
# The example was packaged for human reference; we drive the daemon by
# env so the deploy is reproducible from this script alone.

# ---------------------------------------------------------------------
# 5. Launch mantad
# ---------------------------------------------------------------------
note "launching mantad (--meta-plane=fdb, admin token gated)"
MANTAD_ADMIN_TOKEN="$ADMIN_TOKEN" \
MANTAD_META_PLANE=fdb \
MANTAD_FDB_CLUSTER_FILE="$FDB_CLUSTER_FILE" \
MANTAD_AUTO_MEMBERSHIP=true \
nohup "$BIN_DIR/mantad" >> "$MANTAD_LOG" 2>&1 &
MANTAD_PID=$!
sleep 1
if ! kill -0 "$MANTAD_PID" 2>/dev/null; then
    note "mantad failed to start; last log lines:"
    tail -20 "$MANTAD_LOG" >&2
    fatal "mantad exited immediately"
fi
note "mantad pid=$MANTAD_PID"

# ---------------------------------------------------------------------
# 6. Launch tritond
# ---------------------------------------------------------------------
# Tritond's bootstrap config + env from the existing deploy must
# already exist on this box. We don't touch its config — just swap
# the binary and restart.
note "launching tritond"
nohup "$BIN_DIR/tritond" >> "$TRITOND_LOG" 2>&1 &
TRITOND_PID=$!
sleep 1
if ! kill -0 "$TRITOND_PID" 2>/dev/null; then
    note "tritond failed to start; last log lines:"
    tail -20 "$TRITOND_LOG" >&2
    fatal "tritond exited immediately"
fi
note "tritond pid=$TRITOND_PID"

# ---------------------------------------------------------------------
# 7. Probe ports
# ---------------------------------------------------------------------
probe_port() {
    host="$1"; port="$2"; name="$3"
    i=0
    while [ "$i" -lt 30 ]; do
        if (echo > "/dev/tcp/$host/$port") 2>/dev/null \
           || nc -z "$host" "$port" 2>/dev/null; then
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    fatal "$name did not start listening on $host:$port within 30s"
}

probe_port 127.0.0.1 7101 "mantad admin"
probe_port 127.0.0.1 8443 "tritond API"
note "both daemons listening"

cat <<EOF

next:
  tcadm storage cluster add --name mantad-01 \\
                            --cluster-endpoint http://192.168.1.182:7101 \\
                            --admin-token \$(cat $TOKEN_FILE) \\
                            --surface s3 --json

  tcadm config set storage.default_s3_cluster_id <cluster-id-from-add>

  sh phase-c-verify.sh <silo-id>
EOF
