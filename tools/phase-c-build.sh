#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

# Phase C build: produces a deploy bundle of mantad + tritond + tcadm
# for the SmartOS test box.
#
# Run this ON THE BUILD HOST (e.g. build02). It assumes:
#   - both repos are already checked out on this host
#   - Rust toolchain is installed and matches each repo's
#     rust-toolchain.toml
#   - the build host's target architecture matches 192.168.1.182
#     (i.e. you're either running this ON a SmartOS GZ already, OR
#     you have a cross-toolchain configured for x86_64-unknown-illumos)
#
# Usage:
#   sh phase-c-build.sh <monitor-reef-path> <manta-storage-path> [out-dir]
#
# Defaults:
#   out-dir = $PWD/phase-c-bundles
#
# Output:
#   <out-dir>/phase-c-bundle-<git-sha>.tar.gz
#
# The bundle layout is:
#   bin/mantad
#   bin/tritond
#   bin/tcadm
#   bin/mantad-adm           (the mantad operator CLI)
#   etc/mantad.toml.example  (a starter config; deploy script edits it)
#   COMMIT_SHAS              (one-line each: repo=sha, for the audit log)

set -eu

usage() {
    printf 'usage: %s <monitor-reef-path> <manta-storage-path> [out-dir]\n' "$0" >&2
    exit 2
}

[ $# -ge 2 ] || usage

MONITOR_REEF="$1"
MANTA_STORAGE="$2"
OUT_DIR="${3:-$PWD/phase-c-bundles}"

[ -d "$MONITOR_REEF" ] || { printf 'monitor-reef path %s does not exist\n' "$MONITOR_REEF" >&2; exit 1; }
[ -d "$MANTA_STORAGE" ] || { printf 'manta-storage path %s does not exist\n' "$MANTA_STORAGE" >&2; exit 1; }

mkdir -p "$OUT_DIR"

note() { printf '==> %s\n' "$*"; }

# Resolve absolute paths so cargo doesn't get tripped by relative refs.
MONITOR_REEF="$(cd "$MONITOR_REEF" && pwd)"
MANTA_STORAGE="$(cd "$MANTA_STORAGE" && pwd)"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

# Identify the build for the audit trail. Use both repos' current SHAs
# so the bundle is unambiguously identifiable later.
MR_SHA="$(cd "$MONITOR_REEF" && git rev-parse --short HEAD)"
MS_SHA="$(cd "$MANTA_STORAGE" && git rev-parse --short HEAD)"
BUNDLE_TAG="${MR_SHA}-${MS_SHA}"

note "monitor-reef HEAD=$MR_SHA"
note "manta-storage HEAD=$MS_SHA"
note "out-dir=$OUT_DIR"

# Detect the host target. The point of build02 is that this is the
# same triple as the deploy target — we are *not* cross-compiling.
HOST_TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
note "host target: $HOST_TARGET"

case "$HOST_TARGET" in
    x86_64-unknown-illumos|x86_64-sun-solaris) : ;;
    *) printf 'WARN: build host target %s does not match SmartOS 192.168.1.182 (x86_64-unknown-illumos).\n' "$HOST_TARGET" >&2
       printf '      The resulting binaries will not run on the test box. Continuing anyway.\n' >&2 ;;
esac

# Build tritond + tcadm from monitor-reef.
#
# `tritond --features foundationdb` is REQUIRED. Without it, the
# binary aborts on startup the moment `/etc/tritond/config.toml`
# carries `fdb_cluster_file` (which the deploy on 192.168.1.182
# does). The default-features tests don't exercise the FDB path,
# so this is the only place the feature gets exercised in our
# build pipeline today.
note "building monitor-reef (tritond, tcadm) — release profile, --features foundationdb"
(
    cd "$MONITOR_REEF"
    cargo build --release -p tritond --features foundationdb
    cargo build --release -p tcadm
)

# Build mantad + mantad-adm from manta-storage. mantad needs the `fdb`
# feature because Phase A's workspace methods are FDB-only.
note "building manta-storage (mantad, mantad-adm) — release profile, --features fdb"
(
    cd "$MANTA_STORAGE"
    cargo build --release -p mantad --features fdb
    cargo build --release -p mantad-adm
)

# Stage the bundle.
STAGE="$(mktemp -d -t phase-c-bundle.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/etc"

cp "$MONITOR_REEF/target/release/tritond" "$STAGE/bin/"
cp "$MONITOR_REEF/target/release/tcadm"   "$STAGE/bin/"
cp "$MANTA_STORAGE/target/release/mantad" "$STAGE/bin/"
cp "$MANTA_STORAGE/target/release/mantad-adm" "$STAGE/bin/"

# A starter mantad config the deploy script will patch with the real
# admin token + FDB cluster path. Kept here so the bundle is
# self-describing.
cat > "$STAGE/etc/mantad.toml.example" <<'TOML'
# Phase C verify deploy config. The deploy script overwrites
# admin_token and the FDB cluster file path before launch.
listen          = "0.0.0.0:7443"
internal_listen = "0.0.0.0:7101"
raft_listen     = "0.0.0.0:7102"
meta_dir        = "/var/mantad/meta"
data_dir        = "/var/mantad/data"
owner           = "root"
endpoint        = "http://192.168.1.182:7443"
node_id         = 1
peers           = ""
replication     = 1
TOML

cat > "$STAGE/COMMIT_SHAS" <<EOF
monitor-reef=$(cd "$MONITOR_REEF" && git rev-parse HEAD)
manta-storage=$(cd "$MANTA_STORAGE" && git rev-parse HEAD)
host_target=$HOST_TARGET
built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

# Pack.
OUT="$OUT_DIR/phase-c-bundle-${BUNDLE_TAG}.tar.gz"
(
    cd "$STAGE"
    tar -czf "$OUT" bin etc COMMIT_SHAS
)
note "wrote $OUT"
note "sha256: $(shasum -a 256 "$OUT" 2>/dev/null | awk '{print $1}' || digest -a sha256 "$OUT")"
