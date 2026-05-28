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

# Paths reflect the actual layout on 192.168.1.182 as of the
# 2026-05-28 verify-attempt — see the org-roam note
# `monitor-reef: Phase C deploy attempt 2026-05-28 — gotchas
# captured` for the inventory probe transcript.
TRITOND_BIN_DIR=/opt/tritond/bin
TCADM_BIN_DIR=/opt/triton/bin
MANTAD_BIN_DIR=/opt/mantad/bin
MANTAD_ETC_DIR=/opt/mantad/etc
TOKEN_FILE=$MANTAD_ETC_DIR/admin-token
MANTAD_LOG=/var/log/mantad.log
TRITOND_LOG=/var/log/tritond.log
TRITOND_CONFIG="${TRITOND_CONFIG:-/etc/tritond/config.toml}"
FDB_CLUSTER_FILE="${FDB_CLUSTER_FILE:-/etc/fdb/fdb.cluster}"
# Tritond and mantad both dlopen libfdb_c.so + libfmt.so.11 from
# /opt/fdb/lib. SMF was setting LD_LIBRARY_PATH for the existing
# tritond service; the nohup launch here has to mirror that.
FDB_LIB_DIR=/opt/fdb/lib

mkdir -p "$TRITOND_BIN_DIR" "$TCADM_BIN_DIR" "$MANTAD_BIN_DIR" "$MANTAD_ETC_DIR"
mkdir -p /var/mantad/meta /var/mantad/data

# ---------------------------------------------------------------------
# 1. Quiesce tritond (bound to :8080 per /etc/tritond/config.toml)
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
if [ -f "$TRITOND_BIN_DIR/tritond" ]; then
    cp "$TRITOND_BIN_DIR/tritond" "$TRITOND_BIN_DIR/tritond.prev"
    note "previous tritond backed up to $TRITOND_BIN_DIR/tritond.prev"
fi

# ---------------------------------------------------------------------
# 3. Unpack the bundle (per-component target dirs)
# ---------------------------------------------------------------------
note "unpacking $BUNDLE"
STAGE=$(mktemp -d -t phase-c-deploy.XXXXXX)
tar -xzf "$BUNDLE" -C "$STAGE"
cp "$STAGE/bin/tritond"    "$TRITOND_BIN_DIR/tritond"
cp "$STAGE/bin/tcadm"      "$TCADM_BIN_DIR/tcadm"
cp "$STAGE/bin/mantad"     "$MANTAD_BIN_DIR/mantad"
cp "$STAGE/bin/mantad-adm" "$MANTAD_BIN_DIR/mantad-adm"
rm -rf "$STAGE"
chmod 755 \
    "$TRITOND_BIN_DIR/tritond" \
    "$TCADM_BIN_DIR/tcadm" \
    "$MANTAD_BIN_DIR/mantad" \
    "$MANTAD_BIN_DIR/mantad-adm"

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
MANTAD_LISTEN=0.0.0.0:7443 \
MANTAD_INTERNAL_LISTEN=0.0.0.0:7101 \
MANTAD_RAFT_LISTEN=0.0.0.0:7102 \
MANTAD_ENDPOINT="http://$(hostname):7443" \
LD_LIBRARY_PATH="$FDB_LIB_DIR:${LD_LIBRARY_PATH:-}" \
nohup "$MANTAD_BIN_DIR/mantad" >> "$MANTAD_LOG" 2>&1 &
MANTAD_PID=$!
sleep 2
if ! kill -0 "$MANTAD_PID" 2>/dev/null; then
    note "mantad failed to start; last log lines:"
    tail -20 "$MANTAD_LOG" >&2
    fatal "mantad exited immediately"
fi
note "mantad pid=$MANTAD_PID"

# ---------------------------------------------------------------------
# 6. Launch tritond
# ---------------------------------------------------------------------
# Tritond's bootstrap config lives at /etc/tritond/config.toml on
# this box (carries `bind_address=0.0.0.0:8080` and the
# `fdb_cluster_file` line). LD_LIBRARY_PATH=/opt/fdb/lib has to
# be in the env — SMF set it for the prior installation; nohup
# doesn't inherit SMF env so we set it explicitly.
note "launching tritond (config=$TRITOND_CONFIG)"
LD_LIBRARY_PATH="$FDB_LIB_DIR:${LD_LIBRARY_PATH:-}" \
nohup "$TRITOND_BIN_DIR/tritond" serve --config "$TRITOND_CONFIG" \
    >> "$TRITOND_LOG" 2>&1 &
TRITOND_PID=$!
sleep 2
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
probe_port 127.0.0.1 8080 "tritond API"
note "both daemons listening"

cat <<EOF

next:
  $TCADM_BIN_DIR/tcadm storage cluster add --name mantad-01 \\
      --cluster-endpoint http://$(hostname):7101 \\
      --admin-token \$(cat $TOKEN_FILE) \\
      --surface s3 --json

  $TCADM_BIN_DIR/tcadm config set storage.default_s3_cluster_id <cluster-id-from-add>

  sh phase-c-verify.sh <silo-id>
EOF
