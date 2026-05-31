# `mantad-adm bucket repair-workspace` — live verify (Phase 3)

**Date:** 2026-05-31 02:32 UTC.
**Test box:** `192.168.1.182` (build00, SmartOS GZ).
**mantad:** `surface-s3 8d50af4` (manta-storage), release profile,
`--features mantad/fdb`. Both `mantad` (17.9 MB stripped) and
`mantad-adm` (4.8 MB stripped) deployed.

## Plan reference

`~/.claude/plans/now-that-we-know-harmonic-twilight.md` rev 2,
§Phase 3 item (1): `mantad-adm bucket repair-workspace`.

## What landed

| Repo | Commit | Change |
| --- | --- | --- |
| `manta-storage` | `8d50af4` (surface-s3) | New admin route `PUT /admin/v1/buckets/{name}/workspace` (validates target workspace exists, calls existing `MetaStore::update_bucket`); new `mantad-adm bucket repair-workspace --bucket NAME --workspace t-X` operator CLI; `put_admin` helper in mantad-adm |

mantad.phase2 and mantad-adm.phase2 saved on the test box for
one-step rollback to Phase 2.

The two other Phase 3 items from the plan are accounted for:

- **`gate_denied` tracing event** was pulled forward into the Phase
  1 patch by the round-2 reviewer feedback. Already live since the
  Phase 1 deploy.
- **SDK-driven integration test harness** is deferred as bd
  `monitor-reef-krw4`. The in-process MantadS3 bootstrap is
  non-trivial and the live verify scripts cover the matrix
  end-to-end today; this is a developer-ergonomics win for working
  without a dev cluster.

## Verify script

`tools/s3-repair-workspace-verify.py` — runs from build02 with the
boto3 venv at `/tmp/boto-verify-venv`. Provisions a fresh workspace
+ IAM user, creates a legacy `workspace = ""` bucket through the
admin API (the pre-Phase-1 cohort), proves the workspace-bound IAM
caller cannot see it, runs `mantad-adm bucket repair-workspace`,
proves the same caller can now operate on the bucket, and confirms
idempotency.

## Results

```
== Provisioning target workspace + IAM caller ==
  workspace: t-5f0b964f992741d98dd736a22d303207
  alice ak=MKIAIWLV...

== Creating legacy bucket phase3-legacy-f4fd2a73 (workspace="") ==
  OK   admin view: workspace=""  owner=alice-repair

== Pre-repair: alice cannot see the legacy bucket ==
  OK   alice.head_bucket(phase3-legacy-f4fd2a73) -> NoSuchBucket

== Running mantad-adm bucket repair-workspace --workspace t-5f0b964f992741... ==
  mantad-adm stdout: bucket phase3-legacy-f4fd2a73:
    workspace stamp updated -> "t-5f0b964f992741d98dd736a22d303207"
  {"name":"phase3-legacy-f4fd2a73","owner":"alice-repair","created_at":"...",
   "workspace":"t-5f0b964f992741d98dd736a22d303207","object_count":null,
   "total_bytes":null}
  OK   admin view post-repair: workspace=t-5f0b964f992741d98dd736a22d303207

== Post-repair: alice can now operate on the bucket ==
  OK   alice.head_bucket(phase3-legacy-f4fd2a73) post-repair -> 200
  OK   alice.put+get(repair-probe.txt) round-trip

== Idempotency: rerun repair against the same workspace ==
  OK   idempotent: workspace unchanged on 2nd repair

== Result: 0 failure(s) ==
```

## What this closes

- Operators have a one-shot, idempotent command to stamp a real
  workspace onto legacy bucket rows that pre-date the Phase 1 gate.
  Without this command, every legacy bucket would remain
  root-only forever.
- The admin route is auth-token gated (same `check_auth` as every
  other `/admin/v1/*` route) and validates the target workspace
  exists. Typos return 404 instead of orphan-stamping a bucket to
  a non-existent workspace.
- Round-trip is verified end-to-end: pre-repair, alice
  (workspace-bound) 404s on the bucket; post-repair, the same
  caller sees + reads + writes through it.

## Followups

- **bd monitor-reef-krw4** (P3): SDK-driven integration test harness
  for the workspace gate. Deferred from Phase 3.
- Migration playbook: a runbook that maps the legacy bucket list to
  their owning workspaces (currently a `mantad-adm bucket list`
  scan + manual cross-reference). Out of scope for this slice but
  worth a follow-up when a real cluster needs migration.
