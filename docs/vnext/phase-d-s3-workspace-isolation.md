# Phase D — S3 surface workspace isolation

**Status (2026-05-30):** Code-complete and live-verified on `192.168.1.182`. Branch `nick-tritond-phase0` carries 29 commits ahead of `origin/main`, still local-only per the standing "do not push" boundary.

This is the post-Phase-C work that turns the Tenant↔Workspace binding from "access-gate-only" into true per-tenant data-plane isolation. The original CHG-196 plan labelled this work as "Phase D" only after a relabel; the original Phase D (workspace-root SigV4) remains deferred indefinitely.

## What landed

| Slice | What it does | Tritond commit | Mantad commit | Evidence |
| --- | --- | --- | --- | --- |
| **c8ft — gate fanout** | `resolve_workspace_scope` 412-gate at all 18 storage forwarder sites (buckets, users, access keys, policies, presign) | `dd99d33a` | — | unit only |
| **cw2u buckets** | mantad bucket admin routes accept `?workspace=`, client+tritond thread it through | `c38293e4` | `138d00e` | [bucket verify](../../evidence/cw2u-bucket-verify-20260529.md) (21/21) |
| **cw2u IAM** | same `?workspace=` shape across users, access-keys, policies | `26206a1f` | `9ed8dcd` | [IAM verify](../../evidence/cw2u-iam-verify-20260529.md) (46/46) |
| **`Unscoped` Cedar gate** | replace `is_root` short-circuit with explicit `Action::WorkspaceListAcrossTenants` check | `555b6714`, `080fd906` | — | [capability verify](../../evidence/capability-grant-verify-20260530.md) (10/10) |
| **tenant operator user** | `tcadm tenant create-user` mints `Principal::Operator { tenant_id: Some, is_root: false }` accounts; Cedar permit rule `tenant-member-allows-storage-data-plane` | `53d33d2b`, `ded6a481` | — | [tenant-principal e2e](../../evidence/tenant-principal-e2e-20260530.md) (12/12) |
| **`tcadm tenant init-storage`** | retrofit a binding onto an unbound tenant (Phase 0 first-boot fix) | `725baf2a`, `725ad78f` | — | [init-storage verify](../../evidence/tcadm-tenant-init-storage-20260529.md) (8/8) |
| **`tcadm tenant drop-storage`** | counterpart to init-storage; clears binding + archives workspace | `fb6cebd6`, `c40bc7d2` | — | [drop-storage verify](../../evidence/tcadm-tenant-drop-storage-20260530.md) (8/8 happy-path) |

Commit history in order: `dd99d33a`, `c38293e4`, `40100e73`, `e0b7b637`, `26206a1f`, `f06d93b8`, `555b6714`, `725baf2a`, `725ad78f`, `53d33d2b`, `ded6a481`, `4983146e`, `080fd906`, `1fdf1414`, `fb6cebd6`, `c40bc7d2`. Manta-storage carries `138d00e` and `9ed8dcd` on `main`.

## What works end-to-end

Verified on the live deploy at `192.168.1.182`:

1. **Tenant operator authenticates via password login.** `tcadm tenant create-user` mints a User row; the user logs in via `POST /v1/auth/login` and receives an access token. The token carries `tenant_id` on the Principal.
2. **Storage forwarder is reachable.** Cedar policy `tenant-member-allows-storage-data-plane` permits the operator on every `storage_*` data-plane action (excluding cluster admin and presigner credential rotation).
3. **Workspace scope is derived from the principal.** `resolve_workspace_scope` reads `Principal::Operator.tenant_id`, looks up `Tenant.storage_workspace_id`, returns `Scope::Bound { workspace_name: "t-{tenant_id_simple}" }`.
4. **The mantad call carries `?workspace=`.** mantad-client's bucket / user / access-key / policy methods all accept `workspace: Option<&str>` and forward it as a query param.
5. **Mantad enforces per-workspace isolation.**
   - Create stamps the workspace field on the resource.
   - List filters to the workspace.
   - Get/delete on a mismatched workspace returns 404 (not 403 — no name-existence probe).
6. **Root operators still see cluster-wide.** Cedar's root-permit-all rule satisfies the `WorkspaceListAcrossTenants` action, so `Scope::Unscoped` fires, mantad-client passes `None`, mantad returns the whole cluster.
7. **Non-root operators with `StorageAdmin` capability also see cluster-wide.** The `storage-admin-allows-cross-tenant-view` rule permits the action on `principal.capabilities.contains("storage-admin")`, so the same Unscoped path fires.

## Tools added

| Tool | What | Reference |
| --- | --- | --- |
| `tcadm tenant create-user` | Mint a tenant-bound operator account (non-federated) | `cli/tcadm/src/commands.rs::tenant_create_user` |
| `tcadm tenant init-storage` | Retrofit a binding onto an unbound tenant | `cli/tcadm/src/commands.rs::tenant_init_storage` |
| `tcadm tenant drop-storage` | Clear a binding (archive workspace + clear columns) | `cli/tcadm/src/commands.rs::tenant_drop_storage` |
| `tcadm system user-grant <user> <cap>` | Grant a capability (e.g. `storage-admin`); existed pre-Phase-D | `cli/tcadm/src/main.rs` |
| `tools/cw2u-bucket-verify.sh` | Wire-shape verify against mantad; covers all 15 cw2u routes + tritond root smoke | `tools/cw2u-bucket-verify.sh` |

## Followups (work that survived Phase D)

1. **Empty-workspace check on mantad.** The [drop-storage verify](../../evidence/tcadm-tenant-drop-storage-20260530.md) caught that `mantad.delete_workspace` does NOT 409 when a workspace still has buckets — at least on the `9ed8dcd` build. The mantad client docstring claims it should. Either the check isn't wired or the doc is stale. Worth filing on manta-storage.
2. ~~**Consolidate workspace-archive paths.** Tenant-delete (`delete_silo_tenant`) and drop-storage both archive a mantad workspace. Refactor so tenant-delete calls drop-storage internally when a binding exists; reduces drift between the two flows.~~ **Done (`60971aaa`).** Shared `archive_tenant_workspace` helper in `tenants.rs` does pre-flight + 404-tolerant delete; both handlers wrap it with their own audit. Net -27 lines. Two behavior alignments fell out: tenant-delete now pre-flights cluster health, drop-storage now tolerates 404 on already-archived workspaces.
3. **Federated user end-to-end.** Tenant-principal verify used password-auth via `tcadm tenant create-user`. JIT-on-OIDC-login lands the same `Principal::Operator { tenant_id: Some, is_root: false }` shape, so the gate should fire identically. Live verify against an IdP-backed user is its own slice once an external IdP is in place.
4. **Push the branch.** `nick-tritond-phase0` is local-only with 29 commits. Coordinate with the workspace's "do not push" boundary before publishing.
5. **Workspace-root SigV4 (original CHG-196 Phase D).** Still deferred. The S3-IAM-direct admin path that would let workspace-root credentials hit mantad without round-tripping through tritond. v1 keeps tritond as the bearer holder.

## Cross-references

- [`evidence/cw2u-bucket-verify-20260529.md`](../../evidence/cw2u-bucket-verify-20260529.md) — bucket-only verify on the deployed cluster.
- [`evidence/cw2u-iam-verify-20260529.md`](../../evidence/cw2u-iam-verify-20260529.md) — IAM fanout verify.
- [`evidence/tenant-principal-e2e-20260530.md`](../../evidence/tenant-principal-e2e-20260530.md) — gold-standard end-to-end through a tenant principal.
- [`evidence/capability-grant-verify-20260530.md`](../../evidence/capability-grant-verify-20260530.md) — capability flip from Bound to Unscoped.
- [`evidence/tcadm-tenant-init-storage-20260529.md`](../../evidence/tcadm-tenant-init-storage-20260529.md) — init-storage retrofit.
- [`evidence/tcadm-tenant-drop-storage-20260530.md`](../../evidence/tcadm-tenant-drop-storage-20260530.md) — drop-storage counterpart.
- [`evidence/phase-c-verify-20260529.md`](../../evidence/phase-c-verify-20260529.md) — the prior Phase C verify this work built on.
- [`tools/cw2u-bucket-verify.sh`](../../tools/cw2u-bucket-verify.sh) — the live wire-shape verify script.
- [`docs/ops/phase-c-deploy.md`](../ops/phase-c-deploy.md) — deploy mechanics (also applies to Phase D bundles).
