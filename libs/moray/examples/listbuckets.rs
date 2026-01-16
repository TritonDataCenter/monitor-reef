// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example: Listing buckets in Moray
//!
//! This example demonstrates the different ways to create a MorayClient
//! and list all buckets in a Moray service.
//! Note: Requires a running Moray server.

use moray::buckets;
use moray::client::MorayClient;

use slog::{Drain, Logger, o};
use std::io::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Mutex;

async fn client_fromstr(
    addr: &str,
    opts: buckets::MethodOptions,
) -> Result<(), Error> {
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );

    let mclient = MorayClient::from_str(addr, log, None)?;

    mclient
        .list_buckets(opts, |b| {
            dbg!(&b);
            Ok(())
        })
        .await
}

async fn client_sockaddr(
    sockaddr: SocketAddr,
    opts: buckets::MethodOptions,
    log: Logger,
) -> Result<(), Error> {
    let mclient = MorayClient::new(sockaddr, log, None)?;
    mclient
        .list_buckets(opts, |b| {
            dbg!(&b);
            Ok(())
        })
        .await
}

async fn client_fromparts(
    ip: [u8; 4],
    port: u16,
    opts: buckets::MethodOptions,
    log: Logger,
) -> Result<(), Error> {
    let mclient = MorayClient::from_parts(ip, port, log, None)?;
    mclient
        .list_buckets(opts, |b| {
            dbg!(&b);
            Ok(())
        })
        .await
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );

    // Configure your Moray server address here
    let ip_arr: [u8; 4] = [10, 77, 77, 9];
    let port: u16 = 2021;
    let addr = format!("{}:{}", std::net::Ipv4Addr::from(ip_arr), port);

    let opts = buckets::MethodOptions::default();

    println!("MorayClient from_str");
    client_fromstr(addr.as_str(), opts.clone()).await?;

    println!("MorayClient SocketAddr");
    let sockaddr = SocketAddr::from_str(addr.as_str())
        .map_err(|e| Error::other(format!("Failed to parse address: {}", e)))?;
    client_sockaddr(sockaddr, opts.clone(), log.clone()).await?;

    println!("MorayClient from_parts");
    client_fromparts(ip_arr, port, opts.clone(), log.clone()).await?;

    Ok(())
}
