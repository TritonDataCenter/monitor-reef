// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Errors produced by session auth.
//!
//! Deliberately framework-free: no axum `IntoResponse`, no Dropshot
//! `HttpError`. Callers map these variants to their own HTTP error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("authentication failed")]
    AuthenticationFailed,

    #[error("token expired")]
    TokenExpired,

    #[error("invalid token")]
    InvalidToken,

    #[error("LDAP unavailable: {0}")]
    LdapUnavailable(String),

    #[error("LDAP misconfiguration: {0}")]
    LdapConfigError(String),

    #[error("Mahi unavailable: {0}")]
    MahiUnavailable(String),

    #[error("JWT key error: {0}")]
    JwtKeyError(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl From<jsonwebtoken::errors::Error> for SessionError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        use jsonwebtoken::errors::ErrorKind;
        match err.kind() {
            ErrorKind::ExpiredSignature => Self::TokenExpired,
            _ => Self::InvalidToken,
        }
    }
}

pub type SessionResult<T> = Result<T, SessionError>;
