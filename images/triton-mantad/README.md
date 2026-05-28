# `triton-mantad` zone image

Single-node Manta S3 daemon (`mantad`) running in a joyent-minimal
zone. Used as the in-cluster S3 endpoint that the tritond IMGAPI
points at for image-blob storage, replacing the dependency on a
reachable public Manta. Built once per release stamp, published to
the tritoncloud Manta channel as an imgadm image bundle.

## What's in it

- `/opt/triton/mantad/bin/mantad` — release binary built without
  the `fdb` feature (default `meta_plane = "raft"` for single-node).
  Fetched on demand at build time from
  `~~/public/tritoncloud/sources/mantad-illumos.bin` (gitignored;
  release binary is too large for github's 100 MB limit).
- `/opt/triton/mantad/bin/mantad-adm` — cluster admin CLI
  (placement, status, node ops). Same Manta source.
- `/opt/triton/mantad/smf/triton-mantad.xml` — SMF service.
  Imported by the vmadm `user-script` customer_metadata entry at
  first boot.
- `/var/svc/method/triton-mantad` — start method. Reads
  `triton:mantad_*` mdata, renders `/data/etc/mantad/mantad.toml`,
  mints secrets on first boot into `/data/etc/mantad/secrets.env`,
  backgrounds mantad, exits 0.

## Reprovision-safe layout

Everything stateful lives on the delegated `/data` dataset; the
binaries on `/opt/triton/mantad/` are immutable and get replaced on
`vmadm reprovision`:

| Path | Purpose |
|---|---|
| `/data/version` | sentinel; presence => attach-existing, skip bootstrap |
| `/data/etc/mantad/mantad.toml` | rendered every boot from mdata |
| `/data/etc/mantad/secrets.env` | minted once on first boot; admin token + sigv4 root creds (mode 0600) |
| `/data/state/mantad/meta/` | raft + fjall metadata |
| `/data/state/mantad/data/` | local object-store blobs |
| `/data/state/mantad/identity/` | standalone identity fjall keyspace |
| `/data/state/mantad/log/` | mantad stdout/stderr captures |

A reprovision swaps the root dataset (new binaries) but leaves
`/data` intact: secrets, buckets, objects, and identity all
survive. The method script's `EXPECTED_DATA_VERSION` check guards
against incompatible on-disk format upgrades.

## Operator metadata

All consumed by `/var/svc/method/triton-mantad` via `mdata-get`. All
optional; defaults are noted.

| Key | Default | Purpose |
|---|---|---|
| `triton:mantad_public_listen` | `0.0.0.0:7443` | S3 endpoint |
| `triton:mantad_internal_listen` | `127.0.0.1:7101` | admin + raft mesh |
| `triton:mantad_region` | `us-east-1` | SigV4 region label |
| `triton:mantad_endpoint_url` | `http://<primary-ip>:7443` | endpoint emitted in S3 responses |
| `triton:mantad_admin_token` | _auto-minted_ | override the auto-generated admin token |
| `triton:mantad_root_access_key_id` | _auto-minted_ | override the auto-generated SigV4 root AKID |
| `triton:mantad_root_secret_access_key` | _auto-minted_ | override the auto-generated SigV4 root secret |

Auto-mint runs once on first boot and persists into
`/data/etc/mantad/secrets.env`; subsequent boots read from there.
To rotate, delete `secrets.env` and `svcadm restart`.

## First-boot bucket creation

The start method intentionally does NOT create buckets — bucket
policy is operator policy, not zone policy. After the zone is up:

```sh
# From any host reachable to the public listener:
export AWS_ACCESS_KEY_ID=$(...)        # from secrets.env on the zone
export AWS_SECRET_ACCESS_KEY=$(...)
export AWS_ENDPOINT_URL=http://<zone-ip>:7443

aws s3 mb s3://triton-images
aws s3api put-bucket-policy --bucket triton-images --policy '{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": "*",
    "Action": "s3:GetObject",
    "Resource": "arn:aws:s3:::triton-images/*"
  }]
}'
```

Once the bucket policy is in place, agents on every CN can do
plain anonymous HTTPS GETs (mantad's `is_readable_object` evaluates
the policy first; `Principal: "*"` + `Allow` lets unauthenticated
callers through). PUT remains SigV4-signed against the root creds.

## How `tcadm setup` provisions a zone from this image

```
vmadm create -f <(cat <<EOF
{
    "brand": "joyent-minimal",
    "image_uuid": "<image uuid from channel manifest>",
    "alias": "mantad-0",
    "delegate_dataset": true,
    "ram": 4096,
    "quota": 100,
    "nics": [{
        "interface": "net0",
        "nic_tag": "admin",
        "ip": "<allocated>"
    }],
    "customer_metadata": {
        "triton:mantad_region": "us-east-1",
        "triton:mantad_endpoint_url": "http://<allocated>:7443",
        "user-script": "/usr/sbin/svccfg import /opt/triton/mantad/smf/triton-mantad.xml && /usr/sbin/svcadm enable site/triton-mantad"
    }
}
EOF
)
```

## Upgrade

```
tcadm install triton-mantad                  # imgadm install latest
vmadm reprovision <zone-uuid> <new-uuid>     # swap root dataset
```

`/data` survives; mantad picks up where it left off with the same
buckets, objects, and credentials.
