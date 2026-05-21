# `proteusadm` agent tarball

The proteus debugging CLI. Invoked by tritonagent for the per-port
lifecycle (create, apply blueprint, delete) and by operators at the
shell for direct kmod control + smoke tests.

## Layout (extracted at `/`)

```
/opt/triton/proteusadm/bin/proteusadm
/opt/triton/proteusadm/etc/version
```

No SMF service. proteusadm is not a daemon.

## Install

```bash
cd / && tar -xzf proteusadm-<stamp>.tar.gz
/opt/triton/proteusadm/bin/proteusadm --version
```

Or via tcadm (future): `tcadm agent install proteusadm`.

## Runtime requirements

- The proteus kmod must be loaded (`modinfo | grep proteus`). This
  ships in the PI (build `20260519T191333Z` and later); kmod
  versioning is out of scope for this agent.
- `/dev/proteus` must exist (the kmod creates it on attach via the
  `devlink.tab` rule baked into the PI).
- An underlay v6 address must be configured on the admin NIC for
  inter-CN proteus traffic; this is the operator's responsibility
  at CN-join time (today via `proteus/scripts/install_smf_kmod.sh`,
  future via a `tcadm net proteus-underlay set <addr>` verb).

## Build flow

```bash
STAMP=$(date -u +%Y%m%dT%H%M%SZ) bash agents/proteusadm/build.sh
tritoncloud-publish --channel edge agent \
    --name proteusadm \
    --stamp "$STAMP" \
    --tarball /tmp/proteusadm-$STAMP.tar.gz
```
