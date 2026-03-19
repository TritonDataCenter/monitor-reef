// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! mdata-put: Set the value of a metadata key.
//!
//! Usage: mdata-put <keyname> [<value>]
//!
//! If <value> is not provided, reads from stdin (only when stdin is
//! not a terminal).
//!
//! Requires V2 protocol support from the metadata service.
//!
//! Exit codes:
//!   0 - Success
//!   2 - Error
//!   3 - Usage error

use std::io::{IsTerminal, Read};

use mdata_client::protocol::Protocol;
use mdata_client::{Response, exit_code};

fn main() {
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
    let progname = args
        .first()
        .map(String::as_str)
        .unwrap_or("mdata-put");

    if args.len() < 2 || args.len() > 3 {
        eprintln!("Usage: {progname} <keyname> [<value>]");
        return Ok(exit_code::USAGE_ERROR);
    }

    let key = &args[1];

    // Get value from argument or stdin
    let value = if args.len() == 3 {
        args[2].clone()
    } else if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        eprintln!(
            "Usage: {progname} <keyname> [<value>]\n\
             ERROR: either specify value as argument or pipe via stdin"
        );
        return Ok(exit_code::USAGE_ERROR);
    };

    let mut proto = Protocol::init()?;

    match proto.put(key, &value)? {
        Response::Success(_) => Ok(exit_code::SUCCESS),
        Response::NotFound => {
            eprintln!("ERROR: unexpected NOTFOUND response for PUT");
            Ok(exit_code::ERROR)
        }
    }
}
