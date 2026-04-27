// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Moray-compatible key-value service.
//!
//! We implement the fast-protocol (node-fast) server-side and expose the
//! subset of the Moray RPC surface that Triton services actually use,
//! backed by FoundationDB.
//!
//! Architecture:
//!
//! ```text
//! Triton service (node-moray client)
//!     │  fast-protocol over TCP
//!     ▼
//! morayd (this crate)   ─── MorayStore trait ───▶  FdbStore (FDB tuple layer)
//!                                             └▶  MemStore  (dev / tests)
//! ```
//!
//! Entry point: [`crate::server::run`]. The CLI binary in `main.rs` is a
//! thin wrapper around it.

pub mod error;
pub mod fast;
pub mod filter;
pub mod rpc;
pub mod server;
pub mod store;
pub mod triggers;
pub mod types;
pub mod typeval;
pub mod validate;
pub mod wire;

pub use error::{MorayError, Result};
pub use store::MorayStore;
