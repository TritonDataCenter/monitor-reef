# S3 data-plane workspace gate — live verify

**Date:** 2026-05-30 (deployed 00:53 UTC, verified ~01:00 UTC).
**Test box:** `192.168.1.182` (build00, SmartOS GZ).
**Build:** manta-storage `eca7339` on `surface-s3`, mantad release
profile with `--features fdb`, stripped to 17.5 MB. Built on
build02 with `RUSTFLAGS="-L native=$HOME/lib"` (the foundationdb-sys
build script honours `FDB_CLIENT_LIB_PATH` but the env var didn't
propagate through `cargo`'s subprocess on that toolchain — RUSTFLAGS
was the direct path).
**mantad SigV4:** enabled. Persistent root creds at
`/opt/mantad/etc/root-creds` (mode 600) so future restarts don't
churn the bootstrap key. Without root creds, mantad runs in dev
"SigV4 auth DISABLED" mode and the gate is bypassable.

## Plan reference

`~/.claude/plans/now-that-we-know-harmonic-twilight.md` rev 2,
§Verification §Two-tenant SigV4 live verify on 192.168.1.182.
Approved by all nine reviewers in round 2.

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `manta-storage` | `eca7339` (surface-s3) | `CallerContext` enum + `head_bucket_for` gate across 37 S3 trait handlers + `list_buckets_in_workspace` MetaStore method + `create_bucket` workspace stamp |
| `monitor-reef` | unchanged | tritond / tcadm not touched in Phase 1 |

`mantad.prev` on the test box is the previous good build, ready for
one-step rollback (`mv /opt/mantad/bin/mantad{.prev,}`).

## Verify script

`tools/s3-data-plane-gate-verify.py` — runs from build02, drives
boto3 SigV4 calls direct to mantad at `http://192.168.1.182:7443`,
mints IAM users + access keys via mantad's admin API on
`http://127.0.0.1:7101` (over ssh to root@192.168.1.182, admin token
stays server-side).

The plan asked for "alice@t-A and alice@t-B" — two IAM users sharing
a name. Mantad's `IamUser` namespace is global (the FDB key is
`user.name` only — see `meta/fdb_store.rs::key_user`), so a duplicate-
name create 409s at the meta layer regardless of workspace. The
script names the two users `alice-a` and `alice-b`; the unit test
`gate_tests::cross_tenant_username_collision_is_blocked` (in
`s3_impl.rs`) proves the gate *would* block a same-name case if the
meta layer ever allowed it. The actual cross-tenant exposure
closed here is at the bucket / op level, which is what every probe
in this verify exercises.

## Results

```
== Provisioning tenants ==
  workspace A: t-0215688d2731401eb4a49cb43f8f8c82
  workspace B: t-fb72cd7b43e042e3b5985ae1b0b8b547

== Minting IAM users (one per workspace) ==
  alice-a@t-021568: ak_id=MKIAXZ7C...
  alice-b@t-fb72cd: ak_id=MKIAXXJ5...

== Each Alice creates one bucket via SigV4 ==
  alice-a created s3://phase1-a-f3a78ebc
  alice-b created s3://phase1-b-a9dfae6e

== Cross-tenant probes (must all NoSuchBucket) ==
  OK   alice-a head_bucket(bucket-b): NoSuchBucket
  OK   alice-a list_objects(bucket-b): NoSuchBucket
  OK   alice-a put_object(bucket-b/k): NoSuchBucket
  OK   alice-a get_object(bucket-b/k): NoSuchBucket
  OK   alice-a delete_object(bucket-b/k): NoSuchBucket
  OK   alice-a create_multipart(bucket-b/k): NoSuchBucket
  OK   alice-a copy_object src=bucket-b/k → bucket-a/k2: NoSuchBucket
  OK   alice-a delete_bucket(bucket-b): NoSuchBucket
  OK   alice-b head_bucket(bucket-a): NoSuchBucket
  OK   alice-b list_objects(bucket-a): NoSuchBucket
  OK   alice-b put_object(bucket-a/k): NoSuchBucket
  OK   alice-b delete_bucket(bucket-a): NoSuchBucket

== list_buckets visibility ==
  OK   alice-a sees only her bucket: {'phase1-a-f3a78ebc'}
  OK   alice-b sees only her bucket: {'phase1-b-a9dfae6e'}

== Same-workspace happy path ==
  OK   alice-a put+get(bucket-a/happy.txt)

== Cleanup ==

== Result: 0 failure(s) ==
```

**12/12 cross-tenant probes returned NoSuchBucket. 2/2 list_buckets
visibility checks passed. 1/1 same-workspace happy-path round-trip
worked. Cleanup successful.**

## What this closes

- Every S3 data-plane handler that touches a bucket now transits
  `head_bucket_for`, which 404s on workspace mismatch (matching the
  existing cross-owner convention).
- The compiler-driven fanout (deleting `caller_owner` to force
  every handler to be re-typed against `CallerContext`) caught all
  48 sites; the unit tests + the live multipart / copy / delete
  probes confirm no gate was missed.
- `list_buckets` is now scoped via the new
  `MetaStore::list_buckets_in_workspace` method on the IAM path,
  with root passthrough and Anonymous returning empty.
- New buckets created on the SigV4 data plane stamp the caller's
  workspace; root keeps the empty stamp for cluster-scoped admin
  work.

## Followups still open

- **Phase 2 (`monitor-reef-qi82`): per-workspace presigner
  credentials.** Today the cluster-root presigner key still
  authenticates presigned URLs as root → bypasses the gate. Not
  triggered by this verify (no presign in the script), but the
  bypass is real until Phase 2 lands.
- **Phase 3 (`monitor-reef-64zl`): `mantad-adm bucket
  repair-workspace` + aws-sdk-rust integration test harness.** This
  verify runs from build02 against a live cluster; the integration
  test version would let the gate be exercised in CI without a
  cluster.

## Operational notes

- mantad must be launched with both `MANTAD_ROOT_ACCESS_KEY_ID` and
  `MANTAD_ROOT_SECRET_ACCESS_KEY` set, otherwise SigV4 verification
  is disabled and any request can claim any access key id. The
  restart script `/tmp/restart-mantad.sh` on the test box generates
  these on first run, persists them to `/opt/mantad/etc/root-creds`
  (mode 600), and reuses on subsequent restarts. Worth bringing
  into `tools/phase-c-deploy.sh` as a followup.
- Admin port on mantad is **7101 (internal)**, not 7443 (public).
  7443 is the S3 + STS listener and rejects bearer-auth headers
  once SigV4 is enabled. mantad-adm has `--admin-url` defaulting to
  the public port; that works only because mantad-adm currently
  only hits status/placement/node endpoints, which are *also*
  served on 7443 for legacy reasons. mantad-adm shouldn't grow new
  workspace/user/access-key commands hitting 7443 — those go on 7101.
- The mantad release binary built with `cargo build --release -p
  mantad --features fdb` lands at 91 MB unstripped. `strip` cuts it
  to ~18 MB, which is what we ship to the test box. The
  phase-c-build.sh script doesn't strip today; worth adding for
  faster network transfers.
