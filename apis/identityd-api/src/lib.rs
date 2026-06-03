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

use dropshot::{
    HttpError, HttpResponseDeleted, HttpResponseOk, Path, Query, RequestContext, TypedBody,
};
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
// Admin (operator) surface — `/v1/...`
//
// A separate operator/admin surface from the public realm-scoped OIDC
// endpoints. Every endpoint requires an identityd Bearer access token and
// enforces tenancy isolation (see `services/identityd/src/admin.rs`).
//
// Responses NEVER carry a client secret or a password hash. Secret-bearing
// inputs use write-only request fields; secret-bearing records are summarized
// with a redaction marker on the way out.
// ===========================================================================

/// `{realm}` path parameter on the admin surface. Unlike the OIDC surface
/// (which pins two realm ids), this is the realm's real store id from
/// `GET /v1/realms`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AdminRealmPath {
    pub realm: Uuid,
}

/// `{realm}/users/{user_id}` path parameter.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AdminUserPath {
    pub realm: Uuid,
    pub user_id: Uuid,
}

/// `{realm}/role-assignments/{id}` path parameter.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AdminAssignmentPath {
    pub realm: Uuid,
    pub id: Uuid,
}

/// `{realm}/connections/{id}` path parameter.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AdminConnectionPath {
    pub realm: Uuid,
    pub id: Uuid,
}

/// `{tenant_id}` path parameter for the create-or-get-realm convenience.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AdminTenantPath {
    pub tenant_id: Uuid,
}

/// Optional `?scope=tenant:{uuid}` filter on `GET /v1/realms`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListRealmsQuery {
    /// e.g. `tenant:22222222-...`. When present, narrows the result to the
    /// realm whose scope matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Realm scope tag + ids, mirrored onto the wire (no store dependency in
/// this crate, so we re-describe the scope rather than import it). The
/// store's scope enum is `#[non_exhaustive]`; `Unknown` keeps this view
/// total and forward-compatible (Type-Safety Rule 5).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmScopeView {
    Tenant { tenant_id: Uuid },
    Silo { silo_id: Uuid },
    System,
    Unknown,
}

/// A realm, as returned by the admin surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RealmView {
    pub id: Uuid,
    pub scope: RealmScopeView,
    pub name: String,
    pub description: String,
    pub issuer_url: String,
    pub created_at: String,
}

/// Account state on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserStatusView {
    Active,
    Disabled,
}

/// A user, as returned by the admin surface. Never carries `password_hash`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UserView {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub display_name: String,
    pub status: UserStatusView,
    /// Whether this user was JIT-provisioned from an upstream IdP.
    pub brokered: bool,
    pub created_at: String,
}

/// Create-native-user request. `password` is write-only (bcrypt-hashed
/// server-side); the response is a [`UserView`], which omits the hash.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateUserRequest {
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub password: String,
}

/// Partial user update. Absent fields are left unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<UserStatusView>,
}

/// Set-password request (write-only).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetPasswordRequest {
    pub password: String,
}

/// Coarse role on the wire (mirrors the store's `Role`). `Unknown`
/// forward-covers a `#[non_exhaustive]` store `Role` (Type-Safety Rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleView {
    TenantAdmin,
    TenantMember,
    SiloAdmin,
    FleetAdmin,
    Operator,
    ReadOnly,
    #[serde(other)]
    Unknown,
}

/// Subject of a role assignment. `Unknown` forward-covers the
/// `#[non_exhaustive]` store enum (Type-Safety Rule 5).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssignmentSubjectView {
    User { user_id: Uuid },
    Group { group_id: Uuid },
    Unknown,
}

/// Target of a role assignment. `Unknown` forward-covers the
/// `#[non_exhaustive]` store enum (Type-Safety Rule 5).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssignmentTargetView {
    Tenant { tenant_id: Uuid },
    Silo { silo_id: Uuid },
    Fleet,
    Unknown,
}

/// A role assignment, as returned by the admin surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleAssignmentView {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub subject: AssignmentSubjectView,
    pub target: AssignmentTargetView,
    pub role: RoleView,
    pub created_at: String,
}

/// Create-role-assignment request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateRoleAssignmentRequest {
    pub subject: AssignmentSubjectView,
    pub target: AssignmentTargetView,
    pub role: RoleView,
}

/// The derived identity-source mode for a realm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySourceMode {
    /// No enabled upstream connection: native (integrated) login.
    Integrated,
    /// An enabled OIDC upstream is in force.
    Oidc,
    /// An enabled SAML upstream is in force.
    Saml,
}

/// A non-secret summary of the active upstream connection backing an
/// `oidc`/`saml` identity source.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdentitySourceConnection {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    /// OIDC: the upstream issuer URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer_url: Option<String>,
    /// OIDC: the registered client id (never the secret).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// SAML: the upstream IdP metadata URL/XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idp_metadata: Option<String>,
    /// SAML: the SP entity id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sp_entity_id: Option<String>,
}

/// `GET /v1/realms/{realm}/identity-source` response — the Integrated↔Azure
/// toggle backing summary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdentitySourceView {
    pub mode: IdentitySourceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<IdentitySourceConnection>,
}

/// Connection protocol/config on the wire. The OIDC `client_secret` is a
/// write-only input field (see [`CreateConnectionRequest`]); it never
/// appears in a [`ConnectionView`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ConnectionKindView {
    Oidc {
        issuer_url: String,
        client_id: String,
        scopes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
        /// Always `true` — a secret is on file but is never disclosed.
        client_secret_set: bool,
    },
    Saml {
        idp_metadata: String,
        sp_entity_id: String,
        sp_acs_url: String,
        want_signed_assertions: bool,
    },
    /// Forward-covers the `#[non_exhaustive]` store enum (Type-Safety Rule 5).
    Unknown,
}

/// An upstream connection, as returned by the admin surface. The OIDC
/// `client_secret` is redacted (represented only as `client_secret_set`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionView {
    pub id: Uuid,
    pub realm_id: Uuid,
    pub name: String,
    pub kind: ConnectionKindView,
    pub enabled: bool,
    pub created_at: String,
}

/// Connection config on input. Carries the OIDC `client_secret` (write-only).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ConnectionKindInput {
    Oidc {
        issuer_url: String,
        client_id: String,
        client_secret: String,
        scopes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
    Saml {
        idp_metadata: String,
        sp_entity_id: String,
        sp_acs_url: String,
        want_signed_assertions: bool,
    },
}

/// Create-connection request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateConnectionRequest {
    pub name: String,
    pub kind: ConnectionKindInput,
    /// Defaults to `false` (created disabled — enable to switch the realm
    /// to this upstream).
    #[serde(default)]
    pub enabled: bool,
}

/// Patch-connection request. Any subset; absent fields are unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PatchConnectionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ConnectionKindInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Which realm-user field a claim maps onto. `Unknown` forward-covers the
/// `#[non_exhaustive]` store enum (Type-Safety Rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MappedFieldView {
    Username,
    Email,
    DisplayName,
    Group,
    #[serde(other)]
    Unknown,
}

/// One claim-mapping rule on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClaimMappingView {
    pub source: String,
    pub target: MappedFieldView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_value: Option<String>,
}

/// Replace-claim-mappings request body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PutClaimMappingsRequest {
    pub mappings: Vec<ClaimMappingView>,
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

    // ----------------------------------------------------------------------
    // Admin surface (`/v1/...`)
    // ----------------------------------------------------------------------

    /// List realms (fleet token only); optional `?scope=tenant:{uuid}`.
    #[endpoint {
        method = GET,
        path = "/v1/realms",
        tags = ["admin"],
    }]
    async fn admin_list_realms(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListRealmsQuery>,
    ) -> Result<HttpResponseOk<Vec<RealmView>>, HttpError>;

    /// Realm detail.
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}",
        tags = ["admin"],
    }]
    async fn admin_get_realm(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<RealmView>, HttpError>;

    /// Create-or-get the tenant's realm (idempotent; fleet token only).
    #[endpoint {
        method = POST,
        path = "/v1/tenants/{tenant_id}/realm",
        tags = ["admin"],
    }]
    async fn admin_create_tenant_realm(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminTenantPath>,
    ) -> Result<HttpResponseOk<RealmView>, HttpError>;

    /// List users in a realm.
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}/users",
        tags = ["admin"],
    }]
    async fn admin_list_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<UserView>>, HttpError>;

    /// Create a native user.
    #[endpoint {
        method = POST,
        path = "/v1/realms/{realm}/users",
        tags = ["admin"],
    }]
    async fn admin_create_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateUserRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError>;

    /// Get one user.
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}/users/{user_id}",
        tags = ["admin"],
    }]
    async fn admin_get_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminUserPath>,
    ) -> Result<HttpResponseOk<UserView>, HttpError>;

    /// Update a user's email/display_name/status.
    #[endpoint {
        method = PATCH,
        path = "/v1/realms/{realm}/users/{user_id}",
        tags = ["admin"],
    }]
    async fn admin_update_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminUserPath>,
        body: TypedBody<UpdateUserRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError>;

    /// Set a user's password.
    #[endpoint {
        method = POST,
        path = "/v1/realms/{realm}/users/{user_id}/password",
        tags = ["admin"],
    }]
    async fn admin_set_user_password(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminUserPath>,
        body: TypedBody<SetPasswordRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError>;

    /// Delete a user.
    #[endpoint {
        method = DELETE,
        path = "/v1/realms/{realm}/users/{user_id}",
        tags = ["admin"],
    }]
    async fn admin_delete_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminUserPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List role assignments in a realm.
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}/role-assignments",
        tags = ["admin"],
    }]
    async fn admin_list_role_assignments(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<RoleAssignmentView>>, HttpError>;

    /// Create a role assignment.
    #[endpoint {
        method = POST,
        path = "/v1/realms/{realm}/role-assignments",
        tags = ["admin"],
    }]
    async fn admin_create_role_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateRoleAssignmentRequest>,
    ) -> Result<HttpResponseOk<RoleAssignmentView>, HttpError>;

    /// Delete a role assignment.
    #[endpoint {
        method = DELETE,
        path = "/v1/realms/{realm}/role-assignments/{id}",
        tags = ["admin"],
    }]
    async fn admin_delete_role_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminAssignmentPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Derived identity-source summary (Integrated↔Azure toggle backing).
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}/identity-source",
        tags = ["admin"],
    }]
    async fn admin_get_identity_source(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<IdentitySourceView>, HttpError>;

    /// List upstream connections (`client_secret` redacted).
    #[endpoint {
        method = GET,
        path = "/v1/realms/{realm}/connections",
        tags = ["admin"],
    }]
    async fn admin_list_connections(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<ConnectionView>>, HttpError>;

    /// Create an upstream connection.
    #[endpoint {
        method = POST,
        path = "/v1/realms/{realm}/connections",
        tags = ["admin"],
    }]
    async fn admin_create_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateConnectionRequest>,
    ) -> Result<HttpResponseOk<ConnectionView>, HttpError>;

    /// Patch an upstream connection (fields and/or `enabled`).
    #[endpoint {
        method = PATCH,
        path = "/v1/realms/{realm}/connections/{id}",
        tags = ["admin"],
    }]
    async fn admin_patch_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminConnectionPath>,
        body: TypedBody<PatchConnectionRequest>,
    ) -> Result<HttpResponseOk<ConnectionView>, HttpError>;

    /// Delete an upstream connection.
    #[endpoint {
        method = DELETE,
        path = "/v1/realms/{realm}/connections/{id}",
        tags = ["admin"],
    }]
    async fn admin_delete_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminConnectionPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Replace a connection's claim-mapping list.
    #[endpoint {
        method = PUT,
        path = "/v1/realms/{realm}/connections/{id}/claim-mappings",
        tags = ["admin"],
    }]
    async fn admin_put_claim_mappings(
        rqctx: RequestContext<Self::Context>,
        path: Path<AdminConnectionPath>,
        body: TypedBody<PutClaimMappingsRequest>,
    ) -> Result<HttpResponseOk<Vec<ClaimMappingView>>, HttpError>;
}
