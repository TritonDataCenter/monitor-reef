// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example: Batch operations in Moray
//!
//! This example demonstrates how to execute batch operations atomically.
//! Note: Requires a running Moray server and a bucket named 'rust_test_bucket'.

use moray::buckets;
use moray::client::MorayClient;
use moray::objects::{self, BatchPutOp, BatchRequest, Etag};
use serde_json::json;
use slog::{Drain, Logger, o};
use std::collections::HashMap;
use std::f64::consts::{E, TAU};
use std::io::Error;

/// The golden ratio (φ) - not in std::f64::consts
const PHI: f64 = 1.618033988749895;
use std::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let ip_arr: [u8; 4] = [10, 77, 77, 9];
    let port: u16 = 2021;
    let opts = objects::MethodOptions::default();
    let bucket_opts = buckets::MethodOptions::default();
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );
    let mclient = MorayClient::from_parts(ip_arr, port, log, None)?;
    let bucket_name = "rust_test_bucket";
    let new_etag = String::from("");
    let mut correct_values = HashMap::new();

    correct_values.insert("eulers_number", E);
    correct_values.insert("golden_ratio", PHI);
    correct_values.insert("circle_constant", TAU);

    println!("===confirming bucket exists===");
    if let Err(e) = mclient
        .get_bucket(bucket_name, bucket_opts, |b| {
            dbg!(b);
            Ok(())
        })
        .await
    {
        eprintln!(
            "You must create a bucket named '{}' first. \
             Run the createbucket example to do so.",
            bucket_name
        );
        return Err(Error::other(e));
    }

    /* opts.etag defaults to undefined, and will clobber any existing value */
    println!("\n\n===undefined etag===");

    let put_ops: Vec<BatchPutOp> = vec![
        BatchPutOp {
            bucket: bucket_name.to_string(),
            options: opts.clone(),
            key: "circle_constant".to_string(),
            value: json!({"aNumber": TAU}),
        },
        BatchPutOp {
            bucket: bucket_name.into(),
            options: opts.clone(),
            key: "eulers_number".to_string(),
            value: json!({"aNumber": E}),
        },
        BatchPutOp {
            bucket: bucket_name.into(),
            options: opts.clone(),
            key: "golden_ratio".to_string(),
            value: json!({"aNumber": PHI}),
        },
    ];

    let mut requests = vec![];
    for req in put_ops.iter() {
        requests.push(BatchRequest::Put((*req).clone()));
    }

    mclient.batch(&requests, &opts, |_| Ok(())).await?;

    for req in put_ops.iter() {
        mclient
            .get_object(&req.bucket, &req.key, &opts, |o| {
                dbg!(o);
                Ok(())
            })
            .await
            .map_err(|e| Error::other(format!("get_object failed: {}", e)))?;
    }

    // Specify an incorrect etag for one of the operations and assert
    // the expected failure.
    println!("======= Specified incorrect etag =======");
    let mut bad_opts = opts.clone();
    bad_opts.etag = Etag::Specified(new_etag);

    let put_ops: Vec<BatchPutOp> = vec![
        BatchPutOp {
            bucket: bucket_name.to_string(),
            options: bad_opts,
            key: "circle_constant".to_string(),
            value: json!({"aNumber": 12.28}),
        },
        BatchPutOp {
            bucket: bucket_name.into(),
            options: opts.clone(),
            key: "eulers_number".to_string(),
            value: json!({"aNumber": 4.718}),
        },
        BatchPutOp {
            bucket: bucket_name.into(),
            options: opts.clone(),
            key: "golden_ratio".to_string(),
            value: json!({"aNumber": 2.618}),
        },
    ];

    let mut requests = vec![];

    for req in put_ops.iter() {
        requests.push(BatchRequest::Put((*req).clone()));
    }

    // Assert that specifying the wrong etag for even one of the operations
    // in the batch causes the entire call to fail.
    assert!(mclient.batch(&requests, &opts, |_| Ok(())).await.is_err());

    // Assert that if one of the operations fails the others are not executed.
    for req in put_ops.iter() {
        mclient
            .get_object(&req.bucket, &req.key, &opts, |o| {
                assert_eq!(
                    correct_values
                        .get(req.key.as_str())
                        .ok_or_else(|| Error::other("key not found"))?,
                    o.value
                        .get("aNumber")
                        .ok_or_else(|| Error::other("aNumber not found"))?
                );
                dbg!(o);
                Ok(())
            })
            .await
            .map_err(|e| Error::other(format!("get_object failed: {}", e)))?;
    }

    Ok(())
}
