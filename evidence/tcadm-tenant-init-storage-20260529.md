# `tcadm tenant init-storage` retrofit — live verify transcript

**Date:** 2026-05-29 23:32 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-725baf2a-9ed8dcd.tar.gz` (63 MiB)

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `monitor-reef` | `725baf2a` | `tcadm tenant init-storage` retrofit — Store trait `set_tenant_storage_binding`, mem+FDB impls, `init_silo_tenant_storage` handler, `POST /v1/silos/{silo_id}/tenants/{tenant_id}/init-storage` endpoint, tcadm subcommand. |
| `manta-storage` | `9ed8dcd` | unchanged from the IAM-fanout verify on 22:36 UTC. |

## Scenario

Reproduce a Phase 0 first-boot tenant created before any S3
cluster was registered, then rescue it via the retrofit:

1. Reset `storage.default_s3_cluster_id` to "(unset)".
2. Create a new tenant — it lands with `storage_workspace_id`
   and `storage_cluster_id` both `null`.
3. Re-set `storage.default_s3_cluster_id` to the registered
   `mantad-01` cluster.
4. Run `tcadm tenant init-storage` on the unbound tenant.
5. Cross-check that the tenant row now carries both binding
   columns AND that mantad has a matching workspace.
6. Verify the idempotency / no-rebind guard: a second
   init-storage on the now-bound tenant returns 409.

## Transcript (abridged — full session in
`tools/cw2u-bucket-verify.sh` style)

```
=== 1. unset storage.default_s3_cluster_id
storage.default_s3_cluster_id = (unset) (default)
saved; restart tritond to apply

=== 2. create an unbound tenant
{
  "id": "b757fe07-b84a-4b43-8971-a4e58096f02e",
  "name": "init-storage-verify",
  "silo_id": "988ae554-3508-44cc-afb8-a2300fbeaf13",
  ...
}

=== 3. confirm the tenant has no binding
(same body — `storage_workspace_id` and `storage_cluster_id`
absent from the JSON: serde skips `None`)

=== 4. set storage.default_s3_cluster_id back to mantad-01
storage.default_s3_cluster_id = 5e4e29a6-692a-4499-aa65-ee29909f156c

=== 5. tcadm tenant init-storage (success path)
{
  "id": "b757fe07-b84a-4b43-8971-a4e58096f02e",
  "name": "init-storage-verify",
  ...
  "storage_cluster_id": "5e4e29a6-692a-4499-aa65-ee29909f156c",
  "storage_workspace_id": "b757fe07-b84a-4b43-8971-a4e58096f02e"
}

=== 6. tenant show — confirm binding populated
(same shape: both columns now non-null)

=== 7. init-storage on already-bound tenant — expect 409 Conflict
Error: init tenant storage binding
Caused by:
    Error Response: status: 409 Conflict;
    ...
    message: "tenant b757fe07-b84a-4b43-8971-a4e58096f02e already
              has a storage binding; drop the existing binding
              before rebinding"

=== 8. cross-check on mantad: workspace exists
{
  "name": "t-b757fe07b84a4b438971a4e58096f02e",
  "created_at": "2026-05-29T23:32:37.257752376Z",
  "description": "init-storage-verify",
  "quota_bytes": 107374182400,
  "quota_objects": null,
  "usage_backfilled": true,
  "tenant_uuid": "b757fe07-b84a-4b43-8971-a4e58096f02e"
}
```

## Observations

* **Workspace name** matches the spec — `t-{tenant_uuid_simple}` —
  identical to the name `create_silo_tenant` would mint at create
  time. The retrofit and the original create path are wire-
  compatible.
* **Workspace `description`** is the tenant's display name
  (`init-storage-verify`). Matches the create path's behaviour.
* **`quota_bytes`** is the fleet default
  (`storage.default_workspace_quota_bytes` = 107374182400 = 100 GiB).
* **`storage_workspace_id` field on the tenant equals the
  tenant_id** — that's the deliberate identity used so
  `t-{tenant_id_simple}` is the workspace name; no separate
  workspace UUID is minted (mantad workspaces are name-keyed,
  not UUID-keyed).
* **409 message** mentions the explicit drop-then-rebind path
  the operator must follow, so an operator-error doesn't silently
  orphan a workspace.

## Out of scope / followups

* **Bootstrap-the-very-first-bound-tenant.** This run reproduced
  the "tenant created before cluster" scenario by *resetting*
  the default cluster. The actual Phase 0 first-boot path is
  the same one: bootstrap creates the default tenant before any
  cluster is registered, and the operator runs `init-storage`
  against that tenant once `tcadm storage cluster add` has
  landed. Adding a smoke-test for the literal first-boot
  ordering would need a fresh-FDB rerun on the box.
* **Drop-existing-binding** path. The 409 message tells the
  operator to "drop the existing binding before rebinding,"
  but there's no `tcadm tenant drop-storage` command today.
  When the v1 multi-cluster placement story lands, that
  command becomes necessary.
* **Audit chain coverage.** Each failure stage (`init_storage.client_for`,
  `init_storage.preflight`, `init_storage.mantad.create_workspace`,
  `init_storage.store.set_tenant_storage_binding`) records its
  own outcome; the live verify exercised only the happy-path
  success and the in-store Conflict. The other failure stages
  are unit-covered by the existing audit-record tests but not
  yet by a live failure-injection drill.
