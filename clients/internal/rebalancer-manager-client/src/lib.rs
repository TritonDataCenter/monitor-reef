// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Rebalancer Manager API Client
//!
//! A Progenitor-generated client library for the Rebalancer Manager API.
//! Provides a type-safe, async interface for managing rebalancer jobs.

// Include the Progenitor-generated client code
// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/client.rs"));
}
pub use generated::*;
