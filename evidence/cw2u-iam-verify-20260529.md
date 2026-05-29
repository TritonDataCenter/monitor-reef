# cw2u Phase D (IAM fanout) — live verify transcript

**Date:** 2026-05-29 22:36 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-26206a1f-9ed8dcd.tar.gz` (60 MiB)
**Build SHA256:** `b7798ac8bbb377f833dd61438291310d1110cf2e31bfee883d9494498b940ac4`

## Code under test

This deploy extends the bucket-only cw2u (commits `c38293e4` +
`138d00e` verified 21:08 + 21:31 UTC) to the three remaining
data-plane resource families on mantad.

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `manta-storage` | `9ed8dcd` | `?workspace=` query param on every IAM admin route (users / access keys / policies). `WorkspaceQuery` reused across all resource types. `UserDto` + `AccessKeyDto` grow a `workspace` field. |
| `monitor-reef` | `26206a1f` | tritond IAM forwarders (`users.rs`, `access_keys.rs`, `policies.rs`) rename `_scope` → `scope` and thread `scope.workspace_name()` into 11 mantad-client calls. |

## Verify results

`sh /var/tmp/cw2u-bucket-verify.sh` against the live deployed
mantad on `http://127.0.0.1:7101` with the existing admin token.
The verify script now covers all 15 cw2u routes plus the
tritond-root smoke leg.

**46 cases PASS.**

```
PASS: workspace A created (t-c12fb1ee0000000000000000000000a1)
PASS: workspace B created (t-c12fb1ee0000000000000000000000b2)
# --- bucket cases (18) ---
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
# --- IAM users (8) ---
PASS: create_user?workspace=A stamps workspace field
PASS: create_user?workspace=B stamps workspace field
PASS: create_user without workspace leaves field empty
PASS: create_user?workspace=BOGUS returns 404
PASS: list_users (no workspace) returns all three
PASS: list_users?workspace=A returns only A's user
PASS: get_user A?workspace=A returns 200
PASS: get_user A?workspace=B returns 404 (cross-tenant probe blocked)
# --- IAM access keys (6) ---
PASS: create_access_key A?workspace=A stamps workspace + returns AKID
PASS: create_access_key on A?workspace=B returns 404 (cross-tenant blocked)
PASS: list_access_keys A?workspace=A returns own key
PASS: list_access_keys A?workspace=B returns 404
PASS: delete_access_key cross-tenant returns 404
PASS: delete_access_key own returns 204
# --- IAM policies (8) ---
PASS: put_user_policy A/p?workspace=A returns 204
PASS: put_user_policy A/p?workspace=B returns 404 (cross-tenant write blocked)
PASS: list_user_policies A?workspace=A includes our policy
PASS: list_user_policies A?workspace=B returns 404
PASS: get_user_policy A/p?workspace=A returns 200
PASS: get_user_policy A/p?workspace=B returns 404
PASS: delete_user_policy cross-tenant returns 404
PASS: delete_user_policy own returns 204
PASS: delete_user A?workspace=B returns 404 (cross-tenant blocked)
PASS: cleanup: IAM users deleted
# --- tritond root smoke (3) ---
PASS: tritond root create bucket returns 201
PASS: tritond root create lands in mantad with empty workspace (no scope leak)
PASS: tritond root delete bucket
PASS: cleanup: verify workspaces deleted
```

### Representative wire trace — IAM users

**Create scoped, response carries the stamped workspace:**

```
POST /admin/v1/users?workspace=t-c12fb1ee0000000000000000000000a1
body: {"name":"cw2u-user-a-xxxx"}

HTTP 200
{"name":"cw2u-user-a-xxxx","created_at":"...",
 "workspace":"t-c12fb1ee0000000000000000000000a1"}
```

**Cross-tenant get blocked with 404 (not 403, no name probe):**

```
GET /admin/v1/users/cw2u-user-a-xxxx?workspace=t-c12fb1ee0000000000000000000000b2
HTTP 404
{"code":"NotFound","message":"user cw2u-user-a-xxxx"}
```

### Representative wire trace — access keys

**Cross-tenant create against a user in workspace A using `?workspace=B`:**

```
POST /admin/v1/users/cw2u-user-a-xxxx/access-keys?workspace=t-c12fb1ee0000000000000000000000b2
HTTP 404
{"code":"NotFound","message":"user cw2u-user-a-xxxx"}
```

(The user exists in workspace A, so a workspace-B-scoped request
surfaces UserNotFound. AKIDs are globally unique, so the
`DELETE /admin/v1/access-keys/{id}?workspace=...` path uses
head_access_key to gate on the AK's stored workspace.)

### Representative wire trace — policies

**Cross-tenant policy PUT blocked:**

```
PUT /admin/v1/users/cw2u-user-a-xxxx/policies/other?workspace=t-c12fb1ee0000000000000000000000b2
body: {"Version":"2012-10-17","Statement":[...]}

HTTP 404
{"code":"NotFound","message":"user cw2u-user-a-xxxx"}
```

(Policies inherit the workspace via their owning user. Mismatched
workspace surfaces as UserNotFound — same shape as get_user — so
a tenant cannot probe for a sibling tenant's user-existence by
attempting to put a policy.)

## tritond root smoke (no regression on cluster-wide path)

```
==> tritond root smoke: http://192.168.1.182:8080 cluster=5e4e29a6-692a-4499-aa65-ee29909f156c
PASS: tritond root create bucket returns 201
PASS: tritond root create lands in mantad with empty workspace (no scope leak)
PASS: tritond root delete bucket
```

The middle case is load-bearing: it proves
`WorkspaceScope::Unscoped` (root operator) → `workspace_name()
== None` → mantad-client sends no `?workspace=` → mantad stores
the bucket with `workspace=""`, exactly as the admin-direct path.
No accidental scope leak through the rebuilt tritond with the
IAM commits on top.

## Out of scope / followups

* **Tenant-principal end-to-end.** The gate triggers off
  `Principal::Operator { tenant_id: Some(...) }`. Authenticating
  curl as a tenant-bound principal needs an API-key flow that
  isn't yet wired. The mantad-side wire shape is fully verified;
  full stack verification waits on that infra.
* **Presign routes.** Not part of cw2u (no mantad-client call —
  presign mints URLs directly), and the bucket-level gate
  underneath catches cross-workspace presign abuse on the eventual
  S3 GET/PUT.
* **`WorkspaceListAcrossTenants` Cedar gating.** The action is
  defined (commit `1f5b8fbd`) but the forwarder layer still
  unconditionally treats every root operator as `Unscoped` —
  follow-up slice introduces the explicit action check so
  non-root fleet operators can be granted cross-tenant view.

## Build / deploy notes

* Bundle built incrementally on `build02`. The wrapper script's
  ssh session died before the final `tar` step (same as the
  first deploy) — bundle was rolled by hand using the same
  layout, no semantic difference.
* First run of the verify hit a flaky 403 on the tritond-root
  smoke leg only. Root cause: token-refresh race in the outer
  ssh wrapper — `awk` extracted the stale `access_token` from
  `~/.config/tcadm/config.json` before `tcadm storage cluster
  list` triggered a refresh. The IAM cases themselves passed
  (they use the admin token in `/opt/mantad/etc/admin-token`,
  not the tcadm session token). Re-running with `tcadm storage
  cluster list` issued before the awk extract produced the
  clean 46/46 above. Future runs should issue any tcadm command
  first to warm the token file.
