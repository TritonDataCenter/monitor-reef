#![allow(clippy::unwrap_used, clippy::expect_used)]
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RFD 00004 D-Sg-8 test: a SagaContext built for SEC A returns
//! `FencedOut` from `verify_fence()` after SEC B reassigns the saga.
//!
//! This is the unit-level proof that fence enforcement works against
//! the SecStore. Catalog actions wrap their side effects with
//! `verify_fence` so a stale-but-not-yet-known-stale SEC's action
//! body returns immediately instead of landing a double-write.

use std::sync::Arc;

use chrono::Utc;
use slog::{Drain, o};
use tritond_saga::{
    MemSecStore, SagaContext, SagaError, SagaId, SecEpoch, SecHeartbeat, SecId, TritondSecStore,
};

fn null_logger() -> slog::Logger {
    let drain = slog::Discard;
    slog::Logger::root(drain.fuse(), o!())
}

#[tokio::test]
async fn verify_fence_passes_when_owner_matches() {
    let store = MemSecStore::new();
    let saga_id = SagaId(uuid::Uuid::new_v4());
    let sec_a = SecId::random();

    // Set up the saga record: SEC A is the current owner at epoch 1.
    store
        .stamp_create(saga_id, "test-saga", 1, sec_a, SecEpoch::new(1), &[])
        .await
        .unwrap();

    let trit_store: Arc<dyn TritondSecStore> = store.clone();
    let ctx = SagaContext::new(sec_a, SecEpoch::new(1), null_logger())
        .with_saga_id(saga_id)
        .with_sec_store(trit_store);

    ctx.verify_fence()
        .await
        .expect("fence must hold when (sec, epoch) match the current owner");
}

#[tokio::test]
async fn verify_fence_fails_after_reassignment() {
    let store = MemSecStore::new();
    let saga_id = SagaId(uuid::Uuid::new_v4());
    let sec_a = SecId::random();
    let sec_b = SecId::random();

    // SEC A creates the saga at epoch 1.
    store
        .stamp_create(saga_id, "test-saga", 1, sec_a, SecEpoch::new(1), &[])
        .await
        .unwrap();

    // SEC A's heartbeat goes stale.
    store
        .touch_sec(SecHeartbeat {
            sec_id: sec_a,
            epoch: SecEpoch::new(1),
            // 1h ago
            at: Utc::now() - chrono::Duration::hours(1),
        })
        .await
        .unwrap();

    // SEC B's sweeper reassigns A's sagas. The epoch bumps to 2.
    let stale: Vec<SecId> = store
        .stale_secs(Utc::now() - chrono::Duration::minutes(5))
        .await
        .unwrap();
    assert!(stale.contains(&sec_a));
    let moved = store.reassign_sagas(&stale, sec_b).await.unwrap();
    assert_eq!(
        moved.len(),
        1,
        "exactly one saga should have been reassigned"
    );

    // SEC A's old SagaContext still thinks it's at (sec_a, epoch=1).
    // It calls verify_fence before its next side effect:
    let trit_store: Arc<dyn TritondSecStore> = store.clone();
    let stale_ctx = SagaContext::new(sec_a, SecEpoch::new(1), null_logger())
        .with_saga_id(saga_id)
        .with_sec_store(trit_store.clone());

    match stale_ctx.verify_fence().await {
        Err(SagaError::FencedOut {
            actual_sec,
            actual_epoch,
            ..
        }) => {
            assert_eq!(actual_epoch, 2, "epoch should have bumped on reassignment");
            assert_eq!(actual_sec, sec_b.to_string(), "owner should be SEC B");
        }
        other => panic!("expected FencedOut, got {other:?}"),
    }

    // SEC B's fresh context (built after adoption) passes the check.
    let new_ctx = SagaContext::new(sec_b, SecEpoch::new(2), null_logger())
        .with_saga_id(saga_id)
        .with_sec_store(trit_store);
    new_ctx
        .verify_fence()
        .await
        .expect("the adopting SEC's fresh context must pass the fence check");
}

#[tokio::test]
async fn verify_fence_is_noop_without_sec_store() {
    // The SG-0 trivial-test path: a SagaContext built without
    // sec_store / saga_id wiring (e.g. unit tests that just
    // exercise Steno's engine machinery) should treat verify_fence
    // as a no-op so the test surface doesn't have to construct a
    // real SecStore.
    let ctx = SagaContext::new(SecId::random(), SecEpoch::new(1), null_logger());
    ctx.verify_fence()
        .await
        .expect("no-op when fence not wired");
}
