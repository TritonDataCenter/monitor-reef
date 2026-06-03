// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `MemStore` driver for the [`identity_store`] conformance suite.
//!
//! Every `#[tokio::test]` below is a thin wrapper that constructs a fresh
//! `MemStore` and hands it to `common::check_*`. When `FdbStore` lands a
//! sibling `tests/fdb.rs` will run the *same* `check_*` functions — that's
//! the whole point of factoring them into `tests/common/mod.rs`. New
//! backend-agnostic tests go in `tests/common/mod.rs`, not here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use identity_store::MemStore;

macro_rules! conformance {
    ($($name:ident,)*) => {
        $(
            #[tokio::test]
            async fn $name() {
                common::$name(MemStore::new()).await;
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
