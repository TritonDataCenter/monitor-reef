/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

//! Mock Fast RPC mdapi server for integration tests.
//!
//! Provides an in-process TCP server that speaks the Fast RPC protocol
//! and returns canned responses for mdapi methods. This enables
//! integration tests to exercise the full RPC codepath through
//! `MdapiClient` → TCP → Fast RPC protocol → mock handler without
//! requiring a deployed mdapi instance.

use std::io::{Error, ErrorKind};
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Barrier};
use std::thread;

use fast_rpc::protocol::{FastMessage, FastMessageData};
use fast_rpc::server;
use serde_json::{json, Value};
use slog::{o, Logger};
use tokio::net::TcpListener;
use tokio::prelude::*;
use uuid::Uuid;

/// Test owner UUID used in canned responses.
pub const TEST_OWNER: &str = "550e8400-e29b-41d4-a716-446655440000";

/// Test bucket UUID used in canned responses.
pub const TEST_BUCKET_ID: &str = "660e8400-e29b-41d4-a716-446655440001";

/// Test object UUID (first object) used in canned responses.
pub const TEST_OBJECT_ID: &str = "770e8400-e29b-41d4-a716-446655440002";

pub fn test_owner_uuid() -> Uuid {
    Uuid::parse_str(TEST_OWNER).unwrap()
}

pub fn test_bucket_uuid() -> Uuid {
    Uuid::parse_str(TEST_BUCKET_ID).unwrap()
}

/// Build a canned ObjectPayload JSON value for the given index.
///
/// The shape matches `libmanta::mdapi::ObjectPayload` so that
/// `serde_json::from_value::<ObjectPayload>(v)` succeeds.
fn make_test_object(index: usize) -> Value {
    json!({
        "owner": TEST_OWNER,
        "bucket_id": TEST_BUCKET_ID,
        "name": format!("test-object-{}", index),
        "id": format!(
            "770e8400-e29b-41d4-a716-44665544{:04}",
            index + 2
        ),
        "vnode": index as u64,
        "content_length": 1024 + index as u64,
        "content_md5": "rL0Y20zC+Fzt72VPzMSk2A==",
        "content_type": "application/octet-stream",
        "headers": {
            "content-type": "application/octet-stream"
        },
        "sharks": [
            {
                "datacenter": "us-east-1",
                "manta_storage_id": "1.stor.test.com"
            },
            {
                "datacenter": "us-west-1",
                "manta_storage_id": "2.stor.test.com"
            }
        ],
        "properties": null,
        "request_id": Uuid::new_v4().to_string(),
        "conditions": {
            "if-match": [format!("etag-{}", index)]
        }
    })
}

/// Build a canned Bucket JSON value.
fn make_test_bucket() -> Value {
    json!({
        "id": TEST_BUCKET_ID,
        "owner": TEST_OWNER,
        "name": "test-bucket",
        "created": "2025-01-15T00:00:00.000Z"
    })
}

/// Fast RPC dispatch handler for the mock mdapi server.
///
/// Routes by `msg.data.m.name` (the RPC method name) and returns
/// canned responses matching the real buckets-mdapi server format.
///
/// For single-result RPCs (get, update, listvnodes, listowners):
///   One FastMessage with `d = [<response_value>]`.
///   Client `call()` reads `msg.data.d.get(0)`.
///
/// For list RPCs (listbuckets, listobjects):
///   One FastMessage *per row*, each with `d = [<row_value>]`.
///   Client `call_multi()` collects `d.get(0)` from each message.
fn mock_mdapi_handler(
    msg: &FastMessage,
    _log: &Logger,
) -> Result<Vec<FastMessage>, Error> {
    let method = msg.data.m.name.clone();

    match method.as_str() {
        "listobjects" => {
            // Check bucket_id from request payload.
            // Client sends d = [payload] where payload has bucket_id.
            let bucket_id = msg
                .data
                .d
                .get(0)
                .and_then(|p| p.get("bucket_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if bucket_id == TEST_BUCKET_ID {
                // One FastMessage per object row, matching real server
                let msgs: Vec<FastMessage> = (0..3)
                    .map(|i| {
                        let data = FastMessageData::new(
                            method.clone(),
                            json!([make_test_object(i)]),
                        );
                        FastMessage::data(msg.id, data)
                    })
                    .collect();
                Ok(msgs)
            } else {
                // Empty result — no messages
                Ok(vec![])
            }
        }

        "updateobject" => {
            let data =
                FastMessageData::new(method, json!([{"etag": "new-etag"}]));
            Ok(vec![FastMessage::data(msg.id, data)])
        }

        "listvnodes" => {
            let data = FastMessageData::new(
                method,
                json!([{"vnodes": [0, 1, 2]}]),
            );
            Ok(vec![FastMessage::data(msg.id, data)])
        }

        "listbuckets" => {
            // One FastMessage per bucket row, matching real server
            let data = FastMessageData::new(
                method,
                json!([make_test_bucket()]),
            );
            Ok(vec![FastMessage::data(msg.id, data)])
        }

        "batchupdateobjects" => {
            let data = FastMessageData::new(
                method,
                json!([{"failed_vnodes": []}]),
            );
            Ok(vec![FastMessage::data(msg.id, data)])
        }

        _ => Err(Error::new(
            ErrorKind::Other,
            format!("Unsupported function: {}", method),
        )),
    }
}

/// An in-process mock Fast RPC mdapi server.
///
/// Binds to `127.0.0.1:0` (OS-assigned port) and spawns a background
/// thread running a tokio 0.1 runtime that accepts Fast RPC connections.
///
/// The server thread is intentionally leaked on drop — it exits when
/// the test process terminates.
pub struct MockMdapiServer {
    port: u16,
}

impl MockMdapiServer {
    /// Start a new mock mdapi server on an OS-assigned port.
    ///
    /// Blocks until the server's tokio runtime is initialized and
    /// ready to accept connections.
    pub fn start() -> Self {
        // Bind on the calling thread so we know the port immediately.
        let std_listener = StdTcpListener::bind("127.0.0.1:0")
            .expect("failed to bind mock mdapi listener");
        let port = std_listener.local_addr().unwrap().port();
        std_listener
            .set_nonblocking(true)
            .expect("failed to set non-blocking");

        // Barrier ensures the tokio runtime is running before we return.
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();

        thread::spawn(move || {
            tokio::run(futures::lazy(move || {
                let listener = TcpListener::from_std(
                    std_listener,
                    &tokio::reactor::Handle::default(),
                )
                .expect("failed to convert to tokio TcpListener");

                let log = Logger::root(slog::Discard, o!());

                // Signal that the runtime is ready.
                barrier_clone.wait();

                // Accept loop — runs until the process exits.
                listener
                    .incoming()
                    .map_err(|_| ())
                    .for_each(move |socket| {
                        let task = server::make_task(
                            socket,
                            mock_mdapi_handler,
                            Some(&log),
                        );
                        tokio::spawn(task);
                        Ok(())
                    })
            }));
        });

        barrier.wait();

        MockMdapiServer { port }
    }

    /// Returns the `"127.0.0.1:{port}"` endpoint string for
    /// constructing an `MdapiClient`.
    pub fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}
