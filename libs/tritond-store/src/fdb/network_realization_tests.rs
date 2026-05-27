// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! FDB-backed realization scan tests. Marked ignored because they
//! require a running FoundationDB cluster. Run with
//! `FDB_CLUSTER_FILE=/path/to/fdb.cluster cargo test -p tritond-store --features foundationdb empty_network_realization_scan_returns_empty_vec -- --ignored`.

use super::*;

#[tokio::test]
#[ignore]
async fn empty_network_realization_scan_returns_empty_vec() {
    let store = FdbStore::open(None).expect("open FDB cluster from default cluster file");
    let resource = NetworkResourceId::NatGateway { id: Uuid::new_v4() };

    let rows = store
        .list_network_realizations(resource)
        .await
        .expect("empty realization scan should succeed");

    assert!(rows.is_empty());
}
