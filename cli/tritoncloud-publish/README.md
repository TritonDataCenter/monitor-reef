# `tritoncloud-publish`

Push a built release artifact (zone image, agent tarball, tcadm
binary) to Manta and update the signed channel manifest at
`~~/public/tritoncloud/channels/<channel>.json`.

See [`rfd/00006`](../../../rfd/00006/) for the broader design.

## Quick reference

```bash
# Bootstrap an empty channel (one-time per channel name).
tritoncloud-publish --channel edge init-channel

# Publish a tcadm binary for one target triple.
tritoncloud-publish --channel edge tcadm \
    --stamp $(date -u +%Y%m%dT%H%M%SZ) \
    --target x86_64-unknown-illumos \
    --tarball ./tcadm-illumos.tar.gz

# Publish a per-CN GZ tarball (gz-tools-style).
tritoncloud-publish --channel edge agent \
    --name tritonagent \
    --stamp 20260521T140000Z \
    --tarball ./tritonagent.tar.gz \
    --pi-min 20260518T184011Z

# Publish a zone image (imgadm manifest + content.zfs.gz).
tritoncloud-publish --channel edge image \
    --name triton-tritond \
    --stamp 20260521T140000Z \
    --uuid 9c3b8f00-1111-4222-8333-444555666777 \
    --manifest ./triton-tritond.json \
    --content  ./triton-tritond.zfs.gz \
    --pi-min 20260518T184011Z \
    --data-format-version 1 \
    --data-format-min-read 1

# Read back what's currently in a channel.
tritoncloud-publish --channel edge show
```

## Required environment

| Var | What |
|---|---|
| `MANTA_URL` | Manta endpoint, e.g. `https://us-central.manta.mnx.io` |
| `MANTA_USER` | Manta account, e.g. `nick.wilkens@mnxsolutions.com` |
| `MANTA_KEY_ID` | SSH key fingerprint Manta will use |
| `TRITONCLOUD_SECRET_KEY` | Path to your minisign `.key` file. Defaults to `~/.config/tritoncloud/publisher.key`. |
| `MINISIGN_PASSWORD` | Optional. If set, `minisign -S` uses it instead of prompting. |

## What it actually does

For every publish:

1. Read the artifact from disk; SHA-256 it.
2. `mput` the artifact to the right Manta path under `<base>/{images,agents,tcadm}/<name>/<stamp>.<ext>`.
3. `curl` the current channel JSON (or start fresh).
4. Mutate the relevant entry to point at the new stamp + sha256.
5. Bump `updated_at`, write JSON to a tempfile, `minisign -S` it.
6. `mput` both new `<channel>.json.new` and `<channel>.json.minisig.new`.
7. `mmv` them over the live names atomically.

Per-artifact files are immutable; only the channel JSON moves.

## Promotion

Not yet implemented as a verb. For now, promote a release by re-running
the publish subcommand against the target channel (the same stamp will
land in `stable.json` instead of `edge.json`, pointing at the same
already-uploaded artifact). A dedicated `promote` verb that copies
entries across channels without re-uploading is tracked separately.

## Required tools

- `mput` and `mmv` from node-manta (any recent version)
- `minisign` (pkgsrc / brew / apt)
- `curl` (for read-side channel fetching)

The Rust code is pure orchestration; it never reads or writes the
network directly.
