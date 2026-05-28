# Phase C deploy & verify on 192.168.1.182

End-to-end verification of the vnext Tenant ↔ mantad Workspace
binding (Phase C, commits `8102f736` through `0d293d81` on
`nick-tritond-phase0`, plus the manta-storage workspace work
from a prior session).

This is **not** a production deploy procedure — it is the
runbook for the Phase C verification (beads `co7h` happy-path
and `ifby` failure-paths). The test target is the existing
SmartOS GZ deploy at `192.168.1.182`.

## Inputs you provide

| Variable           | What it is                                                |
| ------------------ | --------------------------------------------------------- |
| `BUILD_HOST`       | SSH alias for the build machine (e.g. `build02`)          |
| `BUILD_WORKDIR`    | path on `BUILD_HOST` containing both repo checkouts      |
| `TARGET_HOST`      | the SmartOS test box (`192.168.1.182`)                    |
| `TARGET_USER`      | usually `root` (SmartOS GZ)                               |
| `FDB_CLUSTER_FILE` | path to the existing FDB cluster file on `TARGET_HOST`    |

The scripts in `tools/` take these as positional args or
environment variables — defaults match the conventions
established in earlier deploys.

## Topology after the deploy

```
                                ┌────────────────────────────────┐
                                │ SmartOS GZ 192.168.1.182        │
                                │                                │
                                │  ┌──────────┐   ┌───────────┐  │
                                │  │ tritond  ├──▶│  mantad   │  │
                                │  │ :8443    │   │  :7101    │  │
                                │  └────┬─────┘   │  :7443    │  │
   tcadm  ───  HTTPS  ──────────┼──────┘         │  :7102    │  │
   workstation                  │                 └─────┬─────┘  │
                                │     ┌─────────────────┴──────┐ │
                                │     │ FoundationDB cluster   │ │
                                │     └────────────────────────┘ │
                                └────────────────────────────────┘
```

Mantad shares FDB with tritond (different subspaces) and listens
on `:7101` for the admin API tritond's forwarder calls.

## Order of operations

1. **Build on `BUILD_HOST`** — `tools/phase-c-build.sh`
   - Native build for `x86_64-unknown-illumos` (SmartOS).
   - Bundles `mantad`, `tritond`, `tcadm`, and a stub SMF
     manifest into `phase-c-bundle-<git-sha>.tar.gz`.
   - Output stays on `BUILD_HOST`; the user copies it to
     `TARGET_HOST` themselves (the runbook does not push
     binaries through `scp` automatically — the human is
     the trust boundary on test deploys).

2. **Deploy on `TARGET_HOST`** — `tools/phase-c-deploy.sh`
   - Stops the existing `tritond` (graceful: SIGTERM, then
     SIGKILL after 10s if still alive).
   - Unpacks the bundle to `/opt/triton/bin/`.
   - Creates `/var/mantad/{meta,data}` if absent.
   - Writes a minimal mantad config to
     `/opt/triton/etc/mantad.toml` (admin bearer token,
     FDB cluster path, single-node mode).
   - Launches `mantad` under `nohup` writing logs to
     `/var/log/mantad.log`. SMF wrapping is out of scope
     for the test deploy — the script restarts on rerun.
   - Launches `tritond` the same way.
   - Verifies both processes are listening on their
     expected ports before returning.

3. **Register the cluster + bind defaults**

   ```sh
   tcadm storage cluster add \
       --name mantad-01 \
       --cluster-endpoint http://192.168.1.182:7101 \
       --admin-token "$(cat /opt/triton/etc/mantad-admin-token)" \
       --surface s3 --json
   # capture the returned `id` field
   tcadm config set storage.default_s3_cluster_id <id-from-add>
   # optional:
   tcadm config set storage.default_workspace_quota_bytes 107374182400
   ```

4. **Run happy-path verify** — `tools/phase-c-verify.sh <silo-id>`

   The script expects a pre-existing silo id (no `tcadm silo create`
   in the current CLI — the bootstrap silo is created at cluster
   init and lives in `/opt/triton/etc/tritond-bootstrap.toml` or
   the FDB cluster-init record).

   What it verifies:
   - Creates a tenant via `tcadm tenant create <silo-id> --name`.
   - Asserts the returned Tenant has `storage_workspace_id` and
     `storage_cluster_id` both populated.
   - Cross-checks `mantad-adm workspace list` shows the
     `t-<simple-uuid>` workspace.
   - Deletes the tenant via `tcadm tenant delete <silo-id> <id>`.
   - Cross-checks the workspace is gone from mantad afterward.
   - Lists last 20 audit events for the paired success entries.

   Bucket-op verification through the forwarder is **deliberately
   out of scope** for this script — tcadm does not yet expose
   bucket subcommands and bucket-level workspace isolation is
   the next slice (`beads-cw2u`). The Tenant↔Workspace contract
   is fully covered by what's above.

5. **Run failure-paths verify** — manually, per case.

   No script for these yet — each requires inducing a fault and
   eyeballing the response code. Capture each into
   `evidence/phase-c-ifby-<case>.log`.

   - **Cluster Unreachable at tenant create.** `pkill -x mantad`,
     `tcadm tenant create <silo-id> --name fail-1` — expect 503
     with `StorageClusterUnreachable` error code. Confirm no
     Tenant row was written: `tcadm tenant list <silo-id>` does
     not show `fail-1`.
   - **Idempotent retry.** Restart mantad. Block the admin port
     mid-create (`pfctl` rule blocking 7101 outbound from
     tritond's IP for ~10s) so the first RPC times out. Tritond
     should error 502; retry the same command and it should
     succeed — the second mantad call resolves the existing
     workspace by `tenant_uuid` idempotency. `mantad-adm
     workspace list` shows only one matching workspace.
   - **Unbound tenant.** `tcadm config set
     storage.default_s3_cluster_id ''` (reset by passing the
     null wire-shape), then `tcadm tenant create <silo-id>
     --name unbound-1`. The tenant succeeds without a binding
     (both columns NULL on the show). Bucket op via direct
     curl against `/v1/storage/clusters/{id}/buckets` for that
     tenant's principal returns 412 with
     `TenantStorageUnbound`.
   - **Non-empty delete.** Out of scope this iteration — needs
     bucket op support to *populate* a workspace before
     attempting delete. The 409 propagation is unit-tested
     locally via `cargo test`; live verify deferred to the
     bucket-ops slice.

6. **Capture transcripts**
   - Save the verify output to `evidence/phase-c-co7h-<date>.log`
     and `evidence/phase-c-ifby-<date>.log` (paths the close-out
     commit on the verify beads will reference).

## Rollback

If any step fails badly:

```sh
# On TARGET_HOST as root:
pkill -x tritond
pkill -x mantad

# Restore the previous tritond binary (you backed it up in step 2):
mv /opt/triton/bin/tritond.prev /opt/triton/bin/tritond
svcadm restart triton/tritond  # if SMF is wrapping it
# OR re-launch by hand if not.
```

Mantad's FDB subspace is separate from tritond's, so removing
mantad does not corrupt tritond's state. Mantad's local meta
(at `/var/mantad/meta`) is throwaway between test runs.

## Known gotchas

- **SmartOS curl CA bundle.** `tritond`'s self-update path uses
  Rust TLS via the OS trust store — set `SSL_CERT_FILE=/opt/tools/etc/openssl/certs/ca-certificates.crt`
  if you exercise update paths.
- **`tcadm` interactive auth.** The first `tcadm login` writes
  a session token to `~/.config/tcadm/session.toml`. The verify
  script assumes that has happened; run a one-shot `tcadm whoami`
  first if you're new to this box.
- **Mantad meta-plane choice.** The verify flows exercise
  workspace operations which are *only* implemented on the FDB
  backend. The deploy script forces `MANTAD_META_PLANE=fdb`.
  Running with `raft` (the default) will surface as 500s on
  every workspace call.
