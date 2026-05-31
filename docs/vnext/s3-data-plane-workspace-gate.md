# S3 data-plane workspace gate

**Status (2026-05-31):** Code-complete and live-verified on
`192.168.1.182`. All three phases shipped + a deferred integration-
test harness. Local-only on the `surface-s3` branch in both
`monitor-reef` and `manta-storage` per the standing "do not push"
boundary.

This work closes the data-plane half of the S3 surface workspace
isolation. The admin-plane half (cw2u / c8ft fanout) is documented
separately in [`phase-d-s3-workspace-isolation.md`](phase-d-s3-workspace-isolation.md);
read that first if you want the prior arc.

## The bug

Mantad's S3 data plane resolved each SigV4-authenticated caller to
an owner *string* (the IAM user name) via `caller_owner` in
`manta-storage/crates/mantas3/s3/src/s3_impl.rs` and gated every
operation by that string. The user's `workspace` field was never
consulted.

Two concrete consequences:

1. **Cross-tenant exposure via colliding usernames.** Mantad's IAM-
   user namespace is global at the FDB layer (the key is `user.name`
   only), but tritond mints workspace-scoped users via the admin
   forwarder. Two tenants could not actually mint users with the
   same name today — but the gate's *behaviour* was unsound. The
   unit test `cross_tenant_username_collision_is_blocked` proves
   the new gate would catch it even if the meta layer ever
   permitted it.
2. **Data-plane bucket creates were workspace-less.** `s3_impl.rs`
   stamped `workspace: ""` on every bucket created via SigV4. Those
   buckets lived in a "no workspace" namespace that no workspace-
   bound tenant should see — and they were owned by whoever signed
   the create. A tenant that knew (or guessed) a bucket name could
   pre-feature reach across into another tenant's buckets via
   `head_bucket` → ACL ownership match.

Phase D shipped the admin-plane half (tritond's `?workspace=t-X`
forwarder + admin-route stamping). The data-plane half required
this work.

## The plan

[`~/.claude/plans/now-that-we-know-harmonic-twilight.md`](../../../../.claude/plans/now-that-we-know-harmonic-twilight.md)
rev 2 — `Mantad S3 data-plane workspace gate`. Unanimously approved
by the nine-reviewers panel in round 2 after rev 1 got `Revise`
with 3 HIGH and 4 MEDIUM issues; rev 2 addressed each and was
clean.

## Phase 1 — `CallerContext` + 37-handler gate fanout

The security win. Mechanical.

**Code:** `manta-storage eca7339` (surface-s3), monitor-reef
`60971aaa` (consolidation refactor earlier this session was a
sibling cleanup).

**What changed:**

* `CallerContext` enum (`Anonymous | Root { owner } | Iam { owner,
  workspace }`) replaced the old `caller_owner: String`. Total
  pattern match in `bucket_visible_to`. Fail-closed on metastore
  errors in `caller_context`.
* New `head_bucket_for(svc, name, &caller)` wrapper: looks up the
  bucket, applies the workspace gate, returns `NoSuchBucket` (404)
  on mismatch. Matches the existing cross-owner convention; avoids
  the 403-side-channel that would let one tenant probe another's
  bucket-name namespace.
* The fanout deleted `caller_owner` to force the compiler to
  enumerate every site. 48 call sites surfaced (37 S3 trait handlers
  plus 11 multipart/copy/internal-lookup repeats). Every bucket-
  touching handler now transits `head_bucket_for`; multipart `upload_part`
  / `complete_multipart_upload` / `abort_multipart_upload` /
  `list_parts` re-resolve the bucket on every call (per-call gate is
  the key defence against upload-id injection); `copy_object` and
  `upload_part_copy` gate both source and destination.
* `list_buckets` is a total match on `CallerContext`: `Root` →
  unfiltered, `Iam` → workspace-filter via the new
  `MetaStore::list_buckets_in_workspace(workspace: &str)`, `Anonymous`
  → empty Vec.
* `create_bucket` stamps `caller.workspace` onto the new `MBucket`
  row. Root keeps the empty stamp for cluster-scoped admin work.
* The `gate_denied` tracing event was pulled forward into this
  phase per round-2 reviewer feedback ("operators need denial
  telemetry from day one"), instead of waiting for Phase 3.

**Decisions locked in plan rev 2 §Locked-decisions:**

1. Identity model is the enum (not a struct with `Option<String> +
   is_root: bool`).
2. Gate placement: one wrapper, `head_bucket_for`.
3. Write-stamping at one site: `create_bucket` only. Object /
   upload / copy / multipart all gate transitively through their
   bucket — no `ObjectMeta`/`UploadMeta` schema change.
4. Anonymous is unscoped; passes the workspace gate; downstream
   ACL gate handles allow/deny (load-bearing — every Anonymous
   admit MUST be followed by an ACL check; review checklist
   enforces it).
5. Authenticated cross-workspace public-read is BLOCKED. An IAM
   caller in t-A cannot see a public-read bucket in t-B even
   though anonymous (no SigV4 → workspace=None → gate skipped) can.
   Safer default; cross-tenant public access uses the anonymous
   code path.
6. Mismatch → `NoSuchBucket` 404, not 403. Side-channel matters.
7. Legacy IAM keys (`workspace = ""`) are bound to the literal-
   empty workspace cohort. Migration is the operator's choice
   (see Phase 3).
8. `list_buckets_in_workspace` is scan + filter at admin rate.
   Promotion to a secondary index is gated on a real metric.
9. Presigner gets its own phase (Phase 2). Phase 1 leaves
   presigned URLs authenticating as root and skipping the gate.
10. Migration is operator-driven, not automatic (see Phase 3).

**Unit tests** (6 new in `s3_impl::gate_tests`):
- `root_sees_every_workspace`
- `anonymous_passes_workspace_gate`
- `iam_caller_bound_to_own_workspace`
- `legacy_iam_caller_sees_only_legacy_buckets`
- `cross_tenant_username_collision_is_blocked` (the named bug as
  a regression test)
- `caller_owner_accessor`

**Live verify on 192.168.1.182** ([`evidence/s3-data-plane-gate-verify-20260530.md`](../../evidence/s3-data-plane-gate-verify-20260530.md)):
12/12 cross-tenant probes → `NoSuchBucket` (head, list, put, get,
delete, multipart, copy, delete-bucket — both directions); 2/2
list-buckets visibility checks; same-workspace put+get round-trip.

## Phase 2 — per-workspace presigner credentials

Closes the presigned-URL-as-root bypass that Phase 1 left open.

**Code:**
- `manta-storage 92869da` — mantad admin route + client method
- `manta-storage 1b71685` — `is_workspace_presigner` ACL recognition
- `monitor-reef 1e2edf84` — presigner cache + scope threading +
  drop-storage eviction + verify script

**The bypass:** Tritond's `mint_presigned_url` signed every URL
with the cluster-root presigner key. Mantad resolved presigned
requests as `Root { .. }`, the workspace gate didn't fire, and a
tenant operator could request a presigned URL and rewrite the
bucket name to another tenant's. (The SigV4 signature would only
catch the *exact* bucket rewrite; an operator authorized to mint
presigns for any bucket name they plausibly knew was already past
the auth gate on tritond's side.)

**The closure:** Mint a non-root IAM-style access key per
workspace, sign with that. Mantad's `caller_context` resolves the
presigned request as `Iam { workspace: t-X, owner: "presigner-t-X" }`
and the gate fires.

**Mantad side:**
- New admin endpoint `POST /admin/v1/workspaces/{name}/presigner`
  mints (or fetches) a system IAM user `presigner-{workspace}`,
  workspace-stamped, with one Active access key. Idempotent —
  every call returns the same `(access_key_id, secret_access_key)`.
- `is_workspace_presigner(bucket, owner)` ACL helper teaches
  `acl_allows_read/write` to recognise `presigner-{bucket.workspace}`
  as a workspace-system role. Bounded by the gate's exact-
  workspace-match (a presigner from another workspace 404s at
  `head_bucket_for` before the ACL helper runs).

**Tritond side:**
- `presigner_cache` module with 5-minute TTL, keyed on `(cluster_id,
  workspace)`. Secret stays mantad-side authoritative. Threat-model
  rationale (FDB compromise blast radius) in the module doc-comment.
- `mint_presigned_url` takes `Option<&str> workspace`: `Some` → per-
  workspace fetch via cache; `None` → cluster-root fallback for
  operator / fleet-admin tooling.
- `drop_silo_tenant_storage` evicts the cache entry after mantad's
  workspace delete cascades the system user's row.

**Discarded alternative** (signed-claim sidecar with
`X-Amz-Mantad-Workspace=...`): mechanically simpler but conflates
identity with "what was signed for," and leaks workspace names into
URLs that may be logged. Plan rev 2 §Phase 2 has the reasoning.

**Live verify** ([`evidence/s3-presigner-workspace-verify-20260530.md`](../../evidence/s3-presigner-workspace-verify-20260530.md)):
idempotency confirmed (2nd call → same AK + secret); in-workspace
PUT/GET round-trip 200; cross-workspace URL rewrite blocked in
both directions.

## Phase 3 — migration tool + observability + integration test

**Three sub-items, all shipped:**

### (a) `mantad-adm bucket repair-workspace`

**Code:** `manta-storage 8d50af4`.

Operator-driven workspace stamping for pre-Phase-1 buckets that
landed with `workspace = ""`. Without this command those buckets
remain root-only forever — they fail the Phase 1 gate for any
workspace-bound IAM caller.

- New admin route `PUT /admin/v1/buckets/{name}/workspace` —
  validates the target workspace exists (404 on typo), then
  overwrites via the existing `MetaStore::update_bucket`. Idempotent.
- New `mantad-adm bucket repair-workspace --bucket NAME --workspace
  t-X` CLI subcommand. Single-peer call (bucket admin lives on the
  primary).
- New `put_admin` helper in mantad-adm to mirror `post_admin` /
  `delete_admin`.

**Live verify** ([`evidence/s3-repair-workspace-verify-20260531.md`](../../evidence/s3-repair-workspace-verify-20260531.md)):
pre-repair, a workspace-bound IAM caller 404s on the legacy bucket
(gate fires); `mantad-adm bucket repair-workspace` stamps the
workspace; post-repair, same caller can head + put + get; 2nd
repair is a no-op (idempotent).

### (b) `gate_denied` tracing event

Pulled forward into Phase 1 per round-2 reviewer feedback —
shipped with the gate itself. `head_bucket_for` emits a structured
`tracing::warn!` with caller-workspace, bucket-workspace, and
bucket-name on every denial. Operators get "missing vs denied"
off-band telemetry from day one rather than waiting on the Phase 3
audit-log polish.

### (c) In-process aws-sdk-s3 integration test

**Code:** `manta-storage ab00c42` —
[`tests/workspace_gate.rs`](../../../manta-storage/crates/mantas3/s3/tests/workspace_gate.rs).

Originally deferred from Phase 3 (bd `monitor-reef-krw4`); shipped
in this session as an addition. Spins up `MantadS3` in-process
(fjall meta + LocalFs storage + s3s SigV4 framework, no cluster
wiring), binds on `127.0.0.1:0`, drives via `aws-sdk-s3` from the
same tokio runtime. Mirrors the live verify matrix in 0.54 s; no
cluster needed.

The fjall meta backend stubs out the workspace plane (it's FDB-
only in production). The S3 data-plane gate does string comparison
on `bucket.workspace == caller.workspace` and never looks up the
workspace row, so the test sidesteps the fjall stub by skipping
`put_workspace`.

## What works end-to-end

Verified on the live deploy + on the integration test:

1. **SigV4-direct caller hits mantad.** Mantad's `caller_context`
   resolves access key → user → `Iam { workspace }`.
2. **Workspace gate fires on every bucket touch.** Cross-workspace
   probe = `NoSuchBucket`, regardless of operation (head, list,
   put, get, delete, multipart, copy, delete-bucket).
3. **Bucket creates stamp the caller's workspace.** A SigV4-direct
   create lands the bucket in the caller's workspace, not the
   "no workspace" cohort.
4. **`list_buckets` is workspace-scoped.** IAM callers see only
   their workspace's buckets. Root sees the whole cluster.
5. **Presigned URLs authenticate as a workspace-bound IAM identity,
   not as root.** Cross-workspace URL rewrites 404 at the gate
   *and* the ACL would 403 even if the gate let them through.
6. **Anonymous + public-read still works.** No SigV4 → workspace =
   None → gate skipped → ACL allows. Cross-tenant public access is
   the anonymous code path.
7. **Authenticated cross-workspace public-read is blocked.** Plan
   §5; audit-log line surfaces the deny.
8. **Legacy buckets can be migrated.** `mantad-adm bucket repair-
   workspace` stamps `workspace` onto pre-Phase-1 rows, idempotent.

## Verification matrix

| Phase | Unit tests | Live verify | Evidence |
| --- | --- | --- | --- |
| Phase 1 | 6 in `s3_impl::gate_tests` + 1 in `presigner_cache::tests` (pulled forward) | 12/12 cross-tenant probes + 2/2 visibility + 1/1 round-trip | [`s3-data-plane-gate-verify-20260530.md`](../../evidence/s3-data-plane-gate-verify-20260530.md) |
| Phase 2 | 1 in `gate_tests::workspace_presigner_recognized_only_for_own_workspace` + 1 in `presigner_cache::evict_is_idempotent` | idempotency + in-workspace PUT/GET + cross-workspace rewrite blocked both directions | [`s3-presigner-workspace-verify-20260530.md`](../../evidence/s3-presigner-workspace-verify-20260530.md) |
| Phase 3a | mantad-adm CLI integration verified end-to-end | pre-/post-repair visibility flip + idempotency | [`s3-repair-workspace-verify-20260531.md`](../../evidence/s3-repair-workspace-verify-20260531.md) |
| Phase 3b (followup) | 1 in-process integration test exercising the full probe matrix in 0.54 s | — (no live cluster needed) | [`workspace_gate.rs`](../../../manta-storage/crates/mantas3/s3/tests/workspace_gate.rs) |

102 tritond lib tests pass after Phase 2 (was 101 before, 1 new
for the cache). 7 mantas3-s3 gate tests pass after Phase 2 (was
6 after Phase 1, 1 new for the presigner ACL).

## Commit chain

`manta-storage surface-s3` (16 commits ahead of `origin/main`,
local-only):

```
ab00c42 mantas3-s3: in-process workspace-gate integration test
8d50af4 mantas3: mantad-adm bucket repair-workspace (Phase 3)
1b71685 mantas3-s3: workspace-presigner ACL recognition (Phase 2)
92869da mantas3: per-workspace presigner admin route (Phase 2)
eca7339 mantas3: S3 data-plane workspace gate (Phase 1)
601bc00 mantad: empty-workspace 409 gate on delete_workspace   ← prior session
9ed8dcd mantad: workspace-scope query param on IAM admin routes
138d00e mantad: workspace-scope query param on bucket admin routes
7cfa4f2 mantad-client: rustfmt pass on workspace-mirror touched files
51868ae mantad-client: workspace methods on MantadClient
cf25eff mantad-client: workspace types + workspace field on existing mirrors
0fad370 mantas3: rustfmt pass on workspace-primitive touched files
3423328 mantas3-meta: add workspace field to Bucket, IamUser, AccessKey
d3c5991 mantas3-cluster: idempotent POST /admin/v1/workspaces keyed by tenant_uuid
f6b20d0 mantas3-cluster: admin routes for workspace CRUD plus quota and usage
db2e63a mantas3-meta: add Workspace primitive types and FDB storage layer
```

`monitor-reef surface-s3` (and `nick-tritond-phase0` at the same
tip), 18 commits ahead of `origin/surface-s3`, local-only:

```
16ece142 beads: close workspace-gate integration test harness (Phase 3b)
009c1427 evidence: mantad-adm bucket repair-workspace live verify (Phase 3)
5100c073 evidence: per-workspace presigner live verify (Phase 2)
1e2edf84 tritond: per-workspace presigner credentials (Phase 2)
00fb1bd2 evidence: S3 data-plane gate live verify (Phase 1)
f87aad39 docs(vnext): mark Phase D followup #2 done — archive consolidation
60971aaa refactor(tritond): share workspace-archive helper between tenant-delete and drop-storage
69d79998 evidence: mantad empty-workspace gate live verify   ← prior session
```

…plus the prior Phase D arc going back further.

## Tracking

Four bd issues, all closed:

| bd | title | closed at |
| --- | --- | --- |
| `monitor-reef-d420` | Phase 1: S3 data-plane workspace gate | `eca7339` |
| `monitor-reef-qi82` | Phase 2: per-workspace presigner credentials | `5100c073` |
| `monitor-reef-64zl` | Phase 3: workspace-gate migration tool + integration tests | `009c1427` |
| `monitor-reef-krw4` | Workspace-gate integration test harness (Phase 3 followup) | `16ece142` |

Memory entries:
- `bd memories phase-1-s3-data-plane-gate`
- `bd memories phase-2-presigner-credentials`
- `bd memories phase-3-workspace-gate-migration`

## Operational notes

- **mantad needs explicit `MANTAD_ROOT_ACCESS_KEY_ID` +
  `MANTAD_ROOT_SECRET_ACCESS_KEY`.** Otherwise it runs in dev mode
  with SigV4 verification disabled — any request claims any
  access key id and the gate is bypassable.
  [`tools/phase-c-deploy.sh`](../../tools/phase-c-deploy.sh) §4b
  now generates these on first deploy and persists them to
  `/opt/mantad/etc/root-creds` (mode 600); subsequent deploys
  reuse the existing file so bucket/AK rows on FDB still validate
  against the same key across redeploys.
- **Admin port on mantad is :7101 (internal), not :7443 (S3
  data-plane).** Bearer-auth headers on :7443 are rejected by the
  SigV4 verifier. mantad-adm has `--admin-url` defaulting to the
  public port; it only works because the few endpoints mantad-adm
  hits today (`status`, `placement`, `node`) are *also* served on
  :7443 for legacy reasons. New admin routes (like Phase 2's
  presigner-provision and Phase 3's repair-workspace) live on
  :7101 only.
- **`strip` the release binaries before scp** — folded into
  [`tools/phase-c-build.sh`](../../tools/phase-c-build.sh) after
  the staging copy. 91 MB unstripped → ~18 MB stripped; the
  difference matters when the network to the test box is slow.
- **Rollback chain on `192.168.1.182`**:
  `/opt/mantad/bin/mantad` (Phase 3) →
  `mantad.phase2` →
  `mantad.phase1` →
  `mantad.prev` (the pre-Phase-1 build before the gate shipped).
  One-step rollback per phase via `mv mantad.{N,}`.

## Cross-references

- [`phase-d-s3-workspace-isolation.md`](phase-d-s3-workspace-isolation.md) — admin-plane half of the same arc.
- [`evidence/s3-data-plane-gate-verify-20260530.md`](../../evidence/s3-data-plane-gate-verify-20260530.md) — Phase 1 live verify.
- [`evidence/s3-presigner-workspace-verify-20260530.md`](../../evidence/s3-presigner-workspace-verify-20260530.md) — Phase 2 live verify.
- [`evidence/s3-repair-workspace-verify-20260531.md`](../../evidence/s3-repair-workspace-verify-20260531.md) — Phase 3 (repair tool) live verify.
- [`tools/s3-data-plane-gate-verify.py`](../../tools/s3-data-plane-gate-verify.py) — Phase 1 verify script.
- [`tools/s3-presigner-workspace-verify.py`](../../tools/s3-presigner-workspace-verify.py) — Phase 2 verify script.
- [`tools/s3-repair-workspace-verify.py`](../../tools/s3-repair-workspace-verify.py) — Phase 3 verify script.
- [`tests/workspace_gate.rs`](../../../manta-storage/crates/mantas3/s3/tests/workspace_gate.rs) (manta-storage) — Phase 3b in-process integration test.
- [`evidence/s3-presigner-cache-fallback-verify-20260531.md`](../../evidence/s3-presigner-cache-fallback-verify-20260531.md) — cache eviction + cluster-root fallback live verify.
- [`tools/s3-presigner-cache-fallback-verify.py`](../../tools/s3-presigner-cache-fallback-verify.py) — verify script for the cache+fallback paths.

## Followups (out of scope)

- **Push.** Both `surface-s3` branches are local-only per the
  standing "do not push" boundary.
- **bd `monitor-reef-n4w7`** — `drop_silo_tenant_storage` 409s on
  the mantad-side cascade gap. The Phase 2 presigner-system user
  isn't auto-cascaded by `delete_workspace`; the verify in
  [`evidence/s3-presigner-cache-fallback-verify-20260531.md`](../../evidence/s3-presigner-cache-fallback-verify-20260531.md)
  works around it. Fix is small (either auto-cascade `*-system`
  users on mantad, or have tritond explicitly delete the
  presigner-system user before delete_workspace).
- **Operator runbook for `mantad-adm bucket repair-workspace`**: a
  procedure for mapping the legacy bucket list to their owning
  workspaces (currently a `mantad-adm bucket list` scan + manual
  cross-reference; not automated). Out of scope until a real
  cluster needs migration.
- **Tritond-mediated presign live verify.** Phase 2 evidence
  captures the mantad side end-to-end. A full integration verify
  via tritond's `/v1/storage/clusters/{id}/presign-{get,put}`
  endpoint with an authenticated tenant principal would prove the
  cache + scope threading + drop-storage eviction work
  end-to-end. The unit tests cover the pieces; the live integration
  is a small followup.
