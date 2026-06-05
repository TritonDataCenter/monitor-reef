// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NAPI type definitions
//!
//! Note: NAPI uses snake_case for all JSON field names (internal Triton API
//! convention). Do NOT use `#[serde(rename_all = "camelCase")]`.

pub mod aggregation;
pub mod common;
pub mod fabric;
pub mod ip;
pub mod manage;
pub mod network;
pub mod nic;
pub mod nic_tag;
pub mod ping;
pub mod pool;

pub use aggregation::*;
pub use common::*;
pub use fabric::*;
pub use ip::*;
pub use manage::*;
pub use network::*;
pub use nic::*;
pub use nic_tag::*;
pub use ping::*;
pub use pool::*;
