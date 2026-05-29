# cw2u (Phase D, bucket-only) — live verify transcript

**Date:** 2026-05-29 21:06–21:08 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Build host:** `build02` (illumos)
**Bundle:** `phase-c-bundle-c38293e4-138d00e.tar.gz` (60 MiB)
**Build SHA256:** `2bb74f04d1d911963598d6f8c241705d163449e0ae1ade5c626ca09548ef00c6`

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `manta-storage` | `138d00e` | `?workspace=` query param on bucket admin routes (create/list/get/delete) + workspace field on `BucketDto` |
| `monitor-reef` | `c38293e4` | tritond bucket forwarders thread `scope.workspace_name()` through to mantad-client |

Two prior commits land underneath these on the tritond branch:
- `d556e3dc` — phase-c verify transcript from 2026-05-29
- `dd99d33a` — c8ft gate fanout across all 16 storage-forwarder handlers

## Deploy

The bundle was produced on `build02` with:

```sh
PATH=$HOME/.rustup/toolchains/1.92-x86_64-unknown-illumos/bin:$PATH \
LIBRARY_PATH=$HOME/lib \
sh tools/phase-c-build.sh ~/Triton-S3-vnext/monitor-reef ~/Triton-S3-vnext/manta-storage \
                          ~/Triton-S3-vnext/phase-c-bundles
```

Two-leg copy (build02 → laptop → 192.168.1.182), then `phase-c-deploy.sh`
on the box. The deploy output confirmed:

```
==> previous tritond backed up to /opt/tritond/bin/tritond.prev
==> unpacking /var/tmp/phase-c-bundle-c38293e4-138d00e.tar.gz
==> reusing existing admin token at /opt/mantad/etc/admin-token
==> launching mantad (--meta-plane=fdb, admin token gated)
==> mantad pid=6790
==> launching tritond (config=/etc/tritond/config.toml)
==> tritond pid=6792
==> both daemons listening
```

## Verify results

`sh /var/tmp/cw2u-bucket-verify.sh` against the live mantad on
`http://127.0.0.1:7101` with the existing admin token at
`/opt/mantad/etc/admin-token`. Two workspaces minted (A, B) with
real hex tenant UUIDs `c12fb1ee-...-a1` / `c12fb1ee-...-b2`.

**All 18 cases PASS.**

```
PASS: workspace A created (t-c12fb1ee0000000000000000000000a1)
PASS: workspace B created (t-c12fb1ee0000000000000000000000b2)
PASS: create_bucket?workspace=A stamps workspace field
PASS: create_bucket?workspace=B stamps workspace field
PASS: create_bucket without workspace leaves field empty
PASS: create_bucket?workspace=BOGUS returns 404
PASS: list_buckets (no workspace) returns all three
PASS: list_buckets?workspace=A returns only A's bucket
PASS: list_buckets?workspace=B returns only B's bucket
PASS: list_buckets?workspace=BOGUS returns empty array
PASS: get_bucket A?workspace=A returns 200
PASS: get_bucket A?workspace=B returns 404 (cross-tenant probe blocked)
PASS: get_bucket A (no workspace, root view) returns 200
PASS: delete A?workspace=B returns 404 (cross-tenant delete blocked)
PASS: bucket A still exists after the failed cross-tenant delete
PASS: delete A?workspace=A returns 204
PASS: get A after delete returns 404
PASS: cleanup: B + root buckets deleted
PASS: cleanup: verify workspaces deleted
```

### Representative wire trace

**Create stamps the workspace field on the response:**

```
POST /admin/v1/buckets?workspace=t-c12fb1ee0000000000000000000000a1
body: {"name":"cw2u-verify-a-6828","owner":"root"}

HTTP 200
{"name":"cw2u-verify-a-6828","owner":"root",
 "created_at":"2026-05-29T21:08:04.177762858Z",
 "workspace":"t-c12fb1ee0000000000000000000000a1",
 "object_count":0,"total_bytes":0}
```

**Create against unknown workspace → 404:**

```
POST /admin/v1/buckets?workspace=t-doesnotexist00000000000000000000
HTTP 404
{"code":"NotFound","message":"workspace t-doesnotexist00000000000000000000"}
```

**Cross-tenant probe blocked (get-then-mismatch reports 404 not 403):**

```
GET /admin/v1/buckets/cw2u-verify-a-6828?workspace=t-c12fb1ee0000000000000000000000b2
HTTP 404
{"code":"NotFound","message":"bucket cw2u-verify-a-6828"}
```

Bucket A continued to exist (the mismatched delete in the next
step also returned 404 and left the row intact; a subsequent
correctly-scoped DELETE returned 204).

## Tritond root smoke (21:31 UTC)

A follow-up run with the smoke leg enabled exercised the rebuilt
tritond binary directly. Token + cluster id were derived on-box
from `/root/.config/tcadm/config.json` + `tcadm storage cluster
list --json` so no credentials crossed the ssh boundary:

```
==> tritond root smoke: http://192.168.1.182:8080 cluster=5e4e29a6-692a-4499-aa65-ee29909f156c
PASS: tritond root create bucket returns 201
PASS: tritond root create lands in mantad with empty workspace (no scope leak)
PASS: tritond root delete bucket
```

The second case is load-bearing: it proves
`WorkspaceScope::Unscoped` (root operator) → `workspace_name()
== None` → mantad-client sends no `?workspace=` → mantad stores
the bucket with `workspace=""`, exactly as the admin-direct
path. No accidental scope leak through the rebuilt tritond.

**21/21 cases pass.**

## Out of scope / followups

* **Tenant-principal end-to-end** — the gate triggers off
  `Principal::Operator { tenant_id: Some(...) }`. Minting a tenant
  API key + authenticating the curl call as that principal requires
  infrastructure not yet wired into the deploy. Once that lands,
  the same verify script can be extended with a "tenant A creates
  bucket → sees only its own" leg through tritond directly.
* **Users / access-keys / policies / presign** — Phase D's other
  routes haven't yet picked up the workspace param on the wire.
  Pattern is mechanical now that buckets prove the design.

## Bugs caught during verify

The first run of the verify script failed on case 1 with two
script bugs that did NOT reflect mantad behaviour:

1. Tenant UUIDs contained the literal `w` character (taken from
   the `cw2u-` prefix) — not valid hex; mantad's serde 422'd it.
   Fixed by using real hex UUIDs (`c12fb1ee-...`).
2. `Authorization: Bearer $TOKEN` was being word-split because the
   header was stored in a string variable and re-expanded —
   curl interpreted the token half as a host argument. Fixed by
   inlining the headers as `-H` args inside a function.

Both fixes are in the committed verify script.
