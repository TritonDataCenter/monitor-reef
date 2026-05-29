# `triton-fdb` zone image

Single-process FoundationDB server running in a joyent-minimal zone.
Built once per release stamp, published to the Manta channel as an
imgadm image bundle (`<stamp>.json` + `<stamp>.zfs.gz`).

## What's in it

- `/opt/fdb/bin/{fdbserver,fdbcli}` — illumos-native FDB 7.3 binaries
  (snapshotted from the working fdb zone at `/opt/fdb` on `.10` — see
  the `fdb_on_illumos` memory note for provenance).
- `/opt/fdb/lib/{libfdb_c.so,libstdc++.so.6,libfmt.so.11,libexecinfo.so.1}`
  — runtime deps the binaries link against.
- `/opt/triton/fdb/smf/triton-fdb.xml` — SMF service definition.
  Not under `/lib/svc/manifest/` because that path is a read-only
  loopback overlay from the PI in joyent-minimal zones, and not
  under `/var/svc/manifest/` because joyent-minimal disables the
  manifest-import service after first boot. Import is driven by
  the `user-script` customer_metadata entry below.
- `/var/svc/method/triton-fdb` — start method script. On every boot it
  first ensures the delegated dataset (`zones/<zone>/data`, `zoned=on`) is
  mounted at `/data` (its default mountpoint is `/zones/<uuid>/data`, which
  would silently put state on the zone root). Then first-boot init (reads
  `triton:fdb_*` mdata, writes `/etc/fdb/{public_ip,fdb.cluster}`, creates
  `/data/state/fdb/{data,log}`), sizes fdbserver memory from available RAM
  (target ~4 GiB/proc, capped at 85% of the smaller of the zone cap and the
  host physmem, floored at 512 MiB), backgrounds fdbserver, and on fresh
  provision spawns a one-shot subshell that waits for the server to be
  reachable and then issues `fdbcli configure new single ssd` to initialise
  the database. Marks `/data/version` after configure succeeds; the next
  boot is a normal attach.

State lives entirely on the delegated dataset under `/data/state/fdb/`,
which makes the image reprovision-safe: `vmadm reprovision <uuid>
<new-image-uuid>` swaps the code without touching the DB. The build also
appends an FDB env block to `/etc/profile` so an interactive root login has
`fdbcli` on `PATH` with `LD_LIBRARY_PATH=/opt/fdb/lib` and
`FDB_CLUSTER_FILE=/etc/fdb/fdb.cluster` set.

Zones provisioned **before** the delegated-dataset-mount fix wrote state to
the zone root `/data`; they must be reprovisioned (their state is rebuilt
fresh on the dataset, not migrated).

## How `tcadm setup` provisions a zone from this image

```
vmadm create -f <(cat <<EOF
{
    "brand": "joyent-minimal",
    "image_uuid": "<image uuid from channel manifest>",
    "alias": "triton-fdb",
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
        "triton:fdb_public_ip":      "<same IP as nics[0].ip>",
        "triton:fdb_cluster_secret": "<random per-cluster, e.g. openssl rand -hex 8>",
        "user-script":               "#!/bin/sh\nsvccfg import /opt/triton/fdb/smf/triton-fdb.xml\nsvcadm enable -s site/triton-fdb\n"
    }
}
EOF
)
```

The `user-script` runs via `svc:/smartdc/mdata:execute` on every
boot; both `svccfg import` and `svcadm enable` are idempotent so the
repeated execution is harmless. On the first boot it brings the
service online; subsequent boots are no-ops.

For single-node bringup the cluster secret can be any random hex
string; FDB uses it to authenticate clients. Adding peers later means
`fdbcli ... configure ...; coordinators auto`.

## Build flow

Runs on `.10` (or any SmartOS host with the `zones` pool and imgadm):

```bash
STAMP=$(date -u +%Y%m%dT%H%M%SZ) OUTPUT_DIR=/var/tmp \
    bash images/triton-fdb/build.sh
```

Produces:
- `/var/tmp/triton-fdb-<stamp>.zfs.gz`
- `/var/tmp/triton-fdb-<stamp>.json`

Then publish from anywhere with Manta creds + the publisher key:

```bash
tritoncloud-publish --channel edge image \
    --name triton-fdb \
    --stamp "$STAMP" \
    --uuid  "$(jq -r .uuid /var/tmp/triton-fdb-$STAMP.json)" \
    --manifest /var/tmp/triton-fdb-$STAMP.json \
    --content  /var/tmp/triton-fdb-$STAMP.zfs.gz \
    --data-format-version 730 \
    --data-format-min-read 730
```

## On-disk format / upgrade

`data_format_version = 730` (FDB 7.3 on-disk format). Reprovisions
across the same major version are safe; 7.x → 8.x must go through a
runbook-driven upgrade (FDB does not promise format compatibility
across major versions).

The channel manifest's `data_format_min_read` field is set to the
same value as `data_format_version`, so `tcadm image update` will
refuse a reprovision that would cross the format boundary.

## Provenance note

The FDB binaries currently shipped in this image were lifted from a
working fdb zone on `.10` rather than built from source. They are
**not tracked in git** because `fdbserver` is 110 MB and GitHub's
hard limit is 100 MB; instead, the source tarball lives at:

```
https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources/fdb-bits-7.3-illumos.tar.gz
```

`build.sh` curls this into `proto/opt/fdb/` on demand if it's
missing. Override via `FDB_BITS_URL=...`.

This lift-and-shift origin is fine for the first publish but should
be replaced with a reproducible build pipeline (cross-build on
Linux, ship the result to the same Manta path) before we treat the
image as a release artifact for anything other than the dev/PoC
channel.
