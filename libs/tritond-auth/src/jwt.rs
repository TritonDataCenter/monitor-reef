// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! HS256 JWT mint / verify for operator login flows.
//!
//! Two token kinds:
//!
//! * **Access tokens** are short-lived (15 minutes) and presented on
//!   every authenticated API request.
//! * **Refresh tokens** are longer-lived (24 hours) and used only by
//!   `POST /v2/auth/refresh` to obtain a fresh access token without
//!   re-prompting the operator.
//!
//! Both use the same symmetric `JwtKey` and discriminate via a `typ`
//! claim, so a refresh token cannot be silently used as an access
//! token (or vice versa).

use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Symmetric key length. 32 bytes (256 bits) matches the HS256 block
/// size and matches what most other ecosystems do.
pub const JWT_KEY_BYTES: usize = 32;

/// Access-token lifetime.
pub const ACCESS_TOKEN_TTL_MINUTES: i64 = 15;

/// Refresh-token lifetime.
pub const REFRESH_TOKEN_TTL_HOURS: i64 = 24;

/// Errors returned by the JWT helpers.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    /// jsonwebtoken rejected the token (bad signature, expired, malformed).
    #[error("jwt error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    /// The token type embedded in the `typ` claim does not match the
    /// type the caller asked us to verify (e.g. an access endpoint
    /// received a refresh token).
    #[error("token kind mismatch: expected {expected:?}, got {got:?}")]
    KindMismatch { expected: TokenKind, got: TokenKind },
}

/// Kind of token. Encoded as the `typ` claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Access,
    Refresh,
}

/// HS256 signing/verifying key. Wraps the raw bytes plus the
/// jsonwebtoken encode/decode keys derived once.
pub struct JwtKey {
    encoding: EncodingKey,
    decoding: DecodingKey,
    bytes: [u8; JWT_KEY_BYTES],
}

impl JwtKey {
    /// Generate a fresh random key. Use this once at cluster
    /// bootstrap; persist `bytes()` to FoundationDB.
    pub fn generate() -> Self {
        let mut bytes = [0u8; JWT_KEY_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self::from_bytes(bytes)
    }

    /// Reconstruct from previously-persisted bytes.
    pub fn from_bytes(bytes: [u8; JWT_KEY_BYTES]) -> Self {
        Self {
            encoding: EncodingKey::from_secret(&bytes),
            decoding: DecodingKey::from_secret(&bytes),
            bytes,
        }
    }

    /// Borrow the raw key bytes (e.g. for persistence).
    pub fn bytes(&self) -> &[u8; JWT_KEY_BYTES] {
        &self.bytes
    }
}

/// Claims for an access token. `sub` is the user id; `exp` and `iat`
/// are unix timestamps in seconds; `typ` discriminates from refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    pub sub: Uuid,
    pub exp: i64,
    pub iat: i64,
    pub typ: TokenKind,
}

/// Claims for a refresh token. `jti` lets the server revoke specific
/// refresh tokens later (not used in Phase 0e but cheap to include).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshClaims {
    pub sub: Uuid,
    pub jti: Uuid,
    pub exp: i64,
    pub iat: i64,
    pub typ: TokenKind,
}

/// Mint an access token for `user_id`. Returns the wire-form JWT and
/// the expiration time so the client can cache and pre-empt
/// refresh.
pub fn mint_access(key: &JwtKey, user_id: Uuid) -> Result<(String, DateTime<Utc>), JwtError> {
    let now = Utc::now();
    let expires_at = now + Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
    let claims = AccessClaims {
        sub: user_id,
        exp: expires_at.timestamp(),
        iat: now.timestamp(),
        typ: TokenKind::Access,
    };
    let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &key.encoding)?;
    Ok((token, expires_at))
}

/// Mint a refresh token for `user_id`. Returns the JWT and the
/// expiration time.
pub fn mint_refresh(key: &JwtKey, user_id: Uuid) -> Result<(String, DateTime<Utc>), JwtError> {
    let now = Utc::now();
    let expires_at = now + Duration::hours(REFRESH_TOKEN_TTL_HOURS);
    let claims = RefreshClaims {
        sub: user_id,
        jti: Uuid::new_v4(),
        exp: expires_at.timestamp(),
        iat: now.timestamp(),
        typ: TokenKind::Refresh,
    };
    let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &key.encoding)?;
    Ok((token, expires_at))
}

/// Verify a token of the given kind and return its access claims.
///
/// `expected` distinguishes access from refresh — passing a refresh
/// token to a handler that wanted an access token (or vice versa)
/// is rejected via [`JwtError::KindMismatch`].
pub fn verify(key: &JwtKey, token: &str, expected: TokenKind) -> Result<AccessClaims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp", "sub"]);
    let data = jsonwebtoken::decode::<AccessClaims>(token, &key.decoding, &validation)?;
    if data.claims.typ != expected {
        return Err(JwtError::KindMismatch {
            expected,
            got: data.claims.typ,
        });
    }
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_round_trip() {
        let key = JwtKey::generate();
        let user = Uuid::new_v4();
        let (token, _exp) = mint_access(&key, user).unwrap();
        let claims = verify(&key, &token, TokenKind::Access).unwrap();
        assert_eq!(claims.sub, user);
        assert_eq!(claims.typ, TokenKind::Access);
    }

    #[test]
    fn refresh_round_trip() {
        let key = JwtKey::generate();
        let user = Uuid::new_v4();
        let (token, _exp) = mint_refresh(&key, user).unwrap();
        let claims = verify(&key, &token, TokenKind::Refresh).unwrap();
        assert_eq!(claims.sub, user);
        assert_eq!(claims.typ, TokenKind::Refresh);
    }

    #[test]
    fn refresh_token_rejected_at_access_endpoint() {
        let key = JwtKey::generate();
        let (token, _) = mint_refresh(&key, Uuid::new_v4()).unwrap();
        let err = verify(&key, &token, TokenKind::Access).unwrap_err();
        assert!(matches!(err, JwtError::KindMismatch { .. }));
    }

    #[test]
    fn wrong_key_rejects_token() {
        let key_a = JwtKey::generate();
        let key_b = JwtKey::generate();
        let (token, _) = mint_access(&key_a, Uuid::new_v4()).unwrap();
        let err = verify(&key_b, &token, TokenKind::Access).unwrap_err();
        assert!(matches!(err, JwtError::Jwt(_)));
    }

    #[test]
    fn round_trip_through_serialized_key_bytes() {
        let original = JwtKey::generate();
        let user = Uuid::new_v4();
        let (token, _) = mint_access(&original, user).unwrap();

        let restored = JwtKey::from_bytes(*original.bytes());
        let claims = verify(&restored, &token, TokenKind::Access).unwrap();
        assert_eq!(claims.sub, user);
    }
}
