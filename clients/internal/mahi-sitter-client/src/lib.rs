// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Mahi Sitter Client Library
//!
//! This client provides typed access to the Mahi sitter (replicator admin)
//! service. The sitter runs on its own port alongside the Mahi replicator
//! process and exposes two endpoints:
//!
//! - `GET /ping` — 204 on success; 500/503 if the replicator is not caught up.
//! - `GET /snapshot` — streams the Redis `dump.rdb` snapshot as
//!   `application/octet-stream` with status 201.
//!
//! ## Usage
//!
//! ```ignore
//! use mahi_sitter_client::Client;
//!
//! let client = Client::new("http://mahi-sitter.my-dc.my-cloud.local");
//!
//! // Health check
//! client.sitter_ping().send().await?;
//!
//! // Snapshot endpoint returns a byte stream (binary RDB body)
//! let response = client.sitter_snapshot().send().await?;
//! ```
//!
//! ## Endpoints with special handling
//!
//! `sitter_snapshot` returns `ResponseValue<ByteStream>` (Progenitor's
//! representation of a binary streaming response). Callers must collect the
//! stream themselves (for example with `futures::TryStreamExt::try_next`).

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;
