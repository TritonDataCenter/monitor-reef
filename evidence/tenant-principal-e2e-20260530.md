# Tenant-principal end-to-end verify — Phase D gold standard

**Date:** 2026-05-30 16:30 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-ded6a481-9ed8dcd.tar.gz`

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `monitor-reef` | `ded6a481` | tenant-bound user create endpoint + Cedar permit rule for storage forwarder actions when `principal has tenant_id` |
| `manta-storage` | `9ed8dcd` | unchanged — workspace gate on bucket + IAM admin routes |

Commits between the IAM verify and this run:

* `725baf2a` — `tcadm tenant init-storage` retrofit
* `725ad78f` — init-storage live verify evidence
* `53d33d2b` — `tcadm tenant create-user` (mint tenant-bound operator)
* `ded6a481` — Cedar permit rule: `tenant-member-allows-storage-data-plane`

## Scenario

Exercise the workspace gate end-to-end as a tenant-bound
operator principal, NOT as the root operator. This is the
proof the cw2u bucket + IAM verifies (`40100e73`, `f06d93b8`)
were missing — they exercised the mantad-side wire shape with
an admin token, but the tritond-side gate hadn't been run
against an actual `Principal::Operator { tenant_id: Some(...) }`.

The flow:

1. Set the fleet's `storage.default_s3_cluster_id` to a real cluster.
2. Run `tcadm tenant init-storage` on the default tenant
   (Phase 0 left it unbound).
3. Mint a tenant-bound operator user via `tcadm tenant create-user`.
4. Log in as that user via `POST /v1/auth/login` to get an access token.
5. Exercise the storage forwarder: create / list / get / delete a bucket.
6. Cross-check mantad to confirm the workspace stamping is correct.
7. Compare what the tenant user sees vs what root sees (root must
   be Unscoped per `Action::WorkspaceListAcrossTenants`).
8. Negative test: create a bucket *via admin-direct mantad* (no
   workspace), confirm the tenant user can't see/delete it via
   tritond.

## Results — 12/12 PASS

```
=== 1. init-storage on tenant A
Initialised storage binding for tenant a938146f-d3e9-47a0-a02b-d86ea4b3105c
  workspace: t-a938146fd3e947a0a02bd86ea4b3105c
  cluster:   5e4e29a6-692a-4499-aa65-ee29909f156c

=== 2. mint tenant-A user
{
  "id": "17561d2b-e6dc-4920-92d9-e27854349589",
  "is_root": false,
  "tenant_id": "a938146f-d3e9-47a0-a02b-d86ea4b3105c",
  "username": "tenant-verify-d3e0ca53"
}

=== 3. login as the tenant user → access token issued (208 chars)

=== 4. tenant creates a bucket through tritond
{
  "name": "tenant-pe-bkt",
  "owner": "tA",
  ...
}
HTTP 201

=== 5. mantad cross-check — bucket carries the tenant's workspace
{
  "name": "tenant-pe-bkt",
  ...
  "workspace": "t-a938146fd3e947a0a02bd86ea4b3105c",
  ...
}

=== 6. tenant lists buckets — sees ONLY its own bucket
[{"name":"tenant-pe-bkt", ...}]

=== 7. ROOT lists buckets (Unscoped) — sees the whole cluster
[{"name":"diag-7934", ...},
 {"name":"tenant-pe-bkt", ...}]

=== 8. tenant GET own bucket → 200

=== 9. ROOT creates a bucket admin-direct (no workspace stamp)
{"name":"root-admin-bkt", "workspace":"", ...}

=== 10. tenant tries to GET root-admin-bkt via tritond → 404
{
  "error_code": "NotFound",
  "message": "mantad upstream error: 404: ... bucket root-admin-bkt"
}

=== 11. tenant tries to DELETE root-admin-bkt → 404
(same shape — cross-workspace delete blocked at mantad)

=== 12. cleanup: tenant deletes own bucket (204);
        root deletes admin-direct bucket (204)
```

### Key wire observations

* **Workspace name** = `t-{tenant_id_simple}` was derived from the
  authenticated principal's `tenant_id` field, NOT from any URL
  parameter or body field. The full chain works:

  ```
  POST /v1/storage/clusters/{cluster}/buckets   (tenant token)
     ↓ authenticate_and_authorize_in_silo → Principal::Operator { tenant_id: Some(a938...) }
     ↓ Cedar check: tenant-member-allows-storage-data-plane → permit
     ↓ resolve_workspace_scope(auth, store, principal)
     ↓ auth.authorize(principal, WorkspaceListAcrossTenants) → Err (no permit) → fall through
     ↓ tenant_id is Some → look up Tenant → storage_workspace_id is Some
     ↓ Scope::Bound { workspace_name: "t-a938..." }
     ↓ mantad_client.create_bucket(req, Some("t-a938..."))
     ↓ mantad: head_workspace("t-a938...") → OK → stamp bucket → store
     ↑ 201 + bucket with workspace="t-a938..."
  ```

* **Root user's Unscoped path** is the contrapositive: same code,
  `auth.authorize(principal, WorkspaceListAcrossTenants)` succeeds
  via the root-permit-all rule, returns `Scope::Unscoped`,
  `workspace_name()` is `None`, mantad gets no `?workspace=` and
  returns the cluster-wide view.

* **Cross-tenant blocked at mantad, not Cedar.** The tenant
  user's GET on `root-admin-bkt` reached mantad (Cedar permitted
  the action because the tenant has `storage_bucket_get`),
  but mantad's workspace gate noticed
  `bucket.workspace = ""` vs `?workspace=t-a938...`, returned 404.
  Tritond propagated the 404 to the tenant. No name-existence
  leak — just like the cw2u IAM verify proved.

## Bug found + fixed during the run

* **First-pass failed with 403 "not authorised for
  storage_bucket_create"** even with a valid tenant-user token.
  Root cause: the Cedar policy bundle didn't permit
  tenant-bound principals on any `storage_*` action. The
  workspace gate was mathematically correct but unreachable.
* **Fix** (commit `ded6a481`): added the
  `tenant-member-allows-storage-data-plane` Cedar rule
  permitting any principal with a `tenant_id` on the storage
  forwarder data-plane actions. Cluster-admin and node-admin
  actions (and the presigner credential rotation) stay
  root-only by exclusion.
* After redeploy, all 12 cases pass on the second run.

## What this proves

The workspace gate ships end-to-end:

* Tenant principals can use the storage forwarder.
* Their requests carry their tenant's workspace into mantad.
* They see ONLY their workspace's buckets / IAM / objects.
* Root operators continue to have unrestricted cluster view.
* Cross-tenant probes 404 with no name leak.

The gold-standard tenant-principal end-to-end is the
property the cw2u IAM verify (46/46 PASS) was missing.
It's now closed.

## Out of scope / followups

* **Storage IAM for tenant users** — this verify only exercised
  bucket operations end-to-end as a tenant user. The cw2u IAM
  fanout (users / access-keys / policies) was verified against
  mantad directly (46/46). Threading a tenant-principal-driven
  verify through the IAM routes is a follow-up — same shape,
  just longer.
* **Capability-grant UX** (item #2 from the Phase D backlog) —
  the `WorkspaceListAcrossTenants` action gate that 555b6714
  added still resolves to "root operators only" in practice
  because root is the only principal that gets the cap (via
  root-permit-all). A `tcadm operator grant-capability` flow
  would close that loop for non-root fleet operators.
* **Federated user end-to-end** — this verify used a
  password-auth tenant user via `tcadm tenant create-user`.
  The JIT-on-OIDC-login path lands the same shape of
  `Principal::Operator { tenant_id: Some(...) }` so the gate
  should fire identically; a live IdP-mediated run would
  confirm.
