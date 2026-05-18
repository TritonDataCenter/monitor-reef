#!/bin/sh
#
# proteus-overlays — SMF method script for the lofs overlays the
# proteus dataplane needs over SmartOS platform-image files.
#
# Two overlays:
#   /usr/vm/node_modules/VM.js          ← /var/tmp/VM.js.m1-proteus
#   /usr/lib/brand/jcommon/statechange  ← /var/tmp/statechange.m1-proteus
#
# The originals are read-only platform-image files that don't know
# about proteus pseudo-NIC tags; the patched copies handle the
# "global NIC proteus<N>" case in `vmadm create` and bhyve
# state-change hooks. Without these overlays vmadm fails with
# "Invalid nic tag" / "undefined VNIC" on any zone that has a
# proteus port.
#
# Idempotent: re-running `start` is a no-op when the mounts are
# already in place. `stop` is intentionally a no-op — the overlays
# are system state, not tritonagent state; we don't want disabling
# the service to silently break running zones.

set -u

VM_JS_SRC=/var/tmp/VM.js.m1-proteus
VM_JS_DST=/usr/vm/node_modules/VM.js
SC_SRC=/var/tmp/statechange.m1-proteus
SC_DST=/usr/lib/brand/jcommon/statechange

mount_lofs () {
    src=$1
    dst=$2
    if [ ! -f "$src" ]; then
        echo "proteus-overlays: missing source $src; skipping" >&2
        return 0
    fi
    if mount | grep -q " on $src "; then
        echo "proteus-overlays: $dst already overlaid"
        return 0
    fi
    echo "proteus-overlays: mount -F lofs $src $dst"
    mount -F lofs "$src" "$dst"
}

case "${1:-start}" in
start)
    mount_lofs "$VM_JS_SRC" "$VM_JS_DST"
    mount_lofs "$SC_SRC"    "$SC_DST"
    ;;
stop)
    # No-op by design: see the header comment.
    :
    ;;
status)
    mount | grep -E "VM\.js\.m1-proteus|statechange\.m1-proteus" || {
        echo "proteus-overlays: no overlays active"
        exit 1
    }
    ;;
*)
    echo "usage: $0 {start|stop|status}" >&2
    exit 2
    ;;
esac
