# Capability-grant cross-tenant view — live verify

**Date:** 2026-05-30 16:46 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-080fd906-9ed8dcd.tar.gz`

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `monitor-reef` | `080fd906` | `principal.capabilities` surfaced to Cedar; new `storage-admin-allows-cross-tenant-view` permit rule |
| `manta-storage` | `9ed8dcd` | unchanged |

## Scenario

Prove that granting a tenant-bound operator the existing
`Capability::StorageAdmin` flips their forwarder scope from
`Bound { workspace_name }` to `Unscoped` *without* elevating them
to root.

The end-to-end chain:

1. `tcadm tenant create-user` mints a tenant-bound user.
2. `tcadm system user-grant <user> storage-admin` flips the
   `User.capabilities` set to include `StorageAdmin`.
3. At next login, the issued access token's Principal carries
   `capabilities = {StorageAdmin}`.
4. Cedar's principal entity carries `capabilities: {"storage-admin"}`
   (kebab-case serde of the enum).
5. `resolve_workspace_scope`'s `auth.authorize(principal,
   WorkspaceListAcrossTenants)` succeeds via the new
   `storage-admin-allows-cross-tenant-view` rule → `Unscoped`.
6. `mantad_client` is called without `?workspace=` → cluster-wide
   view.

## Results

```
=== 1. mint tenant-A user
USER_ID=01ee69a5-673a-42e2-a5d0-f72e3e39aac1

=== 2. tenant creates its own bucket (Bound)
{"name":"cap-bkt-tenant",...}
HTTP 201

=== 3. root: admin-direct unscoped bucket on mantad
{"name":"cap-bkt-root","workspace":"",...}

=== 4. BEFORE grant: tenant lists buckets — Bound, sees only own
[{"name":"cap-bkt-tenant", ...}]

=== 5. ROOT grants storage-admin to the tenant user
Granted storage-admin to 01ee69a5-673a-42e2-a5d0-f72e3e39aac1.
User now carries 1 capability.
  - StorageAdmin

=== 6. tenant relogin (new access token carries StorageAdmin)

=== 7. AFTER grant: same call — Unscoped, sees ALL three buckets
[{"name":"cap-bkt-root",  ...},
 {"name":"cap-bkt-tenant",...},
 {"name":"diag-7934",     ...}]

=== 8. ROOT revokes storage-admin
Revoked storage-admin from 01ee69a5-673a-42e2-a5d0-f72e3e39aac1.

=== 9. tenant relogin

=== 10. AFTER revoke: back to Bound — only own
[{"name":"cap-bkt-tenant", ...}]
```

The pivotal contrast is between case 4 and case 7: *same user*,
*same URL*, *same access token shape* — only the capability set
on their User row differs. With `StorageAdmin` they see the
cluster; without it they see only their workspace.

## What this closes

* The 555b6714 + 080fd906 pair: `WorkspaceListAcrossTenants` is no
  longer "root-only via root-permit-all" in practice. Any user
  with the `StorageAdmin` capability is permitted, and the
  capability has a working grant / revoke UX via the existing
  `tcadm system user-grant` / `user-revoke` flow.
* Operators can run a tenant-bound forensics/audit user with
  cross-tenant inventory access *without* handing them root.
  Useful for support / compliance / incident-response roles.

## Followup

* **Federated users.** This verify used a password-auth tenant
  user via `tcadm tenant create-user`. A federated user from
  the IdP path lands with the same shape but their capability
  set defaults to empty; granting them StorageAdmin should work
  identically. Live verify against an IdP-backed user is a
  separate slice once an external IdP is in place.
* **Tenant users granting themselves capabilities.** The
  grant endpoint requires `Capability::SystemOperate` on the
  caller, so a tenant user without that cap can't self-promote.
  That's the right shape; root has it, fleet-admins should
  ultimately have it, and tenant users by default don't.
