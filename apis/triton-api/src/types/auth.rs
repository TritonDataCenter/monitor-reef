// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the `/v1/auth/*` endpoints.
//!
//! These are the wire-format types clients see. They intentionally do not
//! reuse the internal structures in `triton-auth-session` — the library
//! types can evolve independently of the public API contract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/auth/login` request body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// `POST /v1/auth/login` response body when the password is correct
/// and no second factor is required. Also the response body of
/// `POST /v1/auth/login/verify` (which always completes a session).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoginResponse {
    /// Short-lived ES256 JWT access token.
    pub token: String,
    /// Single-use refresh token; present it to `POST /v1/auth/refresh`
    /// for a new (token, refresh_token) pair.
    pub refresh_token: String,
    pub user: UserInfo,
}

/// Outcome of `POST /v1/auth/login`.
///
/// Tagged on the wire by the `outcome` field. The common case is
/// `complete`: the password verified, no second factor is enrolled,
/// and the response carries the same fields a non-2FA `LoginResponse`
/// always has. Users enrolled in 2FA receive `challenge_required`,
/// must read a code from their authenticator, and post it together
/// with the `challenge_token` to `/v1/auth/login/verify` to obtain a
/// `LoginResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LoginOutcome {
    /// Authentication is complete; tokens are issued.
    Complete(LoginResponse),
    /// Authentication needs a second factor before tokens are
    /// issued. Carry the `challenge_token` to `/v1/auth/login/verify`.
    ChallengeRequired(LoginChallenge),
}

/// Body of a `LoginOutcome::ChallengeRequired` outcome.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoginChallenge {
    /// Short-lived signed token. The client posts this verbatim
    /// alongside the second-factor code to `/v1/auth/login/verify`.
    /// Treat as opaque — the token's claims and TTL are server-side
    /// implementation details.
    pub challenge_token: String,
    /// Methods the client may use to satisfy the challenge. v1 only
    /// emits `[totp]`, but clients should tolerate additional values
    /// via the `Unknown` variant rather than failing closed.
    pub methods: Vec<ChallengeMethod>,
}

/// Second-factor authentication method offered for a login challenge.
///
/// Forward-compatible: clients deserialising an older binary can
/// receive new method names from a newer server and round-trip them
/// through `Unknown` rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeMethod {
    /// Time-based one-time password (RFC 6238). Currently the only
    /// method tritonapi issues challenges for.
    Totp,
    /// Catch-all for forward compatibility; an unrecognised method
    /// from a newer server.
    #[serde(other)]
    Unknown,
}

/// `POST /v1/auth/login/verify` request body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoginVerifyRequest {
    /// The exact `challenge_token` value from the
    /// `ChallengeRequired` outcome. Treated as opaque.
    pub challenge_token: String,
    /// 6-digit code from the user's authenticator app.
    pub code: String,
}

/// `POST /v1/auth/refresh` request body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// `POST /v1/auth/refresh` response body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RefreshResponse {
    pub token: String,
    pub refresh_token: String,
}

/// `GET /v1/auth/session` response body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionResponse {
    pub user: UserInfo,
}

/// `POST /v1/auth/logout` response body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LogoutResponse {
    pub ok: bool,
}

/// Public profile of the authenticated user. `email`, `name`, and `company`
/// are only populated from `/v1/auth/login` (which reads UFDS); the
/// `/v1/auth/session` endpoint can only return fields that live in the
/// JWT claims, which is why those fields are optional.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    pub is_admin: bool,
}

/// RFC 7517 JWKS document returned by `GET /v1/auth/jwks.json`.
///
/// Verifiers (triton-gateway, future admin-UI proxies, etc.) fetch this to
/// obtain the public key(s) used to validate access tokens. Only ES256
/// P-256 keys are currently issued.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// A single JWK entry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub key_use: String,
    pub kid: String,
    /// Base64url-encoded X coordinate of the EC public key.
    pub x: String,
    /// Base64url-encoded Y coordinate of the EC public key.
    pub y: String,
}
