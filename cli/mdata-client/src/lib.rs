// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Rust implementation of the SmartOS metadata protocol client.
//!
//! This crate implements the V1/V2 metadata protocol used by SmartOS
//! zones and KVM guests to communicate with the host metadata service.
//! It supports communication over Unix domain sockets (zones) and
//! serial ports (KVM/HVM guests).

use std::fmt;

pub mod protocol;
pub mod transport;

/// Initialize tracing for mdata-client tools.
///
/// Enables debug output when `MDATA_DEBUG=1` is set, otherwise only
/// warnings. Output goes to stderr to avoid interfering with stdout
/// data (which callers may be parsing).
pub fn init_logging() {
    let filter = if std::env::var("MDATA_DEBUG").is_ok_and(|v| v == "1") {
        "mdata_client=debug"
    } else {
        "mdata_client=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();
}

/// Metadata protocol commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Get,
    Put,
    Delete,
    Keys,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Command::Get => "GET",
            Command::Put => "PUT",
            Command::Delete => "DELETE",
            Command::Keys => "KEYS",
        })
    }
}

/// Exit codes matching the original C mdata-client implementation.
pub mod exit_code {
    pub const SUCCESS: i32 = 0;
    pub const NOT_FOUND: i32 = 1;
    pub const ERROR: i32 = 2;
    pub const USAGE_ERROR: i32 = 3;
}

/// Response from a metadata operation.
#[derive(Clone, Debug, PartialEq)]
#[must_use]
pub enum Response {
    /// Operation succeeded, with optional data payload.
    Success(Option<String>),
    /// Key was not found.
    NotFound,
}
