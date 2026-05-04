// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton API trait definition
//!
//! This crate defines the API trait for the Triton API service.
//! It serves as the public-facing HTTP API for the Triton datacenter.

use dropshot::{HttpError, HttpResponseHeaders, HttpResponseOk, RequestContext, TypedBody};

pub mod types;
pub use types::*;

/// Triton API
#[dropshot::api_description]
pub trait TritonApi {
    type Context: Send + Sync + 'static;

    /// Ping
    #[endpoint {
        method = GET,
        path = "/v1/ping",
        tags = ["system"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    /// Authenticate an LDAP user and either issue tokens directly or
    /// require a second factor.
    ///
    /// The response body is a tagged [`LoginOutcome`]:
    ///
    ///   * `complete` — password was correct and the user has no 2FA
    ///     enrolment; the embedded fields are identical to the
    ///     historical [`LoginResponse`] shape and a `Set-Cookie`
    ///     header carries the access token for browser clients.
    ///   * `challenge_required` — password was correct but the user
    ///     has a second factor enrolled; the embedded
    ///     [`LoginChallenge`] carries a `challenge_token` and the
    ///     list of methods the client may use. The client must POST
    ///     the `challenge_token` plus a code to
    ///     `/v1/auth/login/verify`. No cookie is set on this branch
    ///     since the session has not been established yet.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login",
        tags = ["auth"],
    }]
    async fn auth_login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginOutcome>>, HttpError>;

    /// Complete a 2FA login by presenting the challenge token and a
    /// second-factor code.
    ///
    /// Called only when `/v1/auth/login` returned a
    /// `challenge_required` outcome. The server re-reads the user's
    /// TOTP secret from UFDS (it is never carried in the challenge),
    /// verifies the code, and returns the standard [`LoginResponse`]
    /// — same shape, same `Set-Cookie` semantics — that
    /// `/v1/auth/login` issues for non-2FA users.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login/verify",
        tags = ["auth"],
    }]
    async fn auth_login_verify(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginVerifyRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError>;

    /// Exchange a proof-of-SSH-key-ownership for an access + refresh
    /// token pair. The caller presents an `Authorization: Signature …`
    /// header (draft-cavage HTTP Signature, same dialect cloudapi
    /// accepts). The server resolves the key via mahi, verifies the
    /// signature, and issues tokens via the same path the password
    /// login uses.
    ///
    /// Request body is empty — all auth material is in the headers.
    /// Response mirrors `POST /v1/auth/login`.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login-ssh",
        tags = ["auth"],
    }]
    async fn auth_login_ssh(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError>;

    /// Revoke all outstanding refresh tokens for the caller and clear
    /// the auth cookie. Accepts an expired access token so that callers
    /// whose session has already expired can still log out cleanly.
    #[endpoint {
        method = POST,
        path = "/v1/auth/logout",
        tags = ["auth"],
    }]
    async fn auth_logout(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LogoutResponse>>, HttpError>;

    /// Rotate a refresh token: consume the caller's refresh token and
    /// return a new `(access, refresh)` pair. The old refresh token is
    /// single-use and is invalidated on success.
    #[endpoint {
        method = POST,
        path = "/v1/auth/refresh",
        tags = ["auth"],
    }]
    async fn auth_refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<RefreshResponse>, HttpError>;

    /// Return the authenticated caller's identity, derived from the JWT
    /// claims. Useful for web UIs to check login state on page load.
    #[endpoint {
        method = GET,
        path = "/v1/auth/session",
        tags = ["auth"],
    }]
    async fn auth_session(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<SessionResponse>, HttpError>;

    /// RFC 7517 JWKS document containing the public key(s) used to sign
    /// access tokens. Consumed by external JWT verifiers -- the gateway
    /// today, any future adminui proxy or DC component tomorrow. No auth
    /// required.
    #[endpoint {
        method = GET,
        path = "/v1/auth/jwks.json",
        tags = ["auth"],
    }]
    async fn auth_jwks(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<JwkSet>, HttpError>;
}
