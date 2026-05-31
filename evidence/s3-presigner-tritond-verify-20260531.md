# Per-workspace presigner — tritond-mediated live verify

**Date:** 2026-05-31 13:5x UTC
**Test box:** `192.168.1.182` (build00, SmartOS GZ)
**Tritond:** `nick-tritond-phase0 cc2ef6a2` (53.5 MB stripped). Built
on build02 with Rust 1.92 + `RUSTFLAGS="-L native=$HOME/lib"` +
`--features foundationdb`. Deployed to `/opt/tritond/bin/tritond`;
prior 66 MB build saved at `/opt/tritond/bin/tritond.prev` for
one-step rollback.
**Mantad:** unchanged from Phase 3 deploy (`surface-s3 8d50af4`).

## Plan reference

`~/.claude/plans/now-that-we-know-harmonic-twilight.md` rev 2,
§Verification §Phase 2 verify item 9–10 — the tritond-mediated
side that was filed as a follow-up after the mantad-side direct
verify (`evidence/s3-presigner-workspace-verify-20260530.md`).

This evidence closes `monitor-reef-f990` and proves the full
Phase 2 chain works end-to-end through tritond, not just on
the mantad side:

1. Tenant principal authenticates to tritond.
2. Calls tritond's `/v1/storage/clusters/{id}/s3/presign/put`.
3. Tritond resolves caller → `WorkspaceScope::Bound { t-X }`.
4. `presigner_cache.get_or_fetch(cluster_id, t-X)` hits the
   per-workspace key on mantad (cache cold → fetch; cache warm →
   hit). Caches result with 5-min TTL.
5. URL is signed with `presigner-t-X`'s AKID + SK (not cluster root).
6. Caller PUTs/GETs against the URL.
7. Mantad's data plane resolves caller to `Iam { workspace = t-X }`;
   gate fires on cross-workspace probes.

## Setup state (pre-verify)

Done once via `tcadm` on the test box before the verify:

```sh
sh /tmp/setup-presigner.sh 5e4e29a6-692a-4499-aa65-ee29909f156c \
    http://192.168.1.182:7443
```

Configures the registered storage cluster row with:
- `s3_endpoint = http://192.168.1.182:7443` (was missing — cluster
  registration only had the admin URL on `:7101`).
- `presigner_access_key_id = AKIA97EF32849F791C1D`,
  `presigner_secret_access_key = …` from `/opt/mantad/etc/root-creds`.
  These back the **fallback** path for Unscoped / fleet-admin
  presigns. Per-tenant presigns fetch a per-workspace key via the
  Phase 2 cache.

## Verify script

`tools/s3-presigner-tritond-verify.py` — runs from build02 with the
boto3 venv. Drives the full chain via SSH to the test box:

1. List `/v1/storage/clusters` to discover the registered cluster id.
2. Two fresh tenants in the existing `phase-c-silo` (`988ae554-...`).
   Each tenant gets a workspace minted by tritond automatically per
   the Phase D create-tenant flow.
3. `tcadm tenant create-user --username alice-a-... --password ...`
   for each tenant.
4. `POST /v1/auth/login` for each alice → access token (208 chars).
5. Each alice creates her bucket via the admin proxy
   (`POST /v1/storage/clusters/{id}/buckets`).
6. alice-a hits `POST .../s3/presign/put` for HER bucket → URL.
7. PUT to the URL → 200; GET → 200 + correct body.
8. alice-a hits the same endpoint for BOB's bucket → URL signed
   with `presigner-t-A`'s key but pointing at bucket-b. PUT → 404
   NoSuchBucket (Phase 1 gate fires on mantad because
   `bucket-b.workspace = t-B` and the caller resolves as
   `Iam { workspace = t-A }`).
9. Symmetric: alice-b for bucket-a.

The script also extracts the AKID from the `X-Amz-Credential`
query parameter and checks that it is **different** from the
cluster-root AKID — proves the cache+fetch+sign chain is actually
using the per-workspace key, not silently falling through to root.

## Results

```
== Discovering cluster registration ==
  cluster_id: 5e4e29a6-692a-4499-aa65-ee29909f156c
  (cluster_root_akid: AKIA97EF32849F791C1D)

== Provisioning two tenants in silo 988ae554-... ==
  tenant a: 287a0abe-ce9f-4174-af9a-e09792bd9c66
  tenant b: a467b5c1-3a79-494e-8365-859a96d1f291

== Minting tenant operator accounts ==
  a: alice-a-496a logged in (token len=208)
  b: alice-b-1b88 logged in (token len=208)

== Each operator creates a bucket via tritond admin proxy ==
  OK   alice-a created presign-tritond-a-004613 (HTTP 201)
  OK   alice-b created presign-tritond-b-f3e655 (HTTP 201)

== In-workspace presign PUT/GET ==
  alice-a presign URL AKID prefix: MKIASAIAEILHDPQG...
  OK   URL AKID differs from cluster-root (per-workspace cache engaged)
  OK   PUT in-workspace via tritond-signed URL -> 200
  OK   GET in-workspace round-trip -> 200 + correct body

== Cross-workspace presign — gate fires on mantad ==
  cross URL AKID: MKIASAIAEILHDPQG... (same as alice-a's in-workspace URL)
  OK   PUT cross-workspace -> 404 (NoSuchBucket / gate fires)
  OK   PUT cross-workspace B->A -> 404 (NoSuchBucket / gate fires)

== Result: 0 failure(s) ==
```

(The verify printed "cluster-root AKID unparseable in cluster show"
on the in-workspace section — the `tcadm storage cluster show`
text output doesn't include the presigner AKID; the JSON output
from `cluster list` does. The script's safety net fell through to
a soft pass because the URL AKID was clearly different from
`MKIASAIAEILHDPQG...` vs `AKIA97EF32849F791C1D`.)

## What this closes

- Tritond's `mint_presigned_url` correctly threads
  `WorkspaceScope::Bound { workspace_name }` into the per-workspace
  cache instead of falling back to cluster-root.
- The `PresignerCache.get_or_fetch` flow successfully calls
  mantad's `POST /admin/v1/workspaces/{name}/presigner` admin
  route, gets back `{access_key_id, secret_access_key}`, caches,
  and returns the per-workspace creds.
- Tritond's sigv4 signer uses those per-workspace creds — the
  emitted URL's `X-Amz-Credential` AKID is the per-workspace one,
  not the cluster-root one.
- The Phase 1 workspace gate on mantad's data plane fires on URLs
  that try to reach a bucket in another workspace, even when the
  URL was minted by tritond and the SigV4 signature itself was
  valid for the chosen bucket name (tritond signs whatever it's
  asked to sign — mantad enforces).

## Followups still open

- **Cache eviction on `drop_silo_tenant_storage`** — unit-tested
  but not exercised by this verify. Would require a tenant whose
  workspace is bound, then dropped, then re-bound; assert that the
  next sign request hits the new presigner not a stale cached
  entry. Smaller follow-up.
- **Multi-process tritond** — current deploy is single-process;
  the cache is per-process. A multi-node tritond would have each
  process maintain its own cache. Not a correctness issue, just a
  cold-start cost on first sign per process per workspace.
- **Cluster-root presigner fallback path** — verified
  configurable (the setup script set it up); no direct test that
  the fallback path *fires* for an Unscoped / fleet-admin caller.
  Would require a fleet-admin token; available via the cached
  tcadm session.
