// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Session auth for Triton.
//!
//! Provides LDAP-backed login and ES256 JWT issuance/verification. Designed
//! to be framework-agnostic so that both `triton-api-server` (Dropshot) and
//! `triton-gateway` (Axum) can use it without pulling in each other's HTTP
//! stack. Callers translate `SessionError` into their own HTTP error type.
//!
//! # Identity flow
//!
//! 1. Client `POST /v1/auth/login` with UFDS credentials.
//! 2. [`LdapService::authenticate`] binds admin, looks up the user as
//!    `sdcperson`, and verifies the password via LDAP compare.
//! 3. [`MahiService::lookup`] reads the same login from the Mahi auth cache
//!    to resolve operator status and group memberships — cheaper and with
//!    fewer UFDS quirks than the pre-mahi LDAP groupofuniquenames dance.
//! 4. [`JwtService::create_token`] signs a short-lived access token with an
//!    ES256 private key. [`JwtService::create_refresh_token`] mints an
//!    opaque single-use refresh token held in memory.
//! 5. Any verifier (same process, the gateway, or a future adminui proxy)
//!    validates the access token against the public key. The public key is
//!    published at `GET /v1/auth/jwks.json` via [`JwtService::jwks`].
//!
//! Refresh token storage is in-process only; see [`jwt`] module docs for the
//! migration path to a persistent store.

pub mod error;
pub mod jwks;
pub mod jwt;
pub mod ldap;
pub mod mahi;
pub mod models;
pub mod totp;

pub use error::{SessionError, SessionResult};
pub use jwks::JwksClient;
pub use jwt::{JwtConfig, JwtService, JwtVerifier};
pub use ldap::{LdapConfig, LdapService, UfdsUser};
pub use mahi::{AuthInfo, MahiService};
pub use models::{Claims, Role, roles_imply_admin};
pub use totp::verify_totp;
