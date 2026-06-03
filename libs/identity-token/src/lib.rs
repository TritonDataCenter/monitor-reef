// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Self-contained verifier for identityd-issued access tokens.
//!
//! This crate has no `identity-store`, `tritond`, or FoundationDB
//! dependency: the access-token claims are denormalized (RFD 00004
//! "Token shape"), so a verifier reconstructs a principal with no store
//! round-trip. `tritond`, the storage tier, and the Workbench BFF all
//! link this one crate to turn a bearer token into trusted claims.

pub mod claims;
pub mod error;
pub mod jwks;
pub mod verifier;

pub use claims::{AccessClaims, Confirmation, RealmScope};
pub use error::TokenError;
pub use jwks::{JwksError, JwksSource, PollingJwksSource, StaticJwksSource};
pub use verifier::{Verifier, VerifierOptions, peek_issuer};
