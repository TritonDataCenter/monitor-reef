// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton Cloud control-plane API.
//!
//! Phase 0 ships a deliberately small surface — a liveness check and
//! the silo CRUD primitives — that exercises the full Dropshot +
//! OpenAPI + Progenitor + FoundationDB pipeline end to end. Subsequent
//! phases extend the trait with `/v2/instances`, `/v2/audit`, and the
//! rest of DESIGN.md §14.
//!
//! Domain types live in [`tritond_store`] and are re-exported from
//! [`mod@types`] so wire types and storage types never drift.

pub mod types;

use chrono::{DateTime, Utc};
use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
    TypedBody,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{ApiKeyView, NewSilo, Silo};

/// Liveness response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Path parameters for endpoints that operate on a single silo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SiloPath {
    pub silo_id: Uuid,
}

/// Path parameters for endpoints that operate on a single API key.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApiKeyPath {
    pub api_key_id: Uuid,
}

/// Request body for `POST /v2/auth/login`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Request body for `POST /v2/auth/refresh`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Response body for both login and refresh.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: DateTime<Utc>,
}

/// Request body for `POST /v2/auth/api-keys`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NewApiKey {
    pub description: String,
}

/// Response body for `POST /v2/auth/api-keys`.
///
/// `secret` is the wire-form key. It is shown to the operator
/// **once**; the server retains only a bcrypt hash.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApiKeyCreated {
    #[serde(flatten)]
    pub key: ApiKeyView,
    pub secret: String,
}

#[dropshot::api_description]
pub trait TritondApi {
    /// Context type for request handlers.
    type Context: Send + Sync + 'static;

    /// Liveness check. Returns service status and version string.
    #[endpoint {
        method = GET,
        path = "/v2/health",
        tags = ["system"],
    }]
    async fn health(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError>;

    /// Create a silo. Returns 201 with the created silo.
    ///
    /// Fails with 409 if a silo with the requested name already exists.
    #[endpoint {
        method = POST,
        path = "/v2/silos",
        tags = ["silos"],
    }]
    async fn create_silo(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewSilo>,
    ) -> Result<HttpResponseCreated<Silo>, HttpError>;

    /// Look up a silo by id. Returns 404 if no such silo exists.
    #[endpoint {
        method = GET,
        path = "/v2/silos/{silo_id}",
        tags = ["silos"],
    }]
    async fn get_silo(
        rqctx: RequestContext<Self::Context>,
        path: Path<SiloPath>,
    ) -> Result<HttpResponseOk<Silo>, HttpError>;

    /// Exchange username + password for an access/refresh token pair.
    /// Returns 401 if credentials are invalid.
    #[endpoint {
        method = POST,
        path = "/v2/auth/login",
        tags = ["auth"],
    }]
    async fn login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError>;

    /// Exchange a valid refresh token for a fresh access/refresh pair.
    /// Returns 401 if the refresh token is invalid or expired.
    #[endpoint {
        method = POST,
        path = "/v2/auth/refresh",
        tags = ["auth"],
    }]
    async fn refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError>;

    /// Create a long-lived API key for the calling user. The plaintext
    /// secret is included in the response **once** and never shown again.
    #[endpoint {
        method = POST,
        path = "/v2/auth/api-keys",
        tags = ["auth"],
    }]
    async fn create_api_key(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NewApiKey>,
    ) -> Result<HttpResponseCreated<ApiKeyCreated>, HttpError>;

    /// List the calling user's API keys. The plaintext secret is never
    /// returned by this endpoint.
    #[endpoint {
        method = GET,
        path = "/v2/auth/api-keys",
        tags = ["auth"],
    }]
    async fn list_api_keys(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<Vec<ApiKeyView>>, HttpError>;

    /// Delete one of the calling user's API keys. Returns 404 if the
    /// id does not belong to the calling user.
    #[endpoint {
        method = DELETE,
        path = "/v2/auth/api-keys/{api_key_id}",
        tags = ["auth"],
    }]
    async fn delete_api_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<ApiKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;
}
