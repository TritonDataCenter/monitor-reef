// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! mdata-delete: Delete a metadata key.
//!
//! Usage: mdata-delete <keyname>
//!
//! Deleting a non-existent key is not considered an error.
//! Requires V2 protocol support from the metadata service.
//!
//! Exit codes:
//!   0 - Success (or key did not exist)
//!   2 - Error
//!   3 - Usage error

use mdata_client::protocol::Protocol;
use mdata_client::{Response, exit_code};

fn main() {
    mdata_client::init_logging();
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(exit_code::ERROR);
        }
    }
}

fn run() -> anyhow::Result<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!(
            "Usage: {} <keyname>",
            args.first().map(String::as_str).unwrap_or("mdata-delete"),
        );
        return Ok(exit_code::USAGE_ERROR);
    }

    let key = &args[1];
    let mut proto = Protocol::init()?;

    match proto.delete(key)? {
        // DELETE of non-existent key is not an error
        Response::Success(_) | Response::NotFound => Ok(exit_code::SUCCESS),
    }
}
