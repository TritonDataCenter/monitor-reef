// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example: Finding objects in Moray
//!
//! This example demonstrates how to search for objects using LDAP-style
//! filters and retrieve individual objects by key.
//!
//! Note: This example requires a running Moray server at the configured
//! IP address and port.

use libmanta::moray as manta;
use moray::client::MorayClient;
use moray::objects;

use slog::{Drain, Logger, o};
use std::io::Error;
use std::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let ip_arr: [u8; 4] = [10, 77, 77, 15];
    let port: u16 = 2021;

    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let log = Logger::root(
        Mutex::new(slog_term::FullFormat::new(plain).build()).fuse(),
        o!("build-id" => "0.1.0"),
    );
    let mclient = MorayClient::from_parts(ip_arr, port, log, None)?;

    // Find first 10 objects of type "object" and capture the first one's details
    let mut opts = objects::MethodOptions::default();
    opts.set_limit(10);

    let mut key: String = String::new();
    let mut checksum: String = String::new();
    let mut oid: String = String::new();

    mclient
        .find_objects("manta", "(type=object)", &opts, |o| {
            if o.bucket != "manta" {
                return Err(Error::other(format!(
                    "Unknown bucket type {}",
                    &o.bucket
                )));
            }
            let mobj: manta::MantaObject =
                serde_json::from_value(o.value.clone()).map_err(|e| {
                    Error::other(format!("Failed to parse: {}", e))
                })?;
            assert_eq!(mobj.obj_type, String::from("object"));
            dbg!(&mobj.name);
            if key.is_empty() {
                key = mobj.key.clone();
                checksum = mobj.content_md5.clone();
                oid = mobj.object_id.clone();
            }
            Ok(())
        })
        .await?;

    // Retrieve that object by key and verify the checksum
    let checksum_expected = checksum.clone();
    let opts = objects::MethodOptions::default();

    mclient
        .get_object("manta", key.as_str(), &opts, |o| {
            if o.bucket != "manta" {
                return Err(Error::other(format!(
                    "Unknown bucket type {}",
                    &o.bucket
                )));
            }
            let manta_obj: manta::ObjectType =
                serde_json::from_value(o.value.clone()).map_err(|e| {
                    Error::other(format!("Failed to parse: {}", e))
                })?;
            if let manta::ObjectType::Object(mobj) = manta_obj {
                println!("Found checksum:     {}", &mobj.content_md5);
                println!("Expected checksum:  {}", &checksum_expected);
                assert_eq!(mobj.content_md5, checksum_expected);
            }
            Ok(())
        })
        .await?;

    // Find object by objectId
    let checksum_expected = checksum.clone();
    let filter = format!("(objectId={})", oid);
    let mut count = 0;

    mclient
        .find_objects("manta", filter.as_str(), &opts, |o| {
            count += 1;
            assert_eq!(count, 1, "should only be one result");
            if o.bucket != "manta" {
                return Err(Error::other(format!(
                    "Unknown bucket type {}",
                    &o.bucket
                )));
            }
            let manta_obj: manta::ObjectType =
                serde_json::from_value(o.value.clone()).map_err(|e| {
                    Error::other(format!("Failed to parse: {}", e))
                })?;
            if let manta::ObjectType::Object(mobj) = manta_obj {
                println!("Found checksum:     {}", &mobj.content_md5);
                println!("Expected checksum:  {}", &checksum_expected);
                assert_eq!(mobj.content_md5, checksum_expected);
            }
            Ok(())
        })
        .await?;

    // Find directories
    let mut opts = objects::MethodOptions::default();
    opts.set_limit(10);

    mclient
        .find_objects("manta", "(type=directory)", &opts, |o| {
            if o.bucket != "manta" {
                return Err(Error::other(format!(
                    "Unknown bucket type {}",
                    &o.bucket
                )));
            }
            let manta_obj: manta::ObjectType =
                serde_json::from_value(o.value.clone()).map_err(|e| {
                    Error::other(format!("Failed to parse: {}", e))
                })?;
            match manta_obj {
                manta::ObjectType::Object(_) => {
                    panic!("Found object in directory query");
                }
                manta::ObjectType::Directory(mdir) => {
                    println!("Found directory: {}", mdir.key);
                }
            }
            Ok(())
        })
        .await
}
