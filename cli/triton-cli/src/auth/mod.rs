// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Token storage + [`TokenProvider`] implementation for tritonapi profiles.
//!
//! The CLI stores access / refresh tokens per-profile in
//! `~/.triton/tokens/<profile>.json` (mode 0600). [`FileTokenProvider`]
//! reads those files, proactively refreshes access tokens when they are
//! close to expiry, and exposes a reactive refresh path via
//! [`triton_gateway_client::TokenProvider::on_unauthorized`].
//!
//! ## Why file-backed (not keyring)
//!
//! A file-backed store keeps the Phase 3 surface area small and works
//! the same everywhere (SmartOS GZ, macOS, Linux, headless CI). Because
//! [`TokenProvider`] is a trait, a future
//! keyring-backed implementation (e.g. `keyring-rs`, `secret-service`,
//! macOS `security-framework`) can drop in without touching any call
//! sites — login/logout/whoami + Phase 4's retry logic all go through
//! the trait, not the concrete [`FileTokenProvider`].
//!
//! ## Threat model
//!
//! Access tokens are short-lived (minutes); refresh tokens are
//! single-use. Both live on disk mode 0600, readable only by the
//! operator's user. We never log the token values, not even at TRACE
//! level. Atomic writes go through `<path>.new` + rename so a crashed
//! CLI can never leave a torn file.

pub mod jwt;
pub mod token_provider;
pub mod tokens;

#[allow(unused_imports)] // used by login/logout/whoami commands
pub use token_provider::FileTokenProvider;
#[allow(unused_imports)]
pub use tokens::StoredTokens;
