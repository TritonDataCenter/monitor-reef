// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Tenant CRUD tests against a real FoundationDB cluster. Mirrors
//! the MemStore tenant test block in `mem.rs`.
//!
//! Marked `#[ignore]` because they require a running FDB cluster
//! reachable via the default cluster file resolution. Run with
//! `cargo test -p tritond-store --features foundationdb -- --ignored`.
//!
//! Each test mints fresh silo + tenant uuids so concurrent runs
//! against a shared cluster don't trip on each other; we do not
//! blow away the keyspace.
use super::*;

fn fdb_test_store() -> FdbStore {
    FdbStore::open(None).expect("open FDB cluster from default cluster file")
}

/// Drop a tenant row + indices we know about. Best-effort — the
/// row may have been deleted by the test itself.
async fn purge_tenant(store: &FdbStore, tenant_id: Uuid) {
    let by_id = keys::tenant_by_id_key(tenant_id);
    if let Ok(Some(bytes)) = store.read_bytes(&by_id).await
        && let Ok(t) = serde_json::from_slice::<Tenant>(&bytes)
    {
        let by_name = keys::tenant_by_silo_name_key(t.silo_id, &t.name);
        let in_silo = keys::tenant_in_silo_key(t.silo_id, t.id);
        let _ = store
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let by_name = by_name.clone();
                let in_silo = in_silo.clone();
                async move {
                    tr.clear(&by_id);
                    tr.clear(&by_name);
                    tr.clear(&in_silo);
                    Ok(())
                }
            })
            .await;
    }
}

/// Drop a silo row + by_name index, plus the default tenant
/// that was created atomically with the silo. Best-effort cleanup.
async fn purge_silo(store: &FdbStore, silo_id: Uuid) {
    let by_id = keys::silo_by_id_key(silo_id);
    if let Ok(Some(bytes)) = store.read_bytes(&by_id).await
        && let Ok(s) = serde_json::from_slice::<Silo>(&bytes)
    {
        // Clean up the default tenant first so the silo's
        // tenant_in_silo index also gets cleared.
        purge_tenant(store, s.default_tenant_id).await;

        let by_name = keys::silo_by_name_key(&s.name);
        let _ = store
            .db
            .run(|tr, _| {
                let by_id = by_id.clone();
                let by_name = by_name.clone();
                async move {
                    tr.clear(&by_id);
                    tr.clear(&by_name);
                    Ok(())
                }
            })
            .await;
    }
}

#[tokio::test]
#[ignore]
async fn tenant_round_trip() {
    let store = fdb_test_store();
    let silo = store
        .create_silo(NewSilo {
            name: format!("brand-{}", Uuid::new_v4()),
            description: None,
        })
        .await
        .expect("create silo");

    let t = store
        .create_tenant(
            silo.id,
            NewTenant {
                name: "acme".to_string(),
                description: Some("first customer".to_string()),
            },
        )
        .await
        .expect("create tenant");
    assert_eq!(t.silo_id, silo.id);
    assert_eq!(t.name, "acme");
    assert_eq!(t.description, "first customer");

    let fetched = store.get_tenant(t.id).await.expect("get tenant");
    assert_eq!(fetched, t);

    let listed = store
        .list_tenants_in_silo(silo.id)
        .await
        .expect("list tenants");
    assert!(listed.iter().any(|x| x.id == t.id));

    store.delete_tenant(t.id).await.expect("delete tenant");
    let err = store
        .get_tenant(t.id)
        .await
        .expect_err("post-delete get is not-found");
    assert!(matches!(err, StoreError::NotFound));

    purge_tenant(&store, t.id).await;
    purge_silo(&store, silo.id).await;
}

#[tokio::test]
#[ignore]
async fn tenants_within_silo_must_have_unique_names() {
    let store = fdb_test_store();
    let silo = store
        .create_silo(NewSilo {
            name: format!("brand-{}", Uuid::new_v4()),
            description: None,
        })
        .await
        .expect("create silo");

    let t = store
        .create_tenant(
            silo.id,
            NewTenant {
                name: "acme".to_string(),
                description: None,
            },
        )
        .await
        .expect("create first");
    let err = store
        .create_tenant(
            silo.id,
            NewTenant {
                name: "acme".to_string(),
                description: None,
            },
        )
        .await
        .expect_err("duplicate within silo conflicts");
    assert!(matches!(err, StoreError::Conflict(_)));

    purge_tenant(&store, t.id).await;
    purge_silo(&store, silo.id).await;
}

#[tokio::test]
#[ignore]
async fn same_tenant_name_in_different_silos_does_not_conflict() {
    let store = fdb_test_store();
    let a = store
        .create_silo(NewSilo {
            name: format!("brand-a-{}", Uuid::new_v4()),
            description: None,
        })
        .await
        .expect("create silo a");
    let b = store
        .create_silo(NewSilo {
            name: format!("brand-b-{}", Uuid::new_v4()),
            description: None,
        })
        .await
        .expect("create silo b");

    let t1 = store
        .create_tenant(
            a.id,
            NewTenant {
                name: "acme".to_string(),
                description: None,
            },
        )
        .await
        .expect("create in a");
    let t2 = store
        .create_tenant(
            b.id,
            NewTenant {
                name: "acme".to_string(),
                description: None,
            },
        )
        .await
        .expect("same name across silos must be allowed");

    purge_tenant(&store, t1.id).await;
    purge_tenant(&store, t2.id).await;
    purge_silo(&store, a.id).await;
    purge_silo(&store, b.id).await;
}

#[tokio::test]
#[ignore]
async fn list_tenants_in_unknown_silo_returns_not_found() {
    let store = fdb_test_store();
    let err = store
        .list_tenants_in_silo(Uuid::new_v4())
        .await
        .expect_err("unknown silo should be not-found");
    assert!(matches!(err, StoreError::NotFound));
}
