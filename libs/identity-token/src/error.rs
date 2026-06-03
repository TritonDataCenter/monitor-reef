// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Stable error surface. The underlying `jsonwebtoken` error type is
//! deliberately not part of the public API so the crate can swap it.

use jsonwebtoken::Algorithm;

/// Why a token failed to verify.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// The token could not be parsed (bad segments, missing `kid`, etc.).
    #[error("malformed token: {0}")]
    Malformed(String),

    /// The token's `alg` header is symmetric or otherwise not an
    /// allowed asymmetric algorithm. Rejecting here defeats the HS/RS
    /// "alg confusion" attack before a key is ever loaded.
    #[error("unsupported or disallowed algorithm: {0:?}")]
    UnsupportedAlg(Algorithm),

    /// No JWK in the realm's key set matches the token's `kid`.
    #[error("unknown signing key (kid {0})")]
    UnknownKid(String),

    /// Signature did not verify against the resolved key.
    #[error("token signature is invalid")]
    BadSignature,

    /// `exp` is in the past (beyond leeway).
    #[error("token has expired")]
    Expired,

    /// `nbf` is in the future (beyond leeway).
    #[error("token is not yet valid")]
    NotYetValid,

    /// `iss` did not match the configured issuer.
    #[error("token issuer is not trusted")]
    InvalidIssuer,

    /// `aud` did not match the configured audience.
    #[error("token audience is invalid")]
    InvalidAudience,

    /// The JWKS could not be fetched or parsed.
    #[error("jwks error: {0}")]
    Jwks(String),

    /// Any other validation failure.
    #[error("invalid token: {0}")]
    Invalid(String),
}

/// Map a `jsonwebtoken` validation error onto a stable [`TokenError`].
pub(crate) fn map_validation_err(err: jsonwebtoken::errors::Error) -> TokenError {
    use jsonwebtoken::errors::ErrorKind;
    match err.kind() {
        ErrorKind::ExpiredSignature => TokenError::Expired,
        ErrorKind::ImmatureSignature => TokenError::NotYetValid,
        ErrorKind::InvalidSignature => TokenError::BadSignature,
        ErrorKind::InvalidIssuer => TokenError::InvalidIssuer,
        ErrorKind::InvalidAudience => TokenError::InvalidAudience,
        _ => TokenError::Invalid(err.to_string()),
    }
}
