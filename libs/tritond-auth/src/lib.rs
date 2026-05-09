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
//! * [`password`] — bcrypt hashing and verification (each on the
//!   blocking pool so async callers don't stall a Tokio worker), plus
//!   a helper for generating cryptographically-strong random
//!   passwords for the bootstrap operator.
//! * [`api_key`] — opaque bearer credentials with a `tcadm_…` prefix
//!   split into a non-secret 12-character lookup id and a 32-character
//!   secret payload. Lookup id is indexed plaintext; secret is stored
//!   only as a bcrypt hash.
//! * [`jwt`] — HS256 mint and verify for short-lived access tokens
//!   and longer-lived refresh tokens. Single symmetric key per
//!   cluster, generated at bootstrap, zeroed on drop.
//! * [`redacted`] — `RedactedString` newtype for credential fields
//!   that must not leak via `Debug` or coredump.

pub mod api_key;
pub mod identity_hmac;
pub mod jwt;
pub mod oidc;
pub mod password;
pub mod redacted;

pub use api_key::{
    API_KEY_PREFIX, ApiKeyError, ApiKeyMaterial, LOOKUP_ID_CHARS, SECRET_CHARS, generate_api_key,
    parse_api_key, verify_api_key_secret,
};
pub use identity_hmac::{IDENTITY_HMAC_KEY_BYTES, IdentityHmacKey};
pub use jwt::{
    AccessClaims, JwtError, JwtKey, RefreshClaims, TokenKind, mint_access, mint_refresh, verify,
};
pub use oidc::{OidcClaims, OidcConfig, OidcError, OidcVerifier, peek_issuer};
pub use password::{PasswordError, generate_random_password, hash_password, verify_password};
pub use redacted::RedactedString;
