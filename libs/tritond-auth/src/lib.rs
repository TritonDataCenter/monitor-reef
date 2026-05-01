// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Operator-auth primitives for the Triton Cloud control plane.
//!
//! This crate is HTTP-free, storage-free, and policy-free; it deals
//! only in cryptographic primitives that the storage and service
//! layers compose. The shape is:
//!
//! * [`password`] — bcrypt hashing and verification, plus a helper
//!   for generating cryptographically-strong random passwords for
//!   the bootstrap operator.
//! * [`api_key`] — opaque bearer credentials with a `tcadm_…` prefix,
//!   shown once and stored as bcrypt hashes.
//! * [`jwt`] — HS256 mint and verify for short-lived access tokens
//!   and longer-lived refresh tokens. Single symmetric key per
//!   cluster, generated at bootstrap.
//!
//! When OIDC federation lands later, `jwt::verify` gains a per-silo
//! IdP path alongside the operator key; the primitives here don't
//! change.

pub mod api_key;
pub mod jwt;
pub mod password;

pub use api_key::{
    API_KEY_PREFIX, ApiKeyError, ApiKeyMaterial, generate_api_key, hash_api_key, verify_api_key,
};
pub use jwt::{
    AccessClaims, JwtError, JwtKey, RefreshClaims, TokenKind, mint_access, mint_refresh, verify,
};
pub use password::{PasswordError, generate_random_password, hash_password, verify_password};
