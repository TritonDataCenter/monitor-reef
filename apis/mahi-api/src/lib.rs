// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Mahi (Manta Auth Cache) API trait definitions.
//!
//! Mahi ships two independent Restify servers from the same repo:
//!
//! * The public `mahi` service (port 8080), modeled by [`MahiApi`]. It serves
//!   lookup endpoints used by CloudAPI / Manta / node-mahi plus AWS SigV4,
//!   STS, and IAM endpoints used by Manta SigV4 clients.
//! * The internal `mahi-sitter` replicator admin server, modeled by
//!   [`MahiSitterApi`], which exposes `GET /ping` and `GET /snapshot` (a
//!   binary-streaming endpoint used to ship the backing RDB snapshot).
//!
//! The two services live on different ports with different consumers; they
//! are modeled as two separate traits in the same crate so they can share the
//! common type modules under [`types`].
//!
//! # JSON Field Naming
//!
//! Mahi inherits UFDS attribute names verbatim on its Redis-backed lookup
//! blobs (mostly lowercase / snake_case). The AWS-emulating STS and IAM
//! endpoints use PascalCase response envelopes with lowerCamelCase request
//! bodies. `ListRolesResponse` mixes casings in a single object: lowercase
//! `roles` alongside PascalCase `IsTruncated`/`Marker`. See the individual
//! type modules for the exact attribute decisions.
//!
//! # Notable Endpoint Shapes
//!
//! * `GET /ping` (both services) returns 204 No Content on success.
//! * `POST /sts/get-caller-identity` returns raw XML with
//!   `Content-Type: text/xml`; use `Result<Response<Body>, HttpError>` and
//!   apply the Phase-2b spec patch to rewrite the response schema.
//! * `GET /snapshot` (sitter) streams an `application/octet-stream` binary
//!   body; same treatment as above.
//! * `POST /aws-verify` treats its request body as opaque (only `?method=`
//!   and `?url=` plus raw forwarded headers matter); the service
//!   implementation will need header access via `RequestContext::request`.

use dropshot::{
    Body, HttpError, HttpResponseOk, HttpResponseUpdatedNoContent, Path, Query, RequestContext,
    TypedBody,
};
use http::Response;
use std::collections::HashMap;

pub mod types;
pub use types::*;

// ============================================================================
// MahiApi — the public `mahi` server on port 8080
// ============================================================================

/// Mahi (Manta Auth Cache) public API.
///
/// Groups:
/// * `lookup` — classic auth-cache lookup routes used by node-mahi.
/// * `lookup-deprecated` — legacy paths retained for backward compat.
/// * `aws-sigv4` — access-key resolution and SigV4 signature verification.
/// * `sts` — AWS STS-compatible endpoints (Manta-only).
/// * `iam` — AWS IAM-compatible endpoints (Manta-only).
#[dropshot::api_description]
pub trait MahiApi {
    type Context: Send + Sync + 'static;

    // ------------------------------------------------------------------------
    // Lookup endpoints
    // ------------------------------------------------------------------------

    /// Health check. Returns 204 on success; 503 if Redis is unavailable or
    /// the replicator is not caught up.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["lookup"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Look up an account by UUID. Returns `AuthInfo` populated with account
    /// metadata and the account's roles.
    #[endpoint {
        method = GET,
        path = "/accounts/{accountid}",
        tags = ["lookup"],
    }]
    async fn get_account_by_uuid(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountIdPath>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Look up an account by login (`?login=` or legacy alias `?account=`).
    #[endpoint {
        method = GET,
        path = "/accounts",
        tags = ["lookup"],
    }]
    async fn get_account(
        rqctx: RequestContext<Self::Context>,
        query: Query<GetAccountQuery>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Look up a sub-user by UUID. Also loads the owning account.
    #[endpoint {
        method = GET,
        path = "/users/{userid}",
        tags = ["lookup"],
    }]
    async fn get_user_by_uuid(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserIdPath>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Look up a sub-user by (account-login, sub-user-login).
    ///
    /// When `fallback=true` (the upstream default) and the sub-user is
    /// missing, the handler returns the account-only `AuthInfo` with
    /// `user=None` and an empty `roles` map.
    #[endpoint {
        method = GET,
        path = "/users",
        tags = ["lookup"],
    }]
    async fn get_user(
        rqctx: RequestContext<Self::Context>,
        query: Query<GetUserQuery>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// List members of a role identified by `(account, role-name)`. The
    /// returned `AuthInfo.role` field is populated for this endpoint.
    #[endpoint {
        method = GET,
        path = "/roles",
        tags = ["lookup"],
    }]
    async fn get_role_members(
        rqctx: RequestContext<Self::Context>,
        query: Query<GetRolesQuery>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Resolve a set of names to their UUIDs within an account. Returns
    /// `{account}` alone when no `name=` query is supplied.
    #[endpoint {
        method = GET,
        path = "/uuids",
        tags = ["lookup"],
    }]
    async fn name_to_uuid(
        rqctx: RequestContext<Self::Context>,
        query: Query<NameToUuidQuery>,
    ) -> Result<HttpResponseOk<NameToUuidResponse>, HttpError>;

    /// Resolve a set of UUIDs to their names/logins. The response is a map
    /// keyed by UUID, not an array.
    #[endpoint {
        method = GET,
        path = "/names",
        tags = ["lookup"],
    }]
    async fn uuid_to_name(
        rqctx: RequestContext<Self::Context>,
        query: Query<UuidToNameQuery>,
    ) -> Result<HttpResponseOk<HashMap<String, String>>, HttpError>;

    /// Bulk lookup of all known accounts. Response is a map keyed by account
    /// UUID, not an array.
    #[endpoint {
        method = GET,
        path = "/lookup",
        tags = ["lookup"],
    }]
    async fn lookup(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HashMap<String, LookupEntry>>, HttpError>;

    // ------------------------------------------------------------------------
    // Deprecated lookup endpoints (kept for node-mahi v1 clients)
    // ------------------------------------------------------------------------

    /// Deprecated alias for `/accounts?login=`.
    #[endpoint {
        method = GET,
        path = "/account/{account}",
        tags = ["lookup-deprecated"],
    }]
    async fn get_account_old(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyAccountPath>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Deprecated alias for `/users?account=&login=`.
    #[endpoint {
        method = GET,
        path = "/user/{account}/{user}",
        tags = ["lookup-deprecated"],
    }]
    async fn get_user_old(
        rqctx: RequestContext<Self::Context>,
        path: Path<LegacyUserPath>,
    ) -> Result<HttpResponseOk<AuthInfo>, HttpError>;

    /// Deprecated POST-body variant of `GET /uuids`.
    #[endpoint {
        method = POST,
        path = "/getUuid",
        tags = ["lookup-deprecated"],
    }]
    async fn name_to_uuid_old(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<NameToUuidBody>,
    ) -> Result<HttpResponseOk<NameToUuidResponse>, HttpError>;

    /// Deprecated POST-body variant of `GET /names`.
    #[endpoint {
        method = POST,
        path = "/getName",
        tags = ["lookup-deprecated"],
    }]
    async fn uuid_to_name_old(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<UuidToNameBody>,
    ) -> Result<HttpResponseOk<HashMap<String, String>>, HttpError>;

    // ------------------------------------------------------------------------
    // AWS SigV4 endpoints
    // ------------------------------------------------------------------------

    /// Resolve a principal by AWS access key id. Falls back to UFDS for
    /// MSAR/MSTS temporary credentials.
    #[endpoint {
        method = GET,
        path = "/aws-auth/{accesskeyid}",
        tags = ["aws-sigv4"],
    }]
    async fn get_user_by_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccessKeyIdPath>,
    ) -> Result<HttpResponseOk<AwsAuthResult>, HttpError>;

    /// Verify an AWS SigV4 signature over the original client request.
    ///
    /// The original method / URL are delivered in the query string. The
    /// service implementation must inspect the incoming request's headers
    /// directly (via `RequestContext::request`); the request body is opaque
    /// here.
    #[endpoint {
        method = POST,
        path = "/aws-verify",
        tags = ["aws-sigv4"],
    }]
    async fn verify_sig_v4(
        rqctx: RequestContext<Self::Context>,
        query: Query<SigV4VerifyQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<SigV4VerifyResult>, HttpError>;

    // ------------------------------------------------------------------------
    // STS endpoints (Manta-only; return 501 on sdc deployments)
    // ------------------------------------------------------------------------

    /// Mint MSAR-prefixed temporary credentials after validating the trust
    /// policy of the target role.
    #[endpoint {
        method = POST,
        path = "/sts/assume-role",
        tags = ["sts"],
    }]
    async fn sts_assume_role(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<AssumeRoleRequest>,
    ) -> Result<HttpResponseOk<AssumeRoleResponse>, HttpError>;

    /// Mint MSTS-prefixed temporary credentials for the caller.
    #[endpoint {
        method = POST,
        path = "/sts/get-session-token",
        tags = ["sts"],
    }]
    async fn sts_get_session_token(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<GetSessionTokenRequest>,
    ) -> Result<HttpResponseOk<GetSessionTokenResponse>, HttpError>;

    /// Return caller-identity information. The wire response is XML with
    /// `Content-Type: text/xml`, so this endpoint uses `Response<Body>` and
    /// relies on a Phase-2b OpenAPI spec patch to declare the correct
    /// content type and a `string` schema.
    #[endpoint {
        method = POST,
        path = "/sts/get-caller-identity",
        tags = ["sts"],
    }]
    async fn sts_get_caller_identity(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<GetCallerIdentityRequest>,
    ) -> Result<Response<Body>, HttpError>;

    // ------------------------------------------------------------------------
    // IAM endpoints (Manta-only; return 501 on sdc deployments)
    // ------------------------------------------------------------------------

    /// Create a new IAM role. Writes Redis synchronously, UFDS asynchronously.
    /// Returns 200 (despite the upstream JSDoc claiming 201).
    #[endpoint {
        method = POST,
        path = "/iam/create-role",
        tags = ["iam"],
    }]
    async fn iam_create_role(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateRoleRequest>,
    ) -> Result<HttpResponseOk<CreateRoleResponse>, HttpError>;

    /// Get a single role by name.
    #[endpoint {
        method = GET,
        path = "/iam/get-role/{role_name}",
        tags = ["iam"],
    }]
    async fn iam_get_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleNamePath>,
        query: Query<AccountUuidQuery>,
    ) -> Result<HttpResponseOk<GetRoleResponse>, HttpError>;

    /// Attach a policy to a role.
    #[endpoint {
        method = POST,
        path = "/iam/put-role-policy",
        tags = ["iam"],
    }]
    async fn iam_put_role_policy(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<PutRolePolicyRequest>,
    ) -> Result<HttpResponseOk<PutRolePolicyResponse>, HttpError>;

    /// Delete a role. Writes Redis synchronously, UFDS asynchronously.
    #[endpoint {
        method = DELETE,
        path = "/iam/delete-role/{role_name}",
        tags = ["iam"],
    }]
    async fn iam_delete_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleNamePath>,
        query: Query<AccountUuidQuery>,
    ) -> Result<HttpResponseOk<DeleteRoleResponse>, HttpError>;

    /// Detach a policy from a role. All identifiers come from the query
    /// string, not the path (mirrors upstream).
    #[endpoint {
        method = DELETE,
        path = "/iam/delete-role-policy",
        tags = ["iam"],
    }]
    async fn iam_delete_role_policy(
        rqctx: RequestContext<Self::Context>,
        query: Query<DeleteRolePolicyQuery>,
    ) -> Result<HttpResponseOk<DeleteRolePolicyResponse>, HttpError>;

    /// List roles within an account.
    #[endpoint {
        method = GET,
        path = "/iam/list-roles",
        tags = ["iam"],
    }]
    async fn iam_list_roles(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListRolesQuery>,
    ) -> Result<HttpResponseOk<ListRolesResponse>, HttpError>;

    /// List policies attached to a role. Note the lowercase `maxitems` query
    /// parameter (an upstream quirk; `maxItems` is accepted as an alias).
    #[endpoint {
        method = GET,
        path = "/iam/list-role-policies/{role_name}",
        tags = ["iam"],
    }]
    async fn iam_list_role_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleNamePath>,
        query: Query<ListRolePoliciesQuery>,
    ) -> Result<HttpResponseOk<ListRolePoliciesResponse>, HttpError>;

    /// Retrieve a specific role policy document (as a JSON-encoded string).
    #[endpoint {
        method = GET,
        path = "/iam/get-role-policy/{role_name}/{policy_name}",
        tags = ["iam"],
    }]
    async fn iam_get_role_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePolicyPath>,
        query: Query<AccountUuidQuery>,
    ) -> Result<HttpResponseOk<GetRolePolicyResponse>, HttpError>;
}

// ============================================================================
// MahiSitterApi — the internal replicator admin server
// ============================================================================

/// Mahi replicator sitter admin API.
///
/// Runs on its own port alongside the replicator process. Provides a
/// health-check ping and a binary-streaming `/snapshot` endpoint used to ship
/// the backing Redis RDB snapshot to peers.
#[dropshot::api_description]
pub trait MahiSitterApi {
    type Context: Send + Sync + 'static;

    /// Health check. Returns 204 on success; 500/503 if the sitter is not
    /// caught up or Redis is unavailable.
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["sitter"],
    }]
    async fn sitter_ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Stream the Redis `dump.rdb` snapshot as `application/octet-stream`.
    ///
    /// Upstream terminates the response with `res.send(201)` after piping the
    /// file into the socket. The trait uses `Result<Response<Body>, HttpError>`
    /// and relies on a Phase-2b spec patch to declare the binary content
    /// type and 201 status.
    #[endpoint {
        method = GET,
        path = "/snapshot",
        tags = ["sitter"],
    }]
    async fn sitter_snapshot(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;
}

// ============================================================================
// Path param types local to the AWS-SigV4 endpoints
// ============================================================================

/// Path parameter for `GET /aws-auth/{accesskeyid}`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AccessKeyIdPath {
    pub accesskeyid: String,
}
