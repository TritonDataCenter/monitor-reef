// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example: Raw SQL queries in Moray
//!
//! This example demonstrates how to execute raw SQL queries against Moray.
//! Note: Requires a running Moray server with a 'manta' table.

use moray::client::MorayClient;
use serde_json::{Map, Value};
use slog::{Drain, Logger, o};
use std::io::Error;
use std::sync::Mutex;

fn query_handler(resp: &Value) -> Result<(), Error> {
    dbg!(&resp);
    Ok(())
}

async fn query_client_string_opts(
    ip: [u8; 4],
    port: u16,
    log: Logger,
) -> Result<(), Error> {
    let mclient = MorayClient::from_parts(ip, port, log, None)?;

    // The sql interface does not take 'limit' in opts
    let query = "SELECT * FROM manta limit 10";

    mclient.sql(query, vec![], r#"{}"#, query_handler).await
}

async fn query_client_map_opts(
    ip: [u8; 4],
    port: u16,
    log: Logger,
) -> Result<(), Error> {
    let mclient = MorayClient::from_parts(ip, port, log, None)?;

    // The sql interface does not take 'limit' in opts
    let query = "SELECT * FROM manta limit 10";
    let map = Map::new();

    mclient.sql(query, vec![], map, query_handler).await
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );

    let ip_arr: [u8; 4] = [10, 77, 77, 9];
    let port: u16 = 2021;

    println!("Testing SQL method");
    query_client_string_opts(ip_arr, port, log.clone()).await?;
    query_client_map_opts(ip_arr, port, log.clone()).await?;
    Ok(())
}
