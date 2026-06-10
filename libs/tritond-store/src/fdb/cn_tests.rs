// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CN registration / approval tests against a real FoundationDB
//! cluster. Mirrors the MemStore CN test block in `mem.rs`.
//!
//! Marked `#[ignore]` because they require a running FDB cluster
//! reachable via the default cluster file resolution. Run with
//! `cargo test -p tritond-store --features foundationdb -- --ignored`.
//!
//! Each test uses a per-test key prefix (via a random uuid in the
//! `server_uuid`) so concurrent runs don't trip on each other; we
//! do not blow away the keyspace.
use super::*;
use crate::AutoApproveWindow;

fn fdb_test_store() -> FdbStore {
    FdbStore::open(None).expect("open FDB cluster from default cluster file")
}

fn sysinfo_fixture() -> serde_json::Value {
    serde_json::json!({
        "UUID": "00000000-0000-0000-0000-000000000001",
        "Hostname": "test-cn",
    })
}

/// Drop every key the CN tests touch for `server_uuid`. Runs at
/// the start of each test so reruns against a stale FDB cluster
/// produce repeatable state.
async fn purge_cn(store: &FdbStore, server_uuid: Uuid) {
    // Read the record first to learn which claim/poll indices
    // need clearing; ignore any decode failure.
    let by_uuid = FdbStore::cn_by_uuid_key(server_uuid);
    if let Ok(Some(bytes)) = store.read_bytes(&by_uuid).await
        && let Ok(cn) = serde_json::from_slice::<Cn>(&bytes)
    {
        let _ = store
            .db
            .run(|tr, _| {
                let by_uuid = by_uuid.clone();
                let claim_key = cn.claim_code.as_deref().map(FdbStore::cn_by_claim_key);
                let poll_key = FdbStore::cn_by_poll_key(&cn.poll_token);
                let state_key = FdbStore::cn_by_state_key(cn.state, server_uuid);
                let pending_state_key = FdbStore::cn_by_state_key(CnState::Pending, server_uuid);
                let approved_state_key = FdbStore::cn_by_state_key(CnState::Approved, server_uuid);
                let disabled_state_key = FdbStore::cn_by_state_key(CnState::Disabled, server_uuid);
                async move {
                    tr.clear(&by_uuid);
                    if let Some(k) = claim_key.as_deref() {
                        tr.clear(k);
                    }
                    tr.clear(&poll_key);
                    tr.clear(&state_key);
                    // Belt-and-suspenders: clear all three state
                    // membership keys in case state was rewritten
                    // between the read above and this txn.
                    tr.clear(&pending_state_key);
                    tr.clear(&approved_state_key);
                    tr.clear(&disabled_state_key);
                    Ok(())
                }
            })
            .await;
    } else {
        // Best-effort clear of state membership rows even with no
        // cn record (lets stuck rows from prior runs go away).
        let _ = store
            .db
            .run(|tr, _| {
                let pending = FdbStore::cn_by_state_key(CnState::Pending, server_uuid);
                let approved = FdbStore::cn_by_state_key(CnState::Approved, server_uuid);
                let disabled = FdbStore::cn_by_state_key(CnState::Disabled, server_uuid);
                async move {
                    tr.clear(&pending);
                    tr.clear(&approved);
                    tr.clear(&disabled);
                    Ok(())
                }
            })
            .await;
    }
}

/// Clear the auto-approve singleton. Used by every auto-approve
/// test so leftover state from a previous run doesn't leak in.
async fn purge_window(store: &FdbStore) {
    let _ = store.close_auto_approve_window().await;
}

#[tokio::test]
#[ignore]
async fn settings_round_trip() {
    let store = fdb_test_store();
    // Reset to a known baseline first; another run may have left a
    // blob behind. (We never clear the keyspace wholesale.)
    store
        .put_settings(Settings::default())
        .await
        .expect("seed default settings");
    assert_eq!(
        store.get_settings().await.expect("get"),
        Settings::default()
    );

    let mut s = Settings::default();
    s.set(crate::ConfigKey::SweeperIntervalSecs, serde_json::json!(99))
        .unwrap();
    s.set(
        crate::ConfigKey::MetricsBackend,
        serde_json::json!("clickhouse"),
    )
    .unwrap();
    store.put_settings(s.clone()).await.expect("put");
    assert_eq!(store.get_settings().await.expect("get"), s);

    // Leave the singleton at defaults for the next run.
    store
        .put_settings(Settings::default())
        .await
        .expect("restore defaults");
}

#[tokio::test]
#[ignore]
async fn register_cn_creates_pending_with_claim_code() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    let cn = store
        .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register_cn");
    assert_eq!(cn.state, CnState::Pending);
    assert!(cn.claim_code.is_some());
    assert_eq!(cn.claim_code.as_ref().expect("claim").len(), 6);
    assert_eq!(cn.poll_token.len(), 32);
    assert!(cn.bound_api_key_id.is_none());
    assert!(cn.pending_credential.is_none());
    assert!(cn.approved_at.is_none());
    assert_eq!(cn.role, CnRole::Tenant);

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn re_register_pending_rotates_claim_code() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    let first = store
        .register_cn(id, "host1".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register first");
    let second = store
        .register_cn(id, "host1-renamed".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register second");
    assert_eq!(first.registered_at, second.registered_at);
    assert_ne!(first.claim_code, second.claim_code);
    assert_ne!(first.poll_token, second.poll_token);
    assert_eq!(second.hostname, "host1-renamed");

    let err = store
        .get_cn_by_claim_code(first.claim_code.as_ref().expect("claim"))
        .await
        .expect_err("old claim should be unfindable");
    assert!(matches!(err, StoreError::NotFound));

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn re_register_approved_is_idempotent() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    store
        .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register");
    store
        .approve_cn(
            id,
            Uuid::new_v4(),
            "tcadm_xxx".into(),
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            now,
        )
        .await
        .expect("approve");

    let later = now + chrono::Duration::seconds(60);
    let updated = store
        .register_cn(
            id,
            "h2".into(),
            None,
            serde_json::json!({"updated": true}),
            later,
        )
        .await
        .expect("re-register");
    assert_eq!(updated.state, CnState::Approved);
    assert_eq!(updated.hostname, "h2");
    assert_eq!(updated.last_seen, Some(later));
    assert_eq!(updated.sysinfo, serde_json::json!({"updated": true}));

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn set_cn_role_updates_registered_cn() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    store
        .register_cn(id, "edge-a".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register");

    let updated = store.set_cn_role(id, CnRole::Edge).await.expect("set role");
    assert_eq!(updated.role, CnRole::Edge);
    assert_eq!(store.get_cn(id).await.expect("get").role, CnRole::Edge);

    let refreshed = store
        .register_cn(id, "edge-a-renamed".into(), None, sysinfo_fixture(), now)
        .await
        .expect("re-register");
    assert_eq!(refreshed.role, CnRole::Edge);

    let err = store
        .set_cn_role(Uuid::new_v4(), CnRole::Both)
        .await
        .expect_err("unknown cn");
    assert!(matches!(err, StoreError::NotFound));

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn approve_cn_flips_state_and_stashes_credential() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    let cn = store
        .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register");
    let key_id = Uuid::new_v4();
    let approved = store
        .approve_cn(
            id,
            key_id,
            "tcadm_secret".into(),
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            now,
        )
        .await
        .expect("approve");
    assert_eq!(approved.state, CnState::Approved);
    assert!(approved.claim_code.is_none());
    assert_eq!(approved.bound_api_key_id, Some(key_id));
    assert_eq!(approved.pending_credential.as_deref(), Some("tcadm_secret"));

    let err = store
        .get_cn_by_claim_code(cn.claim_code.as_ref().expect("claim"))
        .await
        .expect_err("old claim should be unfindable");
    assert!(matches!(err, StoreError::NotFound));

    let consumed = store
        .consume_cn_pending_credential(&cn.poll_token)
        .await
        .expect("consume first");
    assert_eq!(consumed.as_deref(), Some("tcadm_secret"));

    let consumed_again = store
        .consume_cn_pending_credential(&cn.poll_token)
        .await
        .expect("consume second");
    assert!(consumed_again.is_none());

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn approve_cn_pending_only() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;

    let err = store
        .approve_cn(
            id,
            Uuid::new_v4(),
            "x".into(),
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            Utc::now(),
        )
        .await
        .expect_err("approve before register should fail");
    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
#[ignore]
async fn disabled_cn_re_registers_back_to_pending() {
    // Re-registration re-arms a Disabled CN to Pending (fresh
    // claim code, bound credential cleared), awaiting re-approval.
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let now = Utc::now();
    store
        .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register");
    store.disable_cn(id).await.expect("disable");
    let re = store
        .register_cn(id, "h".into(), None, sysinfo_fixture(), now)
        .await
        .expect("re-register after disable should re-arm to Pending");
    assert_eq!(re.state, CnState::Pending);
    assert!(re.claim_code.is_some());
    assert!(re.bound_api_key_id.is_none());

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn auto_approve_window_promotes_registration() {
    let store = fdb_test_store();
    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();
    purge_cn(&store, id1).await;
    purge_cn(&store, id2).await;
    purge_cn(&store, id3).await;
    purge_window(&store).await;

    let now = Utc::now();
    store
        .open_auto_approve_window(AutoApproveWindow {
            opened_at: now,
            expires_at: now + chrono::Duration::minutes(30),
            remaining_count: Some(2),
            opened_by: "root".into(),
        })
        .await
        .expect("open window");

    let cn1 = store
        .register_cn(id1, "h1".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register cn1");
    assert_eq!(cn1.state, CnState::Approved);
    assert!(cn1.claim_code.is_none());
    assert!(cn1.approved_at.is_some());

    let cn2 = store
        .register_cn(id2, "h2".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register cn2");
    assert_eq!(cn2.state, CnState::Approved);

    let cn3 = store
        .register_cn(id3, "h3".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register cn3");
    assert_eq!(cn3.state, CnState::Pending);
    assert!(cn3.claim_code.is_some());

    assert!(
        store
            .get_auto_approve_window()
            .await
            .expect("get window")
            .is_none()
    );

    purge_cn(&store, id1).await;
    purge_cn(&store, id2).await;
    purge_cn(&store, id3).await;
}

#[tokio::test]
#[ignore]
async fn auto_approve_window_expires_on_time() {
    let store = fdb_test_store();
    let id = Uuid::new_v4();
    purge_cn(&store, id).await;
    purge_window(&store).await;

    let opened = Utc::now();
    store
        .open_auto_approve_window(AutoApproveWindow {
            opened_at: opened,
            expires_at: opened + chrono::Duration::seconds(10),
            remaining_count: None,
            opened_by: "root".into(),
        })
        .await
        .expect("open window");

    let later = opened + chrono::Duration::seconds(20);
    let cn = store
        .register_cn(id, "h".into(), None, sysinfo_fixture(), later)
        .await
        .expect("register");
    assert_eq!(cn.state, CnState::Pending);
    assert!(
        store
            .get_auto_approve_window()
            .await
            .expect("get window")
            .is_none()
    );

    purge_cn(&store, id).await;
}

#[tokio::test]
#[ignore]
async fn list_cns_filters_by_state() {
    let store = fdb_test_store();
    let pid = Uuid::new_v4();
    let aid = Uuid::new_v4();
    purge_cn(&store, pid).await;
    purge_cn(&store, aid).await;
    purge_window(&store).await;

    let now = Utc::now();
    store
        .register_cn(pid, "p".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register pending");
    store
        .register_cn(aid, "a".into(), None, sysinfo_fixture(), now)
        .await
        .expect("register approved-target");
    store
        .approve_cn(
            aid,
            Uuid::new_v4(),
            "k".into(),
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            now,
        )
        .await
        .expect("approve");

    let pending = store
        .list_cns(Some(CnState::Pending))
        .await
        .expect("list pending");
    assert!(pending.iter().any(|c| c.server_uuid == pid));
    assert!(!pending.iter().any(|c| c.server_uuid == aid));

    let approved = store
        .list_cns(Some(CnState::Approved))
        .await
        .expect("list approved");
    assert!(approved.iter().any(|c| c.server_uuid == aid));
    assert!(!approved.iter().any(|c| c.server_uuid == pid));

    let all = store.list_cns(None).await.expect("list all");
    assert!(all.iter().any(|c| c.server_uuid == pid));
    assert!(all.iter().any(|c| c.server_uuid == aid));

    purge_cn(&store, pid).await;
    purge_cn(&store, aid).await;
}
