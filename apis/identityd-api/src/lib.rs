// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Dropshot API trait and wire types for `identityd`.
//!
//! `identityd` is a minimal native OpenID Connect provider (RFD 00004).
//! This crate is the trait-based source of truth for its HTTP surface:
//! the realm-scoped discovery / JWKS / token / userinfo endpoints the
//! Workbench BFF and `tritond` talk to. The implementation lives in
//! `services/identityd`; the access-token *shape* it mints is
//! `identity-token::AccessClaims`, which both this provider and every
//! verifier link.

use dropshot::{HttpError, HttpResponseOk, Path, RequestContext, TypedBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ===========================================================================
// Path / request / response types
// ===========================================================================

/// Realm id path parameter (`/realms/{realm}/...`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RealmPath {
    /// The realm's UUID.
    pub realm: Uuid,
}

/// Liveness probe response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthResponse {
    /// Always `"ok"` when the process is serving.
    pub status: String,
}

/// OIDC discovery document (the subset RPs in this system consume).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenIdConfiguration {
    pub issuer: String,
    pub jwks_uri: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
}

/// Token request body (RFD 00004 token endpoint; JSON, not form-encoded).
///
/// One struct covers the three grants this minimal provider supports —
/// `password`, `refresh_token`, and `client_credentials`. The handler
/// validates which fields are required per `grant_type`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenRequest {
    /// `"password"`, `"refresh_token"`, or `"client_credentials"`.
    pub grant_type: String,
    /// Resource-owner username (`password` grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Resource-owner password (`password` grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// The refresh token being exchanged (`refresh_token` grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret (confidential clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Optional space-delimited requested scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Token response (RFC 6749 §5.1).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub scope: String,
}

/// `userinfo` response. Carries the denormalized tenancy claims the
/// Workbench BFF turns into a session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UserInfo {
    pub sub: Uuid,
    pub preferred_username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub name: String,
    pub realm: Uuid,
    /// Realm scope tag: `"tenant"`, `"silo"`, or `"system"`.
    pub realm_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silo_id: Option<Uuid>,
    pub is_root: bool,
    pub fleet_admin: bool,
    pub groups: Vec<String>,
}

// ===========================================================================
// API trait
// ===========================================================================

/// identityd's HTTP surface (RFD 00004).
#[dropshot::api_description]
pub trait IdentitydApi {
    /// Context type for request handlers.
    type Context: Send + Sync + 'static;

    /// Liveness probe.
    #[endpoint {
        method = GET,
        path = "/healthz",
        tags = ["system"],
    }]
    async fn healthz(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError>;

    /// OIDC discovery document for a realm.
    #[endpoint {
        method = GET,
        path = "/realms/{realm}/.well-known/openid-configuration",
        tags = ["oidc"],
    }]
    async fn openid_configuration(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<OpenIdConfiguration>, HttpError>;

    /// The realm's published JWK set (`{"keys":[...]}`).
    #[endpoint {
        method = GET,
        path = "/realms/{realm}/jwks",
        tags = ["oidc"],
    }]
    async fn jwks(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Token endpoint: password / refresh_token / client_credentials.
    #[endpoint {
        method = POST,
        path = "/realms/{realm}/token",
        tags = ["oidc"],
    }]
    async fn token(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
        body: TypedBody<TokenRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError>;

    /// userinfo: resolve a bearer access token to its claims.
    #[endpoint {
        method = GET,
        path = "/realms/{realm}/userinfo",
        tags = ["oidc"],
    }]
    async fn userinfo(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<UserInfo>, HttpError>;
}
