<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Kelp cluster storage

How `/v1/k8s/clusters/*` persists records, and the planned migration to
Moray. Companion to RFD-0192 §2 ("Architecture / Proposed") which
identifies Moray as the long-term store.

## Phase 1: file-backed JSON

`services/triton-api-server/src/cluster_store.rs` defines a
[`ClusterStore`] trait and one implementation, [`FileClusterStore`]:

- One JSON document per cluster at `<state_dir>/<cluster_uuid>.json`.
- Writes are atomic — temp file in the same directory, `sync_all`,
  rename. Same pattern as
  `cli/triton-cli/src/commands/login.rs::write_tokens`.
- `list_for_account` reads the directory and filters in-memory by
  `account_id`.
- A corrupt JSON file logs a warning and is skipped rather than
  failing the whole list call.
- `state_dir` is configurable via `[clusters]` section of the
  triton-api-server config; defaults to `./data/clusters` for local
  dev.

This is intentionally minimal — it exists so the endpoint surface is
real and exercised end-to-end while Moray modernization happens out
of band.

### Why not Moray today

`libs/moray` and its transitive dependencies (`libs/fast`,
`libs/cueball-*`) are commented out of the workspace because they are
Rust edition 2018, depend on `tokio 0.1`, and would need substantial
modernization before they compile against the workspace's edition
2024 / `tokio 1.48`. The eventual modernization is its own effort,
unrelated to the cluster-CRUD endpoints themselves. Hiding the store
behind the [`ClusterStore`] trait means swapping in a Moray-backed
implementation later does not touch the endpoint handlers or the API
contract.

## Phase 2: Moray-backed store

Sketch of what the swap will look like once Moray is usable.

- New crate or module: `services/triton-api-server/src/moray_store.rs`
  with `MorayClusterStore` implementing [`ClusterStore`].
- Bucket: `kelp_clusters`, schema:
  - key: `cluster_uuid` (the [`Cluster::id`] value)
  - value: full [`Cluster`] document
  - secondary index on `account_id` (string) so `list_for_account` can
    push the filter to the server side
  - optional secondary index on `state` for future "list all degraded
    clusters" admin queries
- `create` becomes `put_object` with `Etag::Nulled` (insert-only) so
  duplicate IDs surface as conflicts the same way the file store
  reports `AlreadyExists`.
- `delete` becomes a `Batch(BatchDelete)` request (the Moray client
  has no standalone `del_object`).
- The Moray client is synchronous and callback-based; calls from
  Dropshot handlers go through `tokio::task::block_in_place` to keep
  the runtime healthy while a Moray RPC is in flight.

`ApiContext::cluster_store` already holds `Arc<dyn ClusterStore>`, so
the only wiring change at the call sites is which concrete type the
service constructs at startup.

## Testing

- `cluster_store::tests` covers the file store directly:
  create/get/list/delete, duplicate rejection, missing-record
  handling, account filtering, atomic-write resilience.
- `k8s_helper_tests` (in `services/triton-api-server/src/main.rs`)
  covers the new HTTP-error mapping helpers (`cluster_not_found`,
  `store_error_to_http`).
- The `resolve_caller` helper for the cluster endpoints delegates to
  primitives covered in `libs/triton-auth` (HTTP-Signature) and
  `libs/triton-auth-session` (JWT verification).
- **Deferred**: full HTTP integration tests that drive a running
  Dropshot server with a real `ApiContext`. The existing
  triton-api-server has no integration tests today (only inline
  helper tests), and adding them cleanly requires splitting the
  binary into a library + thin binary so a test module can construct
  the API description without rebuilding it from scratch. That
  refactor is its own branch.

[`ClusterStore`]: ../../services/triton-api-server/src/cluster_store.rs
[`FileClusterStore`]: ../../services/triton-api-server/src/cluster_store.rs
[`Cluster`]: ../../apis/triton-api/src/types/k8s.rs
[`Cluster::id`]: ../../apis/triton-api/src/types/k8s.rs
