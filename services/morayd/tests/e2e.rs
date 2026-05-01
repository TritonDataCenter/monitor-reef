// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

// Integration tests pay for clarity of assertions; unwrap/expect are
// idiomatic here even though workspace-level lints deny them elsewhere.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end test: start morayd listening on an ephemeral port backed by
//! MemStore, connect a client using the same `FastCodec`, and exercise the
//! core bucket+object RPC surface. Proves the wire encoding, dispatch, and
//! store interleave correctly.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use morayd::fast::{FastCodec, FastMessage, FastStatus};
use morayd::server;
use morayd::store::mem::MemStore;
use serde_json::json;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

/// Drive a single RPC: send one Data frame with `id`, then collect every
/// reply up to (and including) the End or Error frame.
async fn call(
    framed: &mut Framed<TcpStream, FastCodec>,
    id: u32,
    rpc: &str,
    args: serde_json::Value,
) -> Vec<FastMessage> {
    let req = FastMessage::data(id, rpc, args);
    framed.send(req).await.expect("send");

    let mut replies = Vec::new();
    while let Some(r) = framed.next().await {
        let m = r.expect("decode");
        let terminal = matches!(m.status, FastStatus::End | FastStatus::Error);
        replies.push(m);
        if terminal {
            break;
        }
    }
    replies
}

#[tokio::test(flavor = "multi_thread")]
async fn full_bucket_object_cycle() {
    let store = Arc::new(MemStore::new());
    let listen = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(listen).await.unwrap();
    let local = listener.local_addr().unwrap();
    drop(listener);

    // Kick off the server on the port we just picked.
    let store_cloned = store.clone();
    let _srv = tokio::spawn(async move {
        server::run(store_cloned, local).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sock = TcpStream::connect(local).await.expect("connect");
    let mut framed = Framed::new(sock, FastCodec);

    // ping — node-moray's client expects zero DATA frames, End only.
    let rs = call(&mut framed, 1, "ping", json!([{}])).await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].status, FastStatus::End);

    // createBucket (wire order: [name, cfg, opts])
    let rs = call(&mut framed, 2, "createBucket", json!(["users", {}, {}])).await;
    assert!(
        rs.last().unwrap().status == FastStatus::End,
        "createBucket got {:?}",
        rs
    );

    // getBucket (wire order: [opts, name]). Wire shape: index/pre/post/
    // options are JSON-encoded strings (node-moray parseBucketConfig quirk).
    let rs = call(&mut framed, 3, "getBucket", json!([{}, "users"])).await;
    let got = rs.iter().find(|r| r.status == FastStatus::Data).expect("data");
    assert_eq!(got.data.d[0]["name"], "users");
    assert!(got.data.d[0]["index"].is_string(), "index must be stringified");

    // putObject [bucket, key, value, opts]
    let rs = call(
        &mut framed,
        4,
        "putObject",
        json!(["users", "alice", {"email":"a@example.com"}, {}]),
    )
    .await;
    let data = rs.iter().find(|r| r.status == FastStatus::Data).expect("data");
    assert!(data.data.d[0]["etag"].is_string());
    assert!(data.data.d[0]["_id"].is_u64());

    // getObject [bucket, key, opts]
    let rs = call(&mut framed, 5, "getObject", json!(["users", "alice", {}])).await;
    let data = rs.iter().find(|r| r.status == FastStatus::Data).expect("data");
    assert_eq!(data.data.d[0]["value"]["email"], "a@example.com");
    assert_eq!(data.data.d[0]["bucket"], "users");

    // delObject [bucket, key, opts]
    let rs = call(&mut framed, 6, "delObject", json!(["users", "alice", {}])).await;
    assert_eq!(rs.last().unwrap().status, FastStatus::End);

    // getObject -> ObjectNotFoundError
    let rs = call(&mut framed, 7, "getObject", json!(["users", "alice", {}])).await;
    let err = rs.last().unwrap();
    assert_eq!(err.status, FastStatus::Error);
    assert_eq!(err.data.d["name"], "ObjectNotFoundError");

    // --- named triggers ---
    // Put a packages-style bucket with sdc-papi's canonical `timestamps`
    // pre-trigger and confirm the values are stamped server-side.
    let timestamps_fn = "function timestamps(req, callback) { \
                         var d = new Date().getTime(); \
                         if (!req.value.created_at) req.value.created_at = d; \
                         req.value.updated_at = d; \
                         return callback(); }";
    let create = call(
        &mut framed,
        10,
        "createBucket",
        json!(["packages", {"index": {}, "pre": [timestamps_fn], "post": []}, {}]),
    )
    .await;
    assert_eq!(create.last().unwrap().status, FastStatus::End);

    let rs = call(
        &mut framed,
        11,
        "putObject",
        json!(["packages", "p1", {"name": "small"}, {}]),
    )
    .await;
    assert!(rs.iter().any(|r| r.status == FastStatus::Data));

    let got = call(&mut framed, 12, "getObject", json!(["packages", "p1", {}])).await;
    let row = got.iter().find(|r| r.status == FastStatus::Data).expect("row");
    assert!(
        row.data.d[0]["value"]["created_at"].as_u64().is_some(),
        "timestamps trigger didn't stamp created_at: {}",
        row.data.d[0]["value"]
    );
    assert!(row.data.d[0]["value"]["updated_at"].as_u64().is_some());

    let _ = call(&mut framed, 13, "delObject", json!(["packages", "p1", {}])).await;
    let _ = call(&mut framed, 14, "delBucket", json!(["packages", {}])).await;

    // --- limit=0 means "use the default", not "literally zero rows" ---
    // PAPI and other Triton callers idiomatically pass `limit: opts.limit
    // || 0`, relying on upstream Postgres-moray's behaviour of treating
    // an explicit 0 as "no caller-supplied limit". A strict reading
    // returns nothing and silently breaks /packages, /vms, etc. — this
    // regression test pins the upstream-compatible semantics.
    let _ = call(&mut framed, 20, "createBucket", json!(["lim_test", {}, {}])).await;
    for i in 0u64..3 {
        let _ = call(
            &mut framed,
            21 + i as u32,
            "putObject",
            json!(["lim_test", format!("k{i}"), {"i": i}, {}]),
        )
        .await;
    }
    let rs = call(
        &mut framed,
        30,
        "findObjects",
        json!(["lim_test", "(_id>=0)", {"limit": 0}]),
    )
    .await;
    let data_rows: Vec<_> =
        rs.iter().filter(|r| r.status == FastStatus::Data).collect();
    assert_eq!(
        data_rows.len(),
        3,
        "limit=0 must mean default-limit (returned all 3 rows), got {}",
        data_rows.len()
    );
    let _ = call(&mut framed, 31, "delBucket", json!(["lim_test", {}])).await;

    // --- batch atomicity: a failing op must roll back prior ops ---
    // sdc-napi sends `[put, update, put]` shaped batches. Earlier
    // morayd applied each op in its own FDB transaction, so an error
    // in op 2 left op 1 committed — that pinned the caller's stale
    // etag and caused an infinite EtagConflict retry loop. The fix
    // wraps the whole batch in one transaction. We simulate the bug
    // shape with `put` then a deliberately-failing `put` (etag guard
    // mismatch) and verify the first op did NOT commit.
    let _ = call(
        &mut framed,
        40,
        "createBucket",
        json!(["atomic_test", {"index": {"v": {"type": "number"}}}, {}]),
    )
    .await;
    let _ = call(
        &mut framed,
        41,
        "putObject",
        json!(["atomic_test", "existing", {"v": 1}, {}]),
    )
    .await;
    // Snapshot the existing row's etag so we can reference an
    // intentionally-stale value below.
    let rs = call(&mut framed, 42, "getObject", json!(["atomic_test", "existing", {}])).await;
    let prior_etag = rs
        .iter()
        .find(|r| r.status == FastStatus::Data)
        .and_then(|d| d.data.d[0]["_etag"].as_str())
        .map(|s| s.to_string())
        .expect("etag");

    // Batch: op 1 puts a brand-new key "first". Op 2 tries to put
    // "existing" but with a stale etag guard ("xxxxxxxx") — that op
    // MUST fail. With the atomicity fix, op 1 must not have committed
    // either: getObject "first" should return ObjectNotFoundError.
    let rs = call(
        &mut framed,
        43,
        "batch",
        json!([
            [
                {"bucket": "atomic_test", "operation": "put", "key": "first",  "value": {"v": 100}},
                {"bucket": "atomic_test", "operation": "put", "key": "existing", "value": {"v": 2},
                 "options": {"etag": "deadbeefdeadbeef"}},
            ],
            {}
        ]),
    )
    .await;
    let err = rs.last().expect("frame");
    assert_eq!(
        err.status, FastStatus::Error,
        "batch with stale-etag op should fail"
    );
    assert_eq!(err.data.d["name"], "EtagConflictError");
    let _ = prior_etag;

    // Op 1 must NOT have left "first" behind.
    let rs = call(&mut framed, 44, "getObject", json!(["atomic_test", "first", {}])).await;
    let err = rs.last().expect("frame");
    assert_eq!(
        err.status,
        FastStatus::Error,
        "atomicity violated: \"first\" was created despite the batch failing"
    );
    assert_eq!(err.data.d["name"], "ObjectNotFoundError");

    // And "existing" must still have its original value (v=1, not v=2).
    let rs = call(&mut framed, 45, "getObject", json!(["atomic_test", "existing", {}])).await;
    let row = rs.iter().find(|r| r.status == FastStatus::Data).expect("data");
    assert_eq!(
        row.data.d[0]["value"]["v"],
        1,
        "atomicity violated: failed batch wrote partial state"
    );
    let _ = call(&mut framed, 46, "delObject", json!(["atomic_test", "existing", {}])).await;
    let _ = call(&mut framed, 47, "delBucket", json!(["atomic_test", {}])).await;

    // delBucket [name, opts]
    let rs = call(&mut framed, 8, "delBucket", json!(["users", {}])).await;
    assert_eq!(rs.last().unwrap().status, FastStatus::End);

    // getBucket -> BucketNotFoundError
    let rs = call(&mut framed, 9, "getBucket", json!([{}, "users"])).await;
    let err = rs.last().unwrap();
    assert_eq!(err.status, FastStatus::Error);
    assert_eq!(err.data.d["name"], "BucketNotFoundError");
}
