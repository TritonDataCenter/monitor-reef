# mantad empty-workspace gate — live verify

**Date:** 2026-05-30 17:34 UTC
**Test box:** `192.168.1.182` (`build00`, SmartOS GZ)
**Bundle:** `phase-c-bundle-fb6cebd6-601bc00.tar.gz`

## Code under test

| Repo | HEAD | What it lands |
| --- | --- | --- |
| `manta-storage` | `601bc00` | empty-workspace 409 gate on `delete_workspace` admin route |
| `monitor-reef` | `fb6cebd6` | unchanged (drop-storage now properly propagates the 409) |

## Scenario

Re-run the case the drop-storage live verify caught: drop on a
workspace that still has a bucket. Earlier verify
(`evidence/tcadm-tenant-drop-storage-20260530.md`) noted mantad
silently archived the workspace despite the bucket, leaving an
orphan row. With the gate in place, mantad should now 409
`WorkspaceNotEmpty` and tritond should propagate it.

## Results

```
=== fresh tenant + init-storage
WS=t-06271ba728634722a3fceb87a7fe1d87

=== populate workspace with a bucket
{"name":"empty-gate-blocker","workspace":"t-06271ba7...",...}
HTTP 200

=== drop-storage with non-empty workspace — EXPECT 409
Error: drop tenant storage binding

Caused by:
    Error Response: status: 409 Conflict; ...
    value: Error {
      error_code: Some("Conflict"),
      message: "mantad upstream error: 409:
                {\"code\":\"WorkspaceNotEmpty\",
                 \"message\":\"workspace t-06271ba7...
                              still contains bucket
                              empty-gate-blocker
                              (and possibly others);
                              drain buckets before archiving\"}",
      ...
    }

=== confirm binding intact
  "storage_cluster_id": "5e4e29a6-692a-4499-aa65-ee29909f156c"
  "storage_workspace_id": "06271ba7-2863-4722-a3fc-eb87a7fe1d87"

=== drain the bucket
HTTP 204

=== retry drop-storage — succeeds
Dropped storage binding for tenant 06271ba7-2863-4722-a3fc-eb87a7fe1d87
  (workspace archived on mantad; tenant row is now unbound)
```

## What this closes

The followup logged in `evidence/tcadm-tenant-drop-storage-20260530.md`:

> mantad: confirm empty-workspace check ... Either the
> enforcement isn't on the delete-workspace path, or this
> build is missing it.

It wasn't wired before. Now it is:

* Implemented at the admin API handler in
  `mantas3_cluster::admin::delete_workspace`, not at the meta
  layer. Reason: a delete-time scan of bucket + user rows
  inside the FDB delete_workspace transaction risks the 10 MB
  / 5 s FDB transaction limits on large clusters. The admin
  handler does paginated list scans already.
* Checks bucket existence first, then user existence. Access
  keys cascade with users; policies are inline on User rows.
  An empty (no buckets AND no users) workspace has no
  orphan-able descendants.
* Message names the first blocker found and points the
  operator at the drain action.

The tritond-side drop-storage handler propagates this 409
unchanged — no code change needed on the tritond side because
`mantad_error_to_http_audit` already maps mantad-side 409 to
tritond-side 409 with the upstream body preserved.

## Followups

* **Workspace usage counter could include bucket count.** Today
  `WorkspaceUsage` carries `used_bytes` + `used_objects` (data-
  plane counters). Adding `used_buckets` would let the gate
  avoid the `list_buckets()` scan — a `head_workspace_usage()`
  call would be enough. Defer until the bucket count grows
  enough to matter for the gate's latency.
* **User cascade is implicit.** Deleting a user cascades to its
  access keys (per `mantas3_meta::delete_user` semantics) but
  does NOT cascade to the user's inline policies (those are
  inline on the User row and get dropped with it). Worth
  documenting near the gate.
