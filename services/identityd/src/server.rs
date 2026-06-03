// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The [`IdentitydApi`] implementation over an [`IdentityStore`].

use std::sync::Arc;

use chrono::{Duration, Utc};
use dropshot::{HttpError, HttpResponseOk, Path, RequestContext, TypedBody};
use identity_store::{GrantType, IdentityStore, RealmScope, Role, StoreError, User};
use identity_store::types::{AssignmentSubject, AssignmentTarget};
use identity_token::AccessClaims;
use identity_token::claims::RealmScope as TokenRealmScope;
use identityd_api::{
    HealthResponse, IdentitydApi, OpenIdConfiguration, RealmPath, TokenRequest, TokenResponse,
    UserInfo,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use uuid::Uuid;

use crate::identifiers;
use crate::keys::SigningMaterial;

/// Shared server state.
pub struct Ctx {
    pub store: Arc<dyn IdentityStore>,
    pub signing: SigningMaterial,
}

impl Ctx {
    /// Resolve a realm-id path segment to its store record.
    ///
    /// The wire contract pins the tenant and system realm ids, but the
    /// MemStore assigns its own ids at seed time. We map the pinned path
    /// id to the store record via the (also pinned) issuer URL, so every
    /// token's `iss`/`realm` claim and every `/realms/{realm}/...` path
    /// uses the contract ids while the store stays untouched.
    async fn resolve_realm(&self, path_realm: Uuid) -> Result<identity_store::Realm, HttpError> {
        if path_realm != identifiers::TENANT_REALM_ID && path_realm != identifiers::SYSTEM_REALM_ID {
            return Err(HttpError::for_not_found(
                None,
                format!("unknown realm {path_realm}"),
            ));
        }
        let issuer = identifiers::realm_issuer_url(path_realm);
        self.store
            .get_realm_by_issuer(&issuer)
            .await
            .map_err(store_err_to_http)
    }
}

/// Map a store error to an HTTP status. NotFound→404, Conflict→409,
/// Backend→500.
pub(crate) fn store_err_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_not_found(None, "not found".to_string()),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            None,
            dropshot::ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        StoreError::Backend(msg) => {
            HttpError::for_internal_error(format!("store backend error: {msg}"))
        }
    }
}

/// A fixed bcrypt hash used to spend constant time on the unknown-user
/// password path, so that path costs roughly the same as a real
/// wrong-password verify and does not leak username existence by timing.
///
/// Computed once at the default cost (matching real password hashes) so
/// the dummy verify and a real verify do the same work. The plaintext it
/// hashes is irrelevant — it never matches a presented password (which is
/// fine; the unknown-user path always rejects regardless).
fn dummy_password_hash() -> &'static str {
    use std::sync::OnceLock;
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        bcrypt::hash("identityd-timing-equalizer", bcrypt::DEFAULT_COST)
            // A failure here would only weaken timing equalization, never
            // correctness, so fall back to a valid throwaway hash string.
            .unwrap_or_else(|_| {
                "$2b$12$0000000000000000000000.0000000000000000000000000000000000000"
                    .to_string()
            })
    })
    .as_str()
}

/// A 400 with an OAuth-style error code, kept generic so a caller can't
/// distinguish "no such user" from "bad password".
fn invalid_grant() -> HttpError {
    HttpError::for_client_error(
        Some("invalid_grant".to_string()),
        dropshot::ClientErrorStatusCode::BAD_REQUEST,
        "invalid credentials".to_string(),
    )
}

/// A 400 `unauthorized_client`: the client is not registered for the
/// requested grant type.
fn unauthorized_client() -> HttpError {
    HttpError::for_client_error(
        Some("unauthorized_client".to_string()),
        dropshot::ClientErrorStatusCode::BAD_REQUEST,
        "client is not authorized for this grant type".to_string(),
    )
}

/// Map a wire `grant_type` string to its [`GrantType`]. `None` means the
/// grant type is unknown to us (it then falls through to the dispatch's
/// `unsupported_grant_type` arm rather than the allow-list).
fn grant_type_of(wire: &str) -> Option<GrantType> {
    match wire {
        "authorization_code" => Some(GrantType::AuthorizationCode),
        "refresh_token" => Some(GrantType::RefreshToken),
        "urn:ietf:params:oauth:grant-type:device_code" => Some(GrantType::DeviceCode),
        "client_credentials" => Some(GrantType::ClientCredentials),
        "password" => Some(GrantType::Password),
        _ => None,
    }
}

/// A 401 for a missing/invalid bearer token at userinfo.
fn unauthorized() -> HttpError {
    HttpError::for_client_error(
        Some("invalid_token".to_string()),
        dropshot::ClientErrorStatusCode::UNAUTHORIZED,
        "invalid or missing access token".to_string(),
    )
}

/// Read the realm-scope tag for the token claim.
fn token_realm_scope(scope: &RealmScope) -> TokenRealmScope {
    match scope {
        RealmScope::Tenant { .. } => TokenRealmScope::Tenant,
        RealmScope::Silo { .. } => TokenRealmScope::Silo,
        RealmScope::System => TokenRealmScope::System,
        _ => TokenRealmScope::Unknown,
    }
}

/// The pinned realm id that the contract uses on the wire for a given
/// store realm (resolved back from its issuer URL).
fn pinned_realm_id(realm: &identity_store::Realm) -> Uuid {
    if realm.issuer_url == identifiers::realm_issuer_url(identifiers::SYSTEM_REALM_ID) {
        identifiers::SYSTEM_REALM_ID
    } else {
        identifiers::TENANT_REALM_ID
    }
}

/// Collect a user's group names (best-effort; empty on store failure).
async fn user_groups(store: &dyn IdentityStore, user_id: Uuid) -> Vec<String> {
    let Ok(group_ids) = store.list_groups_of_user(user_id).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for gid in group_ids {
        if let Ok(g) = store.get_group(gid).await {
            names.push(g.name);
        }
    }
    names
}

/// Whether `user` holds a `TenantMember`/`TenantAdmin` grant on the
/// demo tenant. Used only to populate the `tenant_id` claim.
async fn tenant_membership(store: &dyn IdentityStore, user_id: Uuid) -> Option<Uuid> {
    let subject = AssignmentSubject::User { user_id };
    let assignments = store.list_assignments_of_subject(&subject).await.ok()?;
    for a in assignments {
        if let AssignmentTarget::Tenant { tenant_id } = a.target
            && matches!(a.role, Role::TenantMember | Role::TenantAdmin)
        {
            return Some(tenant_id);
        }
    }
    None
}

/// Build the denormalized [`AccessClaims`] for `user` in `realm`.
async fn build_claims(
    ctx: &Ctx,
    realm: &identity_store::Realm,
    user: &User,
    scope: &str,
) -> AccessClaims {
    let now = Utc::now();
    let realm_id = pinned_realm_id(realm);
    let tenant_id = tenant_membership(ctx.store.as_ref(), user.id).await;
    let silo_id = match realm.scope {
        RealmScope::Silo { silo_id } => Some(silo_id),
        // Tenant realm: the demo tenant lives in the pinned silo.
        RealmScope::Tenant { .. } => Some(identifiers::SILO_ID),
        RealmScope::System => None,
        _ => None,
    };
    let groups = user_groups(ctx.store.as_ref(), user.id).await;

    // Fleet-wide operator flags may ONLY ride a token minted by the
    // System realm. A tenant- or silo-realm user record that carries
    // `is_root`/`fleet_admin` (whether by misconfiguration or a
    // create_user escalation) must never produce a fleet-privileged
    // token: that would let a tenant escalate to fleet root. Gate both
    // flags on the minting realm's scope here, and again at the tritond
    // consume site (defense in depth).
    let system_realm = matches!(realm.scope, RealmScope::System);
    let is_root = system_realm && user.is_root;
    let fleet_admin = system_realm && user.fleet_admin;

    AccessClaims {
        sub: user.id,
        iss: realm.issuer_url.clone(),
        // TODO(RFD 00021): mint a per-resource-server `aud` once a
        // resource-server audience design lands; verifiers can then bind
        // tokens to themselves. `None` today => no audience scoping.
        aud: None,
        exp: (now + Duration::seconds(identifiers::ACCESS_TTL_SECS)).timestamp(),
        iat: now.timestamp(),
        nbf: None,
        realm: realm_id,
        realm_scope: token_realm_scope(&realm.scope),
        tenant_id,
        silo_id,
        is_root,
        fleet_admin,
        groups,
        scope: Some(scope.to_string()),
        cnf: None,
    }
}

/// Sign `claims` as an RS256 JWT with the dev key id.
fn sign_access(key: &EncodingKey, claims: &AccessClaims) -> Result<String, HttpError> {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(identifiers::SIGNING_KID.to_string());
    encode(&header, claims, key)
        .map_err(|e| HttpError::for_internal_error(format!("sign access token: {e}")))
}

/// Verify the confidential client's credentials against the realm.
async fn verify_client(
    ctx: &Ctx,
    realm: &identity_store::Realm,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<(), HttpError> {
    let clients = ctx
        .store
        .list_oauth_clients_in_realm(realm.id)
        .await
        .map_err(store_err_to_http)?;
    let client = clients
        .into_iter()
        .find(|c| c.name == client_id)
        .ok_or_else(invalid_grant)?;
    match (&client.client_secret_hash, client_secret) {
        (Some(hash), Some(provided)) => {
            let ok = bcrypt::verify(provided, hash).unwrap_or(false);
            if ok {
                Ok(())
            } else {
                Err(invalid_grant())
            }
        }
        // Public client (no secret on file).
        (None, _) => Ok(()),
        // Confidential client but no secret presented.
        (Some(_), None) => Err(invalid_grant()),
    }
}

/// Mint a refresh token (an opaque random string) and persist it.
///
/// `family_id` ties a rotation chain together: the first token in a
/// login mints a new family (`None`), and every rotation threads the
/// parent's family forward so a replayed token can revoke the whole
/// chain (RFC 6819 §5.2.2.3 refresh-token theft detection).
async fn issue_refresh(
    ctx: &Ctx,
    realm: &identity_store::Realm,
    client_id: Uuid,
    user_id: Uuid,
    scope: &str,
    family_id: Option<Uuid>,
) -> Result<String, HttpError> {
    use base64::Engine;
    let jti = Uuid::new_v4();
    let family_id = family_id.unwrap_or_else(Uuid::new_v4);
    let now = Utc::now();
    let rt = identity_store::RefreshToken {
        jti,
        realm_id: realm.id,
        client_id,
        user_id,
        scope: scope.to_string(),
        granted_tenant: None,
        family_id,
        revoked: false,
        expires_at: now + Duration::seconds(identifiers::REFRESH_TTL_SECS),
        created_at: now,
    };
    ctx.store
        .put_refresh_token(rt)
        .await
        .map_err(store_err_to_http)?;
    // The opaque token is the jti; refresh rotation swaps it for a fresh
    // jti, so it is single-use even though it is just the id here.
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(jti.as_bytes()))
}

/// Decode the opaque refresh token back to its jti.
fn refresh_jti(token: &str) -> Option<Uuid> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()?;
    Uuid::from_slice(&bytes).ok()
}

/// identityd implementation type.
pub enum IdentitydImpl {}

impl IdentitydApi for IdentitydImpl {
    type Context = Arc<Ctx>;

    async fn healthz(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<HealthResponse>, HttpError> {
        Ok(HttpResponseOk(HealthResponse {
            status: "ok".to_string(),
        }))
    }

    async fn openid_configuration(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<OpenIdConfiguration>, HttpError> {
        let ctx = rqctx.context();
        let realm = ctx.resolve_realm(path.into_inner().realm).await?;
        let pinned = pinned_realm_id(&realm);
        let base = identifiers::realm_issuer_url(pinned);
        Ok(HttpResponseOk(OpenIdConfiguration {
            issuer: base.clone(),
            jwks_uri: format!("{base}/jwks"),
            token_endpoint: format!("{base}/token"),
            userinfo_endpoint: format!("{base}/userinfo"),
        }))
    }

    async fn jwks(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
        let ctx = rqctx.context();
        // Validate the realm exists; the published key is the same dev
        // key for every realm in this minimal provider.
        ctx.resolve_realm(path.into_inner().realm).await?;
        Ok(HttpResponseOk(serde_json::json!({
            "keys": [ctx.signing.public_jwk.clone()],
        })))
    }

    async fn token(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
        body: TypedBody<TokenRequest>,
    ) -> Result<HttpResponseOk<TokenResponse>, HttpError> {
        let ctx = rqctx.context();
        let realm = ctx.resolve_realm(path.into_inner().realm).await?;
        let req = body.into_inner();

        verify_client(
            ctx,
            &realm,
            &req.client_id,
            req.client_secret.as_deref(),
        )
        .await?;

        let client = ctx
            .store
            .list_oauth_clients_in_realm(realm.id)
            .await
            .map_err(store_err_to_http)?
            .into_iter()
            .find(|c| c.name == req.client_id)
            .ok_or_else(invalid_grant)?;

        // Per-client grant_type allow-list: reject any grant the client is
        // not registered for before dispatching. An unknown grant string
        // falls through to the dispatch's `unsupported_grant_type` arm.
        if let Some(requested) = grant_type_of(req.grant_type.as_str())
            && !client.grant_types.contains(&requested)
        {
            return Err(unauthorized_client());
        }

        let scope = req
            .scope
            .clone()
            .unwrap_or_else(|| "openid profile".to_string());

        match req.grant_type.as_str() {
            "password" => {
                let username = req.username.as_deref().ok_or_else(invalid_grant)?;
                let password = req.password.as_deref().ok_or_else(invalid_grant)?;
                // Verify against the real hash if the user exists, else
                // against a fixed precomputed hash. Doing a bcrypt verify
                // on the unknown-user path too keeps the unknown-user and
                // wrong-password timings comparable, closing the username
                // enumeration oracle. Both paths return the identical
                // `invalid_grant`.
                let user = ctx.store.get_user_by_username(realm.id, username).await.ok();
                let hash = user
                    .as_ref()
                    .map_or_else(|| dummy_password_hash().to_string(), |u| u.password_hash.clone());
                let verified = bcrypt::verify(password, &hash).unwrap_or(false);
                let Some(user) = user else {
                    return Err(invalid_grant());
                };
                if !verified {
                    return Err(invalid_grant());
                }
                let claims = build_claims(ctx, &realm, &user, &scope).await;
                let access = sign_access(&ctx.signing.encoding_key, &claims)?;
                // New login: start a fresh rotation family.
                let refresh =
                    issue_refresh(ctx, &realm, client.id, user.id, &scope, None).await?;
                Ok(HttpResponseOk(TokenResponse {
                    access_token: access,
                    token_type: "Bearer".to_string(),
                    expires_in: identifiers::ACCESS_TTL_SECS,
                    refresh_token: Some(refresh),
                    scope,
                }))
            }
            "refresh_token" => {
                let presented = req.refresh_token.as_deref().ok_or_else(invalid_grant)?;
                let jti = refresh_jti(presented).ok_or_else(invalid_grant)?;
                let stored = ctx
                    .store
                    .get_refresh_token(jti)
                    .await
                    .map_err(|_| invalid_grant())?;
                if stored.expires_at < Utc::now() {
                    return Err(invalid_grant());
                }
                // Replay: an already-revoked (consumed) token is being
                // presented again. Per RFC 6819 §5.2.2.3, treat this as
                // theft and revoke the whole rotation family so the
                // attacker (and the victim's stolen chain) are both cut
                // off, then reject.
                if stored.revoked {
                    ctx.store
                        .revoke_refresh_family(stored.family_id)
                        .await
                        .map_err(store_err_to_http)?;
                    return Err(invalid_grant());
                }
                // Rotate: revoke the presented token, mint a fresh one in
                // the same family so a future replay can be detected.
                ctx.store
                    .revoke_refresh_token(jti)
                    .await
                    .map_err(store_err_to_http)?;
                let user = ctx
                    .store
                    .get_user(stored.user_id)
                    .await
                    .map_err(|_| invalid_grant())?;
                let claims = build_claims(ctx, &realm, &user, &stored.scope).await;
                let access = sign_access(&ctx.signing.encoding_key, &claims)?;
                let refresh = issue_refresh(
                    ctx,
                    &realm,
                    client.id,
                    user.id,
                    &stored.scope,
                    Some(stored.family_id),
                )
                .await?;
                Ok(HttpResponseOk(TokenResponse {
                    access_token: access,
                    token_type: "Bearer".to_string(),
                    expires_in: identifiers::ACCESS_TTL_SECS,
                    refresh_token: Some(refresh),
                    scope: stored.scope,
                }))
            }
            "client_credentials" => {
                // Workload token: subject is the client id; no refresh.
                let now = Utc::now();
                let claims = AccessClaims {
                    sub: client.id,
                    iss: realm.issuer_url.clone(),
                    aud: None,
                    exp: (now + Duration::seconds(identifiers::ACCESS_TTL_SECS)).timestamp(),
                    iat: now.timestamp(),
                    nbf: None,
                    realm: pinned_realm_id(&realm),
                    realm_scope: token_realm_scope(&realm.scope),
                    tenant_id: Some(identifiers::TENANT_ID),
                    silo_id: Some(identifiers::SILO_ID),
                    is_root: false,
                    fleet_admin: false,
                    groups: vec![],
                    scope: Some(scope.clone()),
                    cnf: None,
                };
                let access = sign_access(&ctx.signing.encoding_key, &claims)?;
                Ok(HttpResponseOk(TokenResponse {
                    access_token: access,
                    token_type: "Bearer".to_string(),
                    expires_in: identifiers::ACCESS_TTL_SECS,
                    refresh_token: None,
                    scope,
                }))
            }
            other => Err(HttpError::for_client_error(
                Some("unsupported_grant_type".to_string()),
                dropshot::ClientErrorStatusCode::BAD_REQUEST,
                format!("unsupported grant_type {other}"),
            )),
        }
    }

    async fn userinfo(
        rqctx: RequestContext<Self::Context>,
        path: Path<RealmPath>,
    ) -> Result<HttpResponseOk<UserInfo>, HttpError> {
        let ctx = rqctx.context();
        let realm = ctx.resolve_realm(path.into_inner().realm).await?;

        let bearer = rqctx
            .request
            .headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;

        // Verify the token against our own public key.
        let claims = verify_access_local(ctx, &realm, bearer).await?;

        let user = ctx
            .store
            .get_user(claims.sub)
            .await
            .map_err(|_| unauthorized())?;

        // Reflect the same realm-gating the token does: fleet flags are
        // meaningful only in the System realm, so userinfo must not report
        // a tenant user as fleet root even if the store record says so.
        let system_realm = matches!(realm.scope, RealmScope::System);
        Ok(HttpResponseOk(UserInfo {
            sub: user.id,
            preferred_username: user.username,
            email: user.email,
            name: user.display_name,
            realm: claims.realm,
            realm_scope: realm.scope.tag().to_string(),
            tenant_id: claims.tenant_id,
            silo_id: claims.silo_id,
            is_root: system_realm && user.is_root,
            fleet_admin: system_realm && user.fleet_admin,
            groups: claims.groups,
        }))
    }

    // ---- Admin surface (`/v1/...`). Bodies live in `crate::admin`. ----

    async fn admin_list_realms(
        rqctx: RequestContext<Self::Context>,
        query: dropshot::Query<identityd_api::ListRealmsQuery>,
    ) -> Result<HttpResponseOk<Vec<identityd_api::RealmView>>, HttpError> {
        Self::admin_list_realms_impl(rqctx, query).await
    }

    async fn admin_get_realm(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
    ) -> Result<HttpResponseOk<identityd_api::RealmView>, HttpError> {
        Self::admin_get_realm_impl(rqctx, path).await
    }

    async fn admin_create_tenant_realm(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminTenantPath>,
    ) -> Result<HttpResponseOk<identityd_api::RealmView>, HttpError> {
        Self::admin_create_tenant_realm_impl(rqctx, path).await
    }

    async fn admin_list_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<identityd_api::UserView>>, HttpError> {
        Self::admin_list_users_impl(rqctx, path).await
    }

    async fn admin_create_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
        body: TypedBody<identityd_api::CreateUserRequest>,
    ) -> Result<HttpResponseOk<identityd_api::UserView>, HttpError> {
        Self::admin_create_user_impl(rqctx, path, body).await
    }

    async fn admin_get_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminUserPath>,
    ) -> Result<HttpResponseOk<identityd_api::UserView>, HttpError> {
        Self::admin_get_user_impl(rqctx, path).await
    }

    async fn admin_update_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminUserPath>,
        body: TypedBody<identityd_api::UpdateUserRequest>,
    ) -> Result<HttpResponseOk<identityd_api::UserView>, HttpError> {
        Self::admin_update_user_impl(rqctx, path, body).await
    }

    async fn admin_set_user_password(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminUserPath>,
        body: TypedBody<identityd_api::SetPasswordRequest>,
    ) -> Result<HttpResponseOk<identityd_api::UserView>, HttpError> {
        Self::admin_set_user_password_impl(rqctx, path, body).await
    }

    async fn admin_delete_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminUserPath>,
    ) -> Result<dropshot::HttpResponseDeleted, HttpError> {
        Self::admin_delete_user_impl(rqctx, path).await
    }

    async fn admin_list_role_assignments(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<identityd_api::RoleAssignmentView>>, HttpError> {
        Self::admin_list_role_assignments_impl(rqctx, path).await
    }

    async fn admin_create_role_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
        body: TypedBody<identityd_api::CreateRoleAssignmentRequest>,
    ) -> Result<HttpResponseOk<identityd_api::RoleAssignmentView>, HttpError> {
        Self::admin_create_role_assignment_impl(rqctx, path, body).await
    }

    async fn admin_delete_role_assignment(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminAssignmentPath>,
    ) -> Result<dropshot::HttpResponseDeleted, HttpError> {
        Self::admin_delete_role_assignment_impl(rqctx, path).await
    }

    async fn admin_get_identity_source(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
    ) -> Result<HttpResponseOk<identityd_api::IdentitySourceView>, HttpError> {
        Self::admin_get_identity_source_impl(rqctx, path).await
    }

    async fn admin_list_connections(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<identityd_api::ConnectionView>>, HttpError> {
        Self::admin_list_connections_impl(rqctx, path).await
    }

    async fn admin_create_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminRealmPath>,
        body: TypedBody<identityd_api::CreateConnectionRequest>,
    ) -> Result<HttpResponseOk<identityd_api::ConnectionView>, HttpError> {
        Self::admin_create_connection_impl(rqctx, path, body).await
    }

    async fn admin_patch_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminConnectionPath>,
        body: TypedBody<identityd_api::PatchConnectionRequest>,
    ) -> Result<HttpResponseOk<identityd_api::ConnectionView>, HttpError> {
        Self::admin_patch_connection_impl(rqctx, path, body).await
    }

    async fn admin_delete_connection(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminConnectionPath>,
    ) -> Result<dropshot::HttpResponseDeleted, HttpError> {
        Self::admin_delete_connection_impl(rqctx, path).await
    }

    async fn admin_put_claim_mappings(
        rqctx: RequestContext<Self::Context>,
        path: Path<identityd_api::AdminConnectionPath>,
        body: TypedBody<identityd_api::PutClaimMappingsRequest>,
    ) -> Result<HttpResponseOk<Vec<identityd_api::ClaimMappingView>>, HttpError> {
        Self::admin_put_claim_mappings_impl(rqctx, path, body).await
    }
}

/// Verify a locally minted access token without a pre-resolved realm.
///
/// The admin surface receives tokens whose `iss` could be any seeded
/// realm's issuer URL. We read the (unverified) `iss`, confirm it names a
/// realm this provider serves, then verify the signature and bind `iss`
/// to that realm — so a token forged for an unknown/foreign issuer is
/// rejected before any authorization decision is made.
pub(crate) async fn verify_token_with_realms(
    ctx: &Ctx,
    token: &str,
) -> Result<AccessClaims, HttpError> {
    let issuer = identity_token::peek_issuer(token).map_err(|_| unauthorized())?;
    let realm = ctx
        .store
        .get_realm_by_issuer(&issuer)
        .await
        .map_err(|_| unauthorized())?;
    verify_access_local(ctx, &realm, token).await
}

/// Verify a locally minted access token against this provider's own
/// public key and the realm's issuer.
async fn verify_access_local(
    ctx: &Ctx,
    realm: &identity_store::Realm,
    token: &str,
) -> Result<AccessClaims, HttpError> {
    use jsonwebtoken::jwk::Jwk;
    use jsonwebtoken::{DecodingKey, Validation, decode};

    let jwk: Jwk = serde_json::from_value(ctx.signing.public_jwk.clone())
        .map_err(|_| HttpError::for_internal_error("bad public jwk".to_string()))?;
    let key = DecodingKey::from_jwk(&jwk)
        .map_err(|e| HttpError::for_internal_error(format!("decoding key: {e}")))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[realm.issuer_url.as_str()]);
    // Reject future-dated tokens (off by default in jsonwebtoken).
    validation.validate_nbf = true;
    // TODO(RFD 00021): per-resource-server audience. We mint `aud: None`,
    // so there is nothing to enforce yet; require a userinfo audience
    // once resource-server audiences are designed.
    validation.validate_aud = false;
    let data =
        decode::<AccessClaims>(token, &key, &validation).map_err(|_| unauthorized())?;
    Ok(data.claims)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use identity_store::types::{NewRealm, NewSigningKey};
    use identity_store::{KeyStatus, MemStore, RedactedString, SigningAlg, UserStatus};

    fn test_ctx(store: Arc<dyn IdentityStore>) -> Ctx {
        Ctx {
            store,
            signing: crate::keys::load().expect("load dev signing key"),
        }
    }

    async fn seed_realm(store: &MemStore, scope: RealmScope, issuer: &str) -> identity_store::Realm {
        let now = Utc::now();
        store
            .create_realm(
                NewRealm {
                    scope,
                    name: "t".to_string(),
                    description: None,
                    issuer_url: issuer.to_string(),
                    signing_alg: Some(SigningAlg::Rs256),
                    access_token_ttl_secs: None,
                    id_token_ttl_secs: None,
                    refresh_token_ttl_secs: None,
                    auth_code_ttl_secs: None,
                    device_code_ttl_secs: None,
                    login_policy: None,
                },
                vec![NewSigningKey {
                    kid: identifiers::SIGNING_KID.to_string(),
                    alg: SigningAlg::Rs256,
                    private_pem: RedactedString::from("x".to_string()),
                    public_jwk: serde_json::json!({}),
                    status: KeyStatus::Active,
                    not_before: now,
                    not_after: now + Duration::days(1),
                }],
            )
            .await
            .expect("create realm")
    }

    fn root_user(realm_id: Uuid) -> User {
        User {
            id: Uuid::new_v4(),
            realm_id,
            username: "evil".to_string(),
            email: None,
            display_name: "evil".to_string(),
            password_hash: String::new(),
            is_root: true,
            fleet_admin: true,
            status: UserStatus::Active,
            mfa: None,
            brokered: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn tenant_realm_user_cannot_mint_root_token() {
        let store = MemStore::new();
        let realm = seed_realm(
            &store,
            RealmScope::Tenant {
                tenant_id: identifiers::TENANT_ID,
            },
            "http://127.0.0.1:8090/realms/tenant-test",
        )
        .await;
        let user = root_user(realm.id);
        let ctx = test_ctx(Arc::new(store));

        let claims = build_claims(&ctx, &realm, &user, "openid").await;
        assert!(
            !claims.is_root,
            "tenant realm must force is_root=false even when the user record is root"
        );
        assert!(
            !claims.fleet_admin,
            "tenant realm must force fleet_admin=false"
        );
    }

    #[tokio::test]
    async fn system_realm_user_keeps_root_token() {
        let store = MemStore::new();
        let realm = seed_realm(
            &store,
            RealmScope::System,
            "http://127.0.0.1:8090/realms/00000000-0000-4000-8000-000000000000",
        )
        .await;
        let user = root_user(realm.id);
        let ctx = test_ctx(Arc::new(store));

        let claims = build_claims(&ctx, &realm, &user, "openid").await;
        assert!(claims.is_root, "system realm preserves is_root");
        assert!(claims.fleet_admin, "system realm preserves fleet_admin");
    }
}
