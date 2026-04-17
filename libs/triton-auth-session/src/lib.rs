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
//! 1. Client `POST /v1/auth/login` with LDAP credentials.
//! 2. [`LdapService::authenticate`] binds admin, looks up the user as
//!    `sdcperson`, verifies the password via LDAP compare, and resolves
//!    roles from `memberof`.
//! 3. [`JwtService::create_token`] signs a short-lived access token with an
//!    ES256 private key. [`JwtService::create_refresh_token`] mints an
//!    opaque single-use refresh token held in memory.
//! 4. Any verifier (same process, the gateway, or a future adminui proxy)
//!    validates the access token against the public key. The public key is
//!    published at `GET /v1/auth/jwks.json` via [`JwtService::jwks`].
//!
//! Refresh token storage is in-process only; see [`jwt`] module docs for the
//! migration path to a persistent store.

pub mod error;
pub mod jwt;
pub mod ldap;
pub mod models;

pub use error::{SessionError, SessionResult};
pub use jwt::{JwtConfig, JwtService};
pub use ldap::{LdapConfig, LdapService, UfdsUser};
pub use models::{Claims, Role, roles_imply_admin};
