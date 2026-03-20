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

/// Metadata protocol commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Get,
    Put,
    Delete,
    Keys,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Get => write!(f, "GET"),
            Command::Put => write!(f, "PUT"),
            Command::Delete => write!(f, "DELETE"),
            Command::Keys => write!(f, "KEYS"),
        }
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
#[derive(Debug)]
#[must_use]
pub enum Response {
    /// Operation succeeded, with optional data payload.
    Success(Option<String>),
    /// Key was not found.
    NotFound,
}
