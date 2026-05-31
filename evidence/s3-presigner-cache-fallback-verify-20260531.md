# Presigner cache eviction + cluster-root fallback live verify

**Date:** 2026-05-31
**Test box:** `192.168.1.182` (build00, SmartOS GZ)
**Tritond:** `nick-tritond-phase0 cc2ef6a2` (deployed)
**Mantad:** `surface-s3 8d50af4` (deployed)

## What this closes

Two followups from `evidence/s3-presigner-tritond-verify-20260531.md`:

1. **Cache eviction on `drop_silo_tenant_storage`** — unit-tested
   in `presigner_cache::evict_is_idempotent` but never exercised
   end-to-end. Binds a tenant, mints a presign (warming the
   cache), drops the storage binding, re-inits, mints again, and
   asserts the post-rebind `X-Amz-Credential` AKID differs from
   the pre-drop AKID.
2. **Cluster-root presigner fallback** — Phase 2 verified the
   per-workspace path; the Unscoped / fleet-admin fallback path
   was configured but unproven. Mints a presign as a fleet-admin
   (Unscoped scope) and asserts the URL AKID equals
   `cluster.presigner_access_key_id`.

## Verify script

`tools/s3-presigner-cache-fallback-verify.py`. Runs from anywhere
with SSH access to the test box; pulls the fleet-admin token out
of `/root/.config/tcadm/config.json` server-side, drives tcadm +
tritond curl over SSH.

## Setup-state findings (bd monitor-reef-n4w7)

Running the verify surfaced a real gap in the drop flow:

```
tcadm tenant drop-storage <silo> <tenant>
  -> Error Response: status: 409 Conflict
     mantad upstream error: 409: WorkspaceNotEmpty
     workspace t-<uuid> still has IAM user
     presigner-t-<uuid> (and possibly others);
     delete IAM users before archiving
```

The Phase 2 plan (§Phase 2 §3) assumed mantad's
`delete_workspace` would cascade the presigner-system user. In
practice mantad refuses any non-empty workspace and the
presigner-system user counts as "not empty," so
`drop_silo_tenant_storage` 409s **before** it reaches
`presigner_cache.evict`. Filed as bd monitor-reef-n4w7.

Fix options (tracked in n4w7):
- (a) mantad-side: auto-cascade `*-system` users on
  `delete_workspace`.
- (b) tritond-side: have `drop_silo_tenant_storage` explicitly
  delete `presigner-{workspace}` via the admin client before
  `delete_workspace`. Smaller blast radius.

## Workaround the verify uses

To exercise the cache-eviction code path while n4w7 is open,
the verify DELETEs `presigner-{workspace}` via the fleet-admin
forwarder (`DELETE /v1/storage/clusters/{id}/users/presigner-{ws}`)
**before** calling `drop-storage`. Once the cascade lands the
DELETE call can be removed and `drop-storage` alone will
exercise the eviction path.

The workaround is honest about what it tests: the post-DELETE
`drop-storage` still goes through `archive_tenant_workspace` →
`mantad.delete_workspace` → `presigner_cache.evict` on the
tritond side. The eviction call site itself is exercised; only
the **trigger** has been bypassed.

## Results

```
== Discovery ==
  cluster_id:    5e4e29a6-692a-4499-aa65-ee29909f156c
  root_akid:     AKIA97EF32849F791C1D
  fleet-admin token len: 208

== Item 3: cache eviction on drop_silo_tenant_storage ==
  tenant created: 56f215ef-9555-46a7-891f-183ebd227bca (cache-evict-9118)
  workspace:      t-56f215ef955546a7891f183ebd227bca
  operator logged in: alice-cache-9118
  pre-drop  AKID: MKIAQFXRQ6HFDTXRLW7W
  pre-drop workaround: deleted presigner-t-56f215ef955546... (n4w7 workaround)
  drop-storage:   Dropped storage binding for tenant 56f215ef-9555-46a7-891f-183ebd227bca
  init-storage:   Initialised storage binding for tenant 56f215ef-9555-46a7-891f-183ebd227bca
  post-rebind AKID: MKIA5EKGMBSW3UF3ZO4Z
  OK   pre-drop AKID != post-rebind AKID — cache evicted, fresh fetch on rebind

== Item 4: cluster-root presigner fallback (Unscoped) ==
  admin URL AKID: AKIA97EF32849F791C1D
  OK   admin URL signed with cluster-root AKID — Unscoped fallback path engaged

== Cleanup ==
  cleaned up 56f215ef

== Result: 0 failure(s) ==
```

## What this proves

- **Cache eviction**: the pre-drop AKID `MKIAQFXR…` and the
  post-rebind AKID `MKIA5EKG…` are distinct. With the workspace
  name unchanged (`t-{tenant_uuid_simple}` is a function of
  `tenant_id`, which doesn't change), a non-evicted cache would
  return the stale `MKIAQFXR…` on the second mint. The fresh
  AKID proves `presigner_cache.evict(cluster_id, workspace)` ran
  successfully on `drop_silo_tenant_storage`'s post-archive path
  and the next sign went through a real fetch.
- **Cluster-root fallback**: the admin URL's AKID
  (`AKIA97EF…`) equals the cluster row's
  `presigner_access_key_id`, NOT one of the per-workspace
  `MKIASA…` keys. Proves `mint_presigned_url`'s `workspace.is_none()`
  branch (the Unscoped fleet-admin path) reaches
  `cluster.presigner_access_key_id` / `..._secret` directly
  instead of falling through to the per-workspace cache.

## Followups still open

- **bd monitor-reef-n4w7** — the cascade gap that motivated the
  workaround. Until that lands, operators dropping a tenant's
  storage manually have to delete the presigner-system user
  first (or use the verify's same workaround over the admin
  forwarder).
- **Multi-process cache eviction** — current tritond is
  single-process, so `presigner_cache.evict` only invalidates
  one process's view. A multi-node tritond would need to fan
  out the eviction (or accept up to one TTL window of stale
  cache per peer). Not a correctness issue today.
- **Cluster-root fallback when `cluster.presigner_access_key_id`
  is unset** — `mint_presigned_url` 409s with
  `presigner_unconfigured` in that case. Already tested implicitly
  by `tools/setup-presigner.sh`'s pre-flight; no dedicated verify.
