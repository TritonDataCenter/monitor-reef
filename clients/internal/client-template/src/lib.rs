// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

//! API Client Template
//!
//! This is a Progenitor-generated client library template.
//! When you use this template:
//! 1. Copy to clients/internal/your-service-client
//! 2. Update Cargo.toml with your service name
//! 3. Register the client in client-generator/src/main.rs
//! 4. Run: make clients-generate
//!
//! The generated client provides a type-safe, async interface to your API.

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;
