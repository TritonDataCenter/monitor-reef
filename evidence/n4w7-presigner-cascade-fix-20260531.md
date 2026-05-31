# bd monitor-reef-n4w7 — cascade fix live verify

**Date:** 2026-05-31
**Test box:** `192.168.1.182`
**Tritond:** `nick-tritond-phase0` + cascade fix (built on build02, 13:47, stripped 66MB → 53MB, deployed)
**Mantad:** unchanged (`surface-s3 8d50af4`)

## What n4w7 was

`drop_silo_tenant_storage` 409s with mantad's
`WorkspaceNotEmpty: workspace t-<uuid> still has IAM user
presigner-t-<uuid>` — the Phase 2 presigner-system user
provisioned on workspace creation is never auto-cascaded on
`delete_workspace`, so the drop flow can't reach
`presigner_cache.evict`. Surfaced by
`evidence/s3-presigner-cache-fallback-verify-20260531.md` which
had to add an explicit
`DELETE /v1/storage/clusters/{id}/users/presigner-{ws}` step
before `drop-storage`.

## Fix

`services/tritond/src/handlers/tenants.rs::archive_tenant_workspace`
now deletes the `presigner-{workspace}` IAM user before calling
`mantad.delete_workspace`. The cascade is symmetric with Phase 2's
provision flow: tritond mints the system user on init, tritond
tears it down on archive. 404 on the delete-user call is
tolerated (idempotent for partial-retry or out-of-band cleanup),
same shape as the existing 404 tolerance on the delete-workspace
call.

Chose the tritond-side cascade over a mantad-side "auto-cascade
\*-system users on delete\_workspace" because:

- tritond is what *created* the user; symmetry of provision and
  teardown belongs on the same side
- mantad's `WorkspaceNotEmpty` is otherwise a load-bearing safety
  rail (`Phase D §empty-workspace gate`); special-casing system
  users on that side weakens an operator guardrail to fix a
  cleanup gap
- the change is local to `archive_tenant_workspace` and
  audit-stage-tagged (`archive.mantad.delete_presigner_user`)
  so failures are traceable

## Verify

`tools/s3-presigner-cache-fallback-verify.py
--skip-n4w7-workaround` — same script as the cache+fallback
verify, but with the explicit pre-drop DELETE skipped. If the
cascade fix is wrong, `drop-storage` 409s and the verify aborts.

## Result

```
== Item 3: cache eviction on drop_silo_tenant_storage ==
  tenant created: 85a6d507-1e83-4cb8-bb22-0cfb440f9e62 (cache-evict-108c)
  workspace:      t-85a6d5071e834cb8bb220cfb440f9e62
  operator logged in: alice-cache-108c
  pre-drop  AKID: MKIAZ7MJZNJLWCMMTWYK
  skipping n4w7 workaround (--skip-n4w7-workaround): drop-storage must cascade the presigner-system user itself
  drop-storage:   Dropped storage binding for tenant 85a6d507-1e83-4cb8-bb22-0cfb440f9e62
  init-storage:   Initialised storage binding for tenant 85a6d507-1e83-4cb8-bb22-0cfb440f9e62
  post-rebind AKID: MKIAYDLRXODHVDQJBQNQ
  OK   pre-drop AKID != post-rebind AKID — cache evicted, fresh fetch on rebind

== Item 4: cluster-root presigner fallback (Unscoped) ==
  admin URL AKID: AKIA97EF32849F791C1D
  OK   admin URL signed with cluster-root AKID — Unscoped fallback path engaged

== Cleanup ==
  cleaned up 85a6d507

== Result: 0 failure(s) ==
```

## What this closes

- bd monitor-reef-n4w7 — cascade gap.
- The workaround in
  `tools/s3-presigner-cache-fallback-verify.py` is still
  available for older builds; on a deployed-fix build, run with
  `--skip-n4w7-workaround` and drop-storage handles the cascade
  itself.

## Deploy state

Tritond binary rollback chain on `192.168.1.182`:
- `/opt/tritond/bin/tritond` (this commit, cascade-fix)
- `/opt/tritond/bin/tritond.prev` (cc2ef6a2, Phase 2 verify build)
- earlier `tritond.prev` chain replaced one step at a time
  (one-step rollback semantics retained).
