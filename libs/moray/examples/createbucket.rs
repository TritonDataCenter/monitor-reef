// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example: Creating a bucket in Moray
//!
//! This example demonstrates how to create a new bucket with an index.
//! Note: Requires a running Moray server.

use moray::buckets;
use moray::client::MorayClient;
use serde_json::json;
use slog::{Drain, Logger, o};
use std::io::Error;
use std::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let ip_arr: [u8; 4] = [10, 77, 77, 9];
    let port: u16 = 2021;
    let opts = buckets::MethodOptions::default();

    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );

    let mclient = MorayClient::from_parts(ip_arr, port, log, None)?;
    let bucket_config = json!({
        "index": {
            "aNumber": {
                "type": "number"
            }
        }
    });

    match mclient
        .create_bucket("rust_test_bucket", bucket_config, opts)
        .await
    {
        Ok(()) => {
            println!("Bucket Created Successfully");
            Ok(())
        }
        Err(e) => {
            eprintln!("Error Creating Bucket");
            Err(e)
        }
    }
}
