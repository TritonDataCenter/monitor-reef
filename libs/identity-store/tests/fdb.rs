// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `FdbStore` driver for the [`identity_store`] conformance suite.
//!
//! Each `#[tokio::test]` is a thin wrapper that constructs a fresh
//! `FdbStore` (against a real cluster) and hands it to the *same*
//! `common::check_*` function `tests/mem.rs` runs against `MemStore` —
//! that's the whole point of factoring them into `tests/common/mod.rs`.
//!
//! Gated behind the `foundationdb` feature and marked `#[ignore]` because
//! the tests need a running FDB cluster reachable via the default cluster
//! file resolution (`FDB_CLUSTER_FILE` env or `/etc/foundationdb/fdb.cluster`).
//! Run with:
//!
//! ```sh
//! FDB_CLUSTER_FILE=/path/to/fdb.cluster \
//!   cargo test -p identity-store --features foundationdb --test fdb -- --ignored --test-threads=1
//! ```
//!
//! The conformance suite seeds realms with fixed issuer URLs / scopes, so
//! the tests are NOT safe to run concurrently against a shared keyspace.
//! Each test therefore wipes the `identity/` prefix on entry (the suite
//! owns the whole identity keyspace) and runs single-threaded.

#![cfg(feature = "foundationdb")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use identity_store::FdbStore;

/// Open the test cluster and clear the entire `identity/` keyspace so each
/// conformance run starts from a clean slate. Single-threaded execution is
/// required (`--test-threads=1`) since the suite uses fixed issuers/scopes.
async fn fresh_store() -> FdbStore {
    let store = FdbStore::open(None).expect("open FDB cluster from default cluster file");
    let db = store.database();
    // Clear `[identity/, identity0)` — `0` (0x30) is the byte after `/`
    // (0x2F), so this is the half-open range covering every identity key.
    db.run(|tr, _| async move {
        tr.clear_range(b"identity/", b"identity0");
        Ok(())
    })
    .await
    .expect("wipe identity keyspace");
    store
}

macro_rules! conformance {
    ($($name:ident,)*) => {
        $(
            #[tokio::test]
            #[ignore = "requires a running FoundationDB cluster"]
            async fn $name() {
                common::$name(fresh_store().await).await;
            }
        )*
    };
}

conformance! {
    check_realm_round_trip_and_seeded_ring,
    check_realm_uniqueness_rules,
    check_update_realm_settings_round_trip,
    check_user_round_trip_and_uniqueness,
    check_large_realm_list_drains_all_batches,
    check_connection_at_most_one_enabled,
    check_delete_user_clears_sessions,
    check_brokered_user_lookup,
    check_refresh_token_revocation,
    check_revoked_token_survives_sweep,
    check_group_membership_both_directions,
    check_role_assignment_cross_scope_rejection,
    check_role_assignment_round_trip_and_dup,
    check_oauth_client_round_trip_and_cn_binding,
    check_upstream_connection_and_mappings,
    check_signing_keys_ring,
    check_auth_code_and_broker_state_take_on_consumption,
    check_device_code_lookup_and_status_update,
    check_sweeper_boundary,
    check_delete_realm_blocked_by_each_child,
    check_multi_realm_list_isolation,
    check_key_encoding_hostile_inputs,
    check_concurrent_create_realm_exactly_one_wins,
    check_not_found_for_absent_ids,
    check_rotation_lock_expiry,
}
