// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! mdata-list: List all available metadata keys.
//!
//! Usage: mdata-list
//!
//! Exit codes:
//!   0 - Success (keys printed to stdout, one per line)
//!   2 - Error
//!   3 - Usage error

use mdata_client::protocol::Protocol;
use mdata_client::{Command, Response, exit_code};

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
    if args.len() != 1 {
        eprintln!(
            "Usage: {}",
            args.first().map(String::as_str).unwrap_or("mdata-list"),
        );
        return Ok(exit_code::USAGE_ERROR);
    }

    let mut proto = Protocol::init()?;

    match proto.execute(Command::Keys, None)? {
        Response::Success(Some(data)) => {
            print!("{data}");
            if !data.ends_with('\n') {
                println!();
            }
            Ok(exit_code::SUCCESS)
        }
        Response::Success(None) => Ok(exit_code::SUCCESS),
        Response::NotFound => {
            eprintln!("ERROR: unexpected NOTFOUND response for KEYS");
            Ok(exit_code::ERROR)
        }
    }
}
