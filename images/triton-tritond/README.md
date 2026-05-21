# `triton-tritond` zone image

The Triton Cloud control-plane daemon (`tritond`) running in a
joyent-minimal zone. Built once per release stamp, published to the
Manta channel as an imgadm image bundle.

## What's in it

- `/opt/triton/tritond/bin/tritond` — release binary built with
  `--features foundationdb`. Fetched on demand at build time from
  `~~/public/tritoncloud/sources/tritond-illumos.bin` (gitignored;
  the release binary is too large for github's 100 MB limit).
- `/opt/triton/tritond/lib/libfdb_c.so` — extracted from the same
  fdb-bits tarball used by `triton-fdb`. tritond links against this
  at runtime; `LD_LIBRARY_PATH` in the start method points here.
- `/opt/triton/tritond/etc/config.toml.tmpl` — bootstrap-config
  template with `__BIND_ADDRESS__` / `__FDB_CLUSTER_FILE__` /
  `__LOG_FILTER__` placeholders. Re-rendered into
  `/data/etc/tritond/config.toml` on every boot.
- `/opt/triton/tritond/smf/triton-tritond.xml` — SMF service.
  Lives under `/opt/triton/<svc>/smf/` and is imported by the
  vmadm `user-script` customer_metadata entry, same shape as
  triton-fdb.
- `/var/svc/method/triton-tritond` — start method. Reads
  `triton:fdb_cluster_secret` + `triton:fdb_cluster_peers` from
  mdata, renders the FDB cluster file + bootstrap TOML, backgrounds
  tritond, exits 0.

State (FDB-backed) lives on the delegated dataset:
`/data/etc/tritond/*` (rendered config + cluster file),
`/data/state/tritond/server.out` (captured stderr; the one-time
root password lands here on first boot of a fresh FDB cluster).

## How `tcadm setup` provisions a zone from this image

```
vmadm create -f <(cat <<EOF
{
    "brand": "joyent-minimal",
    "image_uuid": "<image uuid from channel manifest>",
    "alias": "triton-tritond",
    "delegate_dataset": true,
    "ram": 4096,
    "cpu_cap": 200,
    "nics": [{
        "nic_tag": "admin",
        "ip": "<operator-supplied>",
        "netmask": "<...>",
        "gateway": "<...>"
    }],
    "customer_metadata": {
        "triton:fdb_cluster_secret": "<same as triton-fdb zone>",
        "triton:fdb_cluster_peers":  "<comma-separated FDB zone IPs>",
        "triton:tritond_bind_address": "0.0.0.0:8080",
        "triton:tritond_log_filter":   "info",
        "user-script": "#!/bin/sh\nset -e\nsvccfg import /opt/triton/tritond/smf/triton-tritond.xml\nsvcadm enable -s site/triton-tritond\n"
    }
}
EOF
)
```

After the zone starts, retrieve the one-time root password:

```
zlogin <zone-uuid> grep -A1 "WRITE THIS DOWN" /data/state/tritond/server.out
```

…then `tcadm configure --endpoint <zone-ip>:8080` from a workstation
to log in.

## Build flow

Runs on `.10`:

```bash
# Once: build tritond on .10 (takes 5–15 min depending on cache).
cd /opt/tcadm-build && \
  PATH=/opt/tools/bin:$PATH \
  CARGO_TARGET_DIR=/opt/tcadm-build/target \
  CARGO_HOME=/opt/cargo-home \
  LIBRARY_PATH=/opt/fdb/lib \
  cargo build -p tritond --release --features foundationdb -j 2

# Publish the binary to Manta sources (one-off per build).
mput -f /opt/tcadm-build/target/release/tritond \
  /nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/tritond-illumos.bin

# Build the image.
STAMP=$(date -u +%Y%m%dT%H%M%SZ) OUTPUT_DIR=/var/tmp \
  bash images/triton-tritond/build.sh

# Publish.
tritoncloud-publish --channel edge image \
    --name triton-tritond \
    --stamp "$STAMP" \
    --uuid "$(jq -r .uuid /var/tmp/triton-tritond-$STAMP.json)" \
    --manifest /var/tmp/triton-tritond-$STAMP.json \
    --content  /var/tmp/triton-tritond-$STAMP.zfs.gz \
    --data-format-version 1 \
    --data-format-min-read 1
```

## On-disk format

`data_format_version = 1`. Most tritond state lives in FDB; the
delegated dataset only holds the rendered config + the captured
stderr log. The version number is reserved for future schema
changes (e.g., locally-cached blueprints, audit-chain spool).
