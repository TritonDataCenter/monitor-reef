# Per-workspace presigner credentials — live verify (Phase 2)

**Date:** 2026-05-31 02:00 UTC.
**Test box:** `192.168.1.182` (build00, SmartOS GZ).
**mantad:** `surface-s3 1b71685` (manta-storage), release profile,
`--features fdb`, stripped to 17.8 MB. Built on build02 with
`RUSTFLAGS="-L native=$HOME/lib"` and Rust 1.92 toolchain.

## Plan reference

`~/.claude/plans/now-that-we-know-harmonic-twilight.md` rev 2,
§Verification §Phase 2 verify items 9–11.

## What landed

| Repo | Commit | Change |
| --- | --- | --- |
| `manta-storage` | `92869da` | mantad admin route `POST /admin/v1/workspaces/{name}/presigner` + `mantad-client::provision_workspace_presigner` |
| `manta-storage` | `1b71685` | `is_workspace_presigner` ACL recognition: `presigner-{ws}` resolves as a workspace-system role in `acl_allows_read/write` |
| `monitor-reef` | `1e2edf84` | `presigner_cache` module + `ApiContext::presigner_cache` + `mint_presigned_url` `Option<&str> workspace` arg + `drop_silo_tenant_storage` cache eviction + verify script |

mantad.phase1 saved as `/opt/mantad/bin/mantad.phase1` on the test
box for one-step rollback to Phase 1. `mantad.prev` is the
pre-Phase-1 build for a deeper rollback.

## Verify script

`tools/s3-presigner-workspace-verify.py` — runs from build02 with the
boto3 venv at `/tmp/boto-verify-venv`. Drives mantad's admin API via
ssh+curl (admin token stays server-side) and SigV4-signs presigned
URLs locally with the per-workspace creds returned from the new
admin route.

## Results

```
== Provisioning tenants + IAM users + buckets ==
  workspace A: t-2e132055077842ffbe776d266bc7a529
  workspace B: t-6e18aa4ebb40416ca1384bec8318c82d
  alice_a created s3://phase2-a-4a06d5d7
  alice_b created s3://phase2-b-3d071295

== Provisioning per-workspace presigner credentials ==
  presigner@t-2e1320: user=presigner-t-2e132055077842ffbe776d266bc7a529 ws=...
  presigner@t-6e18aa: user=presigner-t-6e18aa4ebb40416ca1384bec8318c82d ws=...
  OK   idempotent: 2nd call returns the same AK + secret

== Presigned PUT signed with per-workspace key — in-workspace ==
  OK   PUT phase2-a-…/phase2/probe.txt via per-ws presign → 200

== Presigned PUT cross-workspace — must NoSuchBucket ==
  OK   PUT bucket-b via t-A presigner blocked (status=404)

== Presigned PUT cross-workspace (B→A) — must NoSuchBucket ==
  OK   PUT bucket-a via t-B presigner blocked (status=404)

== Presigned GET — in-workspace round-trip ==
  OK   GET phase2-a-…/phase2/probe.txt → 200 + correct body

== Result: 0 failure(s) ==
```

## What this closes

- Mantad now mints (or re-fetches, idempotently) a per-workspace
  IAM-style presigner credential under the system user name
  `presigner-{workspace}`, workspace-stamped.
- A presigned URL signed with that credential resolves on mantad
  as `Iam { workspace = t-X, owner = "presigner-t-X" }`.
- The Phase 1 workspace gate (`head_bucket_for`) 404s any
  presigned-URL rewrite that targets a bucket in another workspace
  — even when the SigV4 signature itself is valid for the chosen
  bucket name.
- The new `is_workspace_presigner` ACL helper recognises the
  presigner identity as workspace-admin within its own workspace
  ONLY. A presigner from another workspace, or a non-system user
  named `presigner-bob`, is denied. Six gate unit tests + one
  ACL-specific unit test cover the matrix.
- Tritond caches per-workspace presigner creds in-process with a
  5-min TTL. `drop_silo_tenant_storage` evicts the cache entry
  after mantad's workspace delete cascades the system user's row.

## Followups still open

- **Tritond-mediated presign live verify.** This evidence captures
  the mantad side end-to-end. Tritond's `mint_presigned_url`
  rewrite is unit-tested (cache + scope threading) but the live
  verify ran the SigV4 signing locally in the script for
  isolation. A future live verify can route through tritond's
  `/v1/storage/clusters/{id}/presign-{get,put}` endpoint with an
  authenticated tenant principal to prove the full integration.
- **`monitor-reef-64zl` (Phase 3): `mantad-adm bucket
  repair-workspace` + SDK integration test harness.** Still open.

## Operational notes

- The Phase 1 deploy notes still apply (strip the binary, set
  `MANTAD_ROOT_ACCESS_KEY_ID`+`MANTAD_ROOT_SECRET_ACCESS_KEY`).
- Cleanup at the end of the verify can leave a few workspaces
  behind if a prior run partial-failed; the empty-workspace-gate
  on mantad will refuse to drop a workspace with buckets in it,
  which is by design. Sweep manually with `mantad-adm`-style
  curls if it matters between runs.
