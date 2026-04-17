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

    /// Authenticate an LDAP user and issue an access + refresh token pair.
    ///
    /// On success the response body carries the tokens and a `Set-Cookie`
    /// header with the access token for browser clients. CLI clients can
    /// ignore the cookie and read the token from the JSON body.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login",
        tags = ["auth"],
    }]
    async fn auth_login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
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
