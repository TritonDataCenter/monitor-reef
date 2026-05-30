# `tcadm tenant drop-storage` — live verify transcript

**Date:** 2026-05-30 17:07 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-fb6cebd6-9ed8dcd.tar.gz`

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `monitor-reef` | `fb6cebd6` | `tcadm tenant drop-storage` — Store trait `clear_tenant_storage_binding`, mem+FDB impls, `drop_silo_tenant_storage` handler, `DELETE /v1/silos/{silo_id}/tenants/{tenant_id}/storage`, tcadm subcommand. |
| `manta-storage` | `9ed8dcd` | unchanged. |

## Scenario

Exercise the counterpart to init-storage end-to-end:

1. Create a tenant + init-storage → binding established, workspace minted.
2. Drop-storage on the empty binding → 200, binding cleared, workspace
   archived.
3. Re-drop the now-unbound tenant → 412 `TenantStorageUnbound`
   (distinct-outcome idempotency, not silent success).
4. Re-init the same tenant + populate with a bucket → drop behaviour
   when mantad refuses.

## Results

```
=== 1. create tenant + init-storage
TENANT_ID=b9cecf3c-83e9-4034-bc14-bc4459f7caaa
Initialised storage binding for tenant b9cecf3c-83e9-4034-bc14-bc4459f7caaa
  workspace: t-b9cecf3c83e94034bc14bc4459f7caaa
  cluster:   5e4e29a6-692a-4499-aa65-ee29909f156c

=== 2. confirm bound
"storage_cluster_id": "5e4e29a6-692a-4499-aa65-ee29909f156c"
"storage_workspace_id": "b9cecf3c-83e9-4034-bc14-bc4459f7caaa"

=== 3. mantad workspace present
{"name":"t-b9cecf3c...","tenant_uuid":"b9cecf3c-...","description":"drop-verify",...}
HTTP 200

=== 4. drop-storage on empty workspace
Dropped storage binding for tenant b9cecf3c...
  (workspace archived on mantad; tenant row is now unbound)

=== 5. tenant show: binding cleared
(storage_workspace_id + storage_cluster_id absent from JSON — both
 are None now; serde-skips on Option<Uuid>)

=== 6. mantad: workspace gone
{"code":"NotFound","message":"workspace t-b9cecf3c..."}
HTTP 404

=== 7. drop-storage AGAIN — expect 412 TenantStorageUnbound
Error: drop tenant storage binding
Caused by:
  status: 412 Precondition Failed; ...
  message: "tenant b9cecf3c... has no storage binding to drop"

=== 8. re-init + populate workspace with a bucket
Initialised storage binding ...
Created bucket drop-bkt-blocker stamped with workspace=t-b9cecf3c...

=== 9. drop-storage with non-empty workspace
Dropped storage binding for tenant b9cecf3c-83e9-4034-bc14-bc4459f7caaa
  (workspace archived on mantad; tenant row is now unbound)
```

**Findings:**

* **Happy-path drop is clean.** The empty workspace gets archived
  on mantad and both binding columns clear on the tenant row in
  one round-trip.
* **Double-drop returns 412 `TenantStorageUnbound`, not silent
  success.** The store-level Conflict surfaces as the expected
  precondition-failed shape so an operator's "did I run this
  twice?" check has a non-ambiguous answer.

* **Mantad observation (not a drop-storage bug):** Step 9 was
  expected to surface 409 from mantad (workspace non-empty),
  per the doc comment on `mantad_client::delete_workspace`:
  *"Mantad enforces 'empty workspace' semantics; a non-empty
  workspace produces an upstream 409."* In the live run,
  mantad accepted the delete despite the bucket still existing,
  and tritond cleared the binding successfully. The orphaned
  bucket was still deletable afterwards with the (now-vanished)
  workspace name as the query param. Two reads of this:

  - The empty-workspace check on this particular mantad build
    (`9ed8dcd`) may not be wired (or wired only against the
    bucket-count via a different path). Worth filing as a
    manta-storage follow-up.
  - Tritond's drop-storage contract is *upstream-faithful*: it
    forwards whatever mantad says. If mantad enforces the check
    later, drop-storage will surface the 409 unchanged with no
    code change on the tritond side.

## What this closes

* The init-storage 409 message ("drop the existing binding
  before rebinding") now has a working CLI. Operators can swap
  tenants between clusters: drop → init-storage against a
  different `storage.default_s3_cluster_id`.
* Idempotency semantics are explicit (412 on a second drop) so
  retries are unambiguous.

## Followups

* **manta-storage: confirm empty-workspace check.** Either the
  enforcement isn't on the delete-workspace path, or this
  build is missing it. Document the intent precisely and
  either restore the check or relax the doc to match.
* **Tenant delete cascade.** Today the tenant delete path runs
  its own mantad-archive call (per phase-c commits). With
  drop-storage in place we now have *two* archive paths
  (drop-storage and tenant-delete). A follow-up could
  consolidate: tenant-delete calls drop-storage internally
  when a binding exists, then drops the tenant row. Reduces
  drift between the two flows.
