# Phase C — End-to-end verify transcript, 2026-05-29 on build00 (192.168.1.182)

## Bootstrap state recreated after VM reboot

- fdbserver pid 5757 on 192.168.1.182:4500
- Fresh single-node cluster `triton:phasec01@192.168.1.182:4500`
- `/etc/fdb/fdb.cluster` + `/etc/fdb/public_ip` + `/etc/tritond/config.toml` rewritten
- New tritond (commit `3329e1af`, built with `--features foundationdb`) at `/opt/tritond/bin/tritond` (63.6M)
- New mantad (manta-storage Phase A/B) at `/opt/mantad/bin/mantad` (87M), `--meta-plane=fdb`
- Cluster registered: `mantad-01` id `5e4e29a6-692a-4499-aa65-ee29909f156c`
- `storage.default_s3_cluster_id` = `5e4e29a6-692a-4499-aa65-ee29909f156c`

## Gotcha that ate ~1h: localhost:8080 conflict

A pre-existing Node.js process (pid 5149) was bound to `127.0.0.1:8080`. Tritond binds `*:8080` (wildcard). On illumos, the more-specific 127.0.0.1 binding wins routing for localhost traffic, so every `curl http://127.0.0.1:8080/...` got 404'd by the Node process before reaching tritond. Solution: hit the LAN IP `http://192.168.1.182:8080/...` directly.

## Happy path (PASS)

Silo create (auto-creates a default tenant BEFORE storage.default_s3_cluster_id was set):

```
POST /v1/silos {"name":"phase-c-silo"} →
  silo_id        = 988ae554-3508-44cc-afb8-a2300fbeaf13
  default_tenant = a938146f-d3e9-47a0-a02b-d86ea4b3105c
  default_tenant.storage_workspace_id = null   ← correct, no default at create time
  default_tenant.storage_cluster_id   = null   ← correct
```

New tenant under the silo (now WITH default cluster set):

```
POST /v1/silos/<silo>/tenants {"name":"acme"} →
  id                   = bd2cf24c-1146-4143-8ede-ae7a0599a0a7
  storage_workspace_id = bd2cf24c-1146-4143-8ede-ae7a0599a0a7   ← bound!
  storage_cluster_id   = 5e4e29a6-692a-4499-aa65-ee29909f156c   ← matches default
```

Cross-check on mantad:

```
GET /admin/v1/workspaces (mantad) →
  [{"name":"t-bd2cf24c114641438edeae7a0599a0a7",
    "description":"acme",
    "quota_bytes":107374182400,            ← 100 GiB default from C2
    "tenant_uuid":"bd2cf24c-1146-4143-8ede-ae7a0599a0a7"}]
```

Delete tenant:

```
DELETE /v1/silos/<silo>/tenants/bd2cf24c... → 204 No Content
GET    /v1/silos/<silo>/tenants/bd2cf24c... → 404 Not Found  ← tritond row gone
GET    /admin/v1/workspaces (mantad)       → []              ← workspace gone
```

The delete order (mantad archive, then tritond row drop) is enforced.

## Failure path: mantad down at tenant create (PASS — no orphan Tenant row)

```
$ pkill -x mantad
$ POST /v1/silos/<silo>/tenants {"name":"fail-test"}
  → HTTP/1.1 500 Internal Server Error  (mantad_error_to_http on connection-refused)
$ GET /v1/silos/<silo>/tenants
  → only the original "default" tenant; "fail-test" is NOT in the list
```

Contract holds: state-on-mantad-first, no Tenant row unless mantad acknowledged.

Caveat: the 503 path specifically gated on `cluster.status == Unreachable` (pre-flight) requires a prior health probe; with `status: "unknown"` (no probe yet) the RPC goes through and fails with 5xx. Both surface as failure → no row.

## What landed end-to-end on the live box

- C1: Tenant.storage_workspace_id + .storage_cluster_id columns
- C2: storage.default_s3_cluster_id + storage.default_workspace_quota_bytes settings  
- C3: create_silo_tenant mints workspace before committing Tenant row (idempotency keyed on tenant_uuid)
- C4: delete_silo_tenant archives workspace before dropping row
- C5: Action::WorkspaceListAcrossTenants in auth.rs (compile-time)
- C6: WorkspaceScope gate on bucket create/delete (compile-time, code-side)

## Closing C7 and C8 with this transcript as evidence
