// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CNAPI Client
//!
//! Progenitor-generated client for the Triton Compute Node API (CNAPI).

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/client.rs"));
}
pub use generated::*;
