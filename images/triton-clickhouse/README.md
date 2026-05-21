# `triton-clickhouse` zone image

ClickHouse 26.3.10 (LTS) running in a joyent-minimal zone. Used by
tritond as the optional timeseries metrics backend (when
`metrics.backend = clickhouse` in `tcadm config`).

## What's in it

- `/opt/triton/clickhouse/bin/clickhouse` — 667 MB multi-call binary
  (built per `clickhouse-build/` in the parent workspace; lifted
  verbatim from the running dev zone on `.41`). Symlinks for
  `clickhouse-{server,client,local,benchmark,format,keeper,compressor,extract-from-config}`
  are created at build time.
- `/opt/triton/clickhouse/etc/config.xml.tmpl` — bootstrap config
  template with `__HTTP_PORT__` / `__TCP_PORT__` / `__LISTEN_HOST__` /
  `__DATA_DIR__` / `__LOG_DIR__` placeholders. Re-rendered into
  `/data/etc/clickhouse-server/config.xml` on every boot.
- `/opt/triton/clickhouse/etc/users.xml` — seed users config (default
  user, no password, access_management=1). Copied to `/data/etc/clickhouse-server/`
  on first boot only; ClickHouse mutates it via SQL `CREATE USER`
  (because `access_management=1`), so subsequent boots leave the
  on-disk copy alone.
- `/opt/triton/clickhouse/smf/triton-clickhouse.xml` — SMF service.
- `/var/svc/method/triton-clickhouse` — start method (mdata-get
  driven, backgrounds clickhouse-server, same pattern as triton-fdb /
  triton-tritond).

State lives entirely on the delegated dataset:
`/data/state/clickhouse/{data,log}`. Reprovision-safe.

## How `tcadm setup` provisions a zone from this image

```
vmadm create -f <(cat <<EOF
{
    "brand": "joyent-minimal",
    "image_uuid": "<image uuid from channel manifest>",
    "alias": "triton-clickhouse",
    "delegate_dataset": true,
    "ram": 8192,
    "cpu_cap": 200,
    "nics": [{
        "nic_tag": "admin",
        "ip": "<operator-supplied>",
        "netmask": "<...>",
        "gateway": "<...>"
    }],
    "customer_metadata": {
        "triton:clickhouse_http_port": "8123",
        "triton:clickhouse_tcp_port":  "9000",
        "triton:clickhouse_listen":    "0.0.0.0",
        "user-script": "#!/bin/sh\nset -e\nsvccfg import /opt/triton/clickhouse/smf/triton-clickhouse.xml\nsvcadm enable -s site/triton-clickhouse\n"
    }
}
EOF
)
```

After the zone is up, point tritond at it:

```
tcadm config set metrics.backend clickhouse
tcadm config set metrics.clickhouse_url http://<ch-zone-ip>:8123
# restart tritond to apply (svcadm restart site/triton-tritond inside
# the tritond zone)
```

## Build flow

Runs on `.10`:

```bash
STAMP=$(date -u +%Y%m%dT%H%M%SZ) OUTPUT_DIR=/var/tmp \
    bash images/triton-clickhouse/build.sh

tritoncloud-publish --channel edge image \
    --name triton-clickhouse \
    --stamp "$STAMP" \
    --uuid "$(jq -r .uuid /var/tmp/triton-clickhouse-$STAMP.json)" \
    --manifest /var/tmp/triton-clickhouse-$STAMP.json \
    --content  /var/tmp/triton-clickhouse-$STAMP.zfs.gz \
    --data-format-version 1 \
    --data-format-min-read 1
```

## On-disk format

`data_format_version = 1`. ClickHouse handles its own MergeTree
on-disk versioning internally; minor-version reprovisions are safe.
Major-version (e.g. 26.x → 27.x) upgrades may need ALTER TABLE work
and should be runbook-driven; the channel-manifest gate will refuse
a reprovision that crosses the format boundary once we bump
`data_format_version`.

## Provenance

The current ClickHouse binary in
`~~/public/tritoncloud/sources/clickhouse-26.3.10-illumos.tar.gz`
was lifted from the working zone at `/opt/clickhouse/bin/clickhouse`
on `8d337ccf-2693-4c7a-b1e4-d878c8fa45b0` (the dev metrics zone on
`.41`). It was built per the `clickhouse-build/` harness in the
parent workspace. A reproducible build pipeline that publishes the
binary directly from CI is a future commit; for now this is
dev-channel only.
