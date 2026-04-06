// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SAPI type definitions
//!
//! Note: SAPI uses snake_case for all JSON field names (except PingResponse
//! which has some camelCase fields). No blanket `rename_all` is needed since
//! Rust field names are already snake_case.

pub mod application;
pub mod common;
pub mod instance;
pub mod manifest;
pub mod ops;
pub mod service;

pub use application::*;
pub use common::*;
pub use instance::*;
pub use manifest::*;
pub use ops::*;
pub use service::*;
