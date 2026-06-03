// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The operator/admin surface (`/v1/...`) over an [`IdentityStore`].
//!
//! This is distinct from the public realm-scoped OIDC surface. Every
//! endpoint requires an identityd Bearer access token and enforces the
//! tenancy-isolation contract in [`authorize`]:
//!
//! * A System-realm token with `fleet_admin`/`is_root` manages ANY realm.
//! * A `Tenant`-scoped token whose bearer holds a `TenantAdmin` grant on
//!   tenant `T` manages ONLY the realm whose scope is `Tenant{T}`.
//! * Everyone else (TenantMember, ReadOnly, cross-tenant, …) gets 403.
//!
//! Cross-tenant isolation is the load-bearing property: a tenant-admin
//! token for tenant A must never read or mutate tenant B's realm, nor the
//! System realm.

use chrono::{Duration, Utc};
use dropshot::{
    HttpError, HttpResponseDeleted, HttpResponseOk, Path, Query, RequestContext, TypedBody,
};
use identity_store::types::{
    AssignmentSubject, AssignmentTarget, ClaimMapping, ConnectionKind, MappedField,
    NewRoleAssignment, NewUpstreamConnection, RedactedString,
};
use identity_store::{
    NewRealm, NewSigningKey, NewUser, Realm, RealmScope, Role, StoreError, UpstreamConnection, User,
    UserStatus,
};
use identity_token::AccessClaims;
use identity_token::claims::RealmScope as TokenRealmScope;
use identityd_api::{
    AdminAssignmentPath, AdminConnectionPath, AdminRealmPath, AdminTenantPath, AdminUserPath,
    AssignmentSubjectView, AssignmentTargetView, ClaimMappingView, ConnectionKindInput,
    ConnectionKindView, ConnectionView, CreateConnectionRequest, CreateRoleAssignmentRequest,
    CreateUserRequest, IdentitySourceConnection, IdentitySourceMode, IdentitySourceView,
    ListRealmsQuery, MappedFieldView, PatchConnectionRequest, PutClaimMappingsRequest, RealmScopeView,
    RealmView, RoleAssignmentView, RoleView, SetPasswordRequest, UpdateUserRequest, UserStatusView,
    UserView,
};
use uuid::Uuid;

use crate::identifiers;
use crate::server::{Ctx, IdentitydImpl, store_err_to_http, verify_token_with_realms};

// ===========================================================================
// Error helpers
// ===========================================================================

/// 401: the request had no usable Bearer access token.
fn unauthorized() -> HttpError {
    HttpError::for_client_error(
        Some("invalid_token".to_string()),
        dropshot::ClientErrorStatusCode::UNAUTHORIZED,
        "invalid or missing access token".to_string(),
    )
}

/// 403: a valid token that is not permitted to act on the target realm.
fn forbidden() -> HttpError {
    HttpError::for_client_error(
        Some("forbidden".to_string()),
        dropshot::ClientErrorStatusCode::FORBIDDEN,
        "not authorized for this realm".to_string(),
    )
}

// ===========================================================================
// Authz
// ===========================================================================

/// The authority a verified token carries on the admin surface.
enum Authority {
    /// May manage any realm.
    Fleet,
    /// May manage only the realm scoped to this tenant.
    Tenant(Uuid),
}

/// Extract and verify the Bearer access token from the request, then
/// resolve the caller's [`Authority`].
///
/// A token is verified against this provider's own JWKS, with its `iss`
/// pinned to one of the seeded realms (so a token forged for an unknown
/// issuer can't slip through). Fleet authority requires a System-realm
/// token carrying `fleet_admin`/`is_root`. Tenant authority additionally
/// requires the bearer to hold a `TenantAdmin` grant on its own tenant —
/// a `TenantMember` token resolves to no authority and is refused.
async fn caller_authority(ctx: &Ctx, rqctx_headers: &http::HeaderMap) -> Result<Authority, HttpError> {
    let bearer = rqctx_headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(unauthorized)?;

    let claims = verify_token_with_realms(ctx, bearer)
        .await
        .map_err(|_| unauthorized())?;

    // Fleet: a System-realm token with a fleet flag. `build_claims`
    // already gates these flags on the minting realm being System, so a
    // tenant-realm token can never present them — but we re-check the
    // scope here as defense in depth.
    if matches!(claims.realm_scope, TokenRealmScope::System)
        && (claims.fleet_admin || claims.is_root)
    {
        return Ok(Authority::Fleet);
    }

    // Tenant-admin: the token must be tenant-scoped, name a tenant, and the
    // bearer must actually hold a `TenantAdmin` grant on that tenant in the
    // store (the role is not in the token, so we consult the store).
    if matches!(claims.realm_scope, TokenRealmScope::Tenant)
        && let Some(tenant_id) = claims.tenant_id
        && bearer_is_tenant_admin(ctx, &claims, tenant_id).await
    {
        return Ok(Authority::Tenant(tenant_id));
    }

    Err(forbidden())
}

/// True if the token's subject holds a `TenantAdmin` grant targeting
/// `tenant_id`. The role lives in a store role-assignment, not in the
/// token, so this is the authoritative check.
async fn bearer_is_tenant_admin(ctx: &Ctx, claims: &AccessClaims, tenant_id: Uuid) -> bool {
    let subject = AssignmentSubject::User { user_id: claims.sub };
    let Ok(assignments) = ctx.store.list_assignments_of_subject(&subject).await else {
        return false;
    };
    assignments.iter().any(|a| {
        a.role == Role::TenantAdmin
            && matches!(a.target, AssignmentTarget::Tenant { tenant_id: t } if t == tenant_id)
    })
}

/// Resolve a realm-id path segment to its store record on the admin
/// surface, accepting EITHER the store-assigned id OR the pinned
/// wire-contract id.
///
/// identityd's MemStore/FdbStore assign their own v4 realm ids, but every
/// minted token's `realm` claim carries the PINNED id, and the BFF
/// forwards that pinned claim verbatim into `/v1/realms/{realm}/...`. The
/// public OIDC surface bridges pinned→store via the issuer URL
/// ([`Ctx::resolve_realm`]); this is the admin-surface equivalent so both
/// id forms address the same realm. We try the store id directly, then
/// fall back to the pinned→issuer mapping the OIDC surface uses.
async fn resolve_admin_realm(ctx: &Ctx, path_id: Uuid) -> Result<Realm, HttpError> {
    match ctx.store.get_realm(path_id).await {
        Ok(realm) => Ok(realm),
        Err(StoreError::NotFound) => {
            let issuer = identifiers::realm_issuer_url(path_id);
            ctx.store
                .get_realm_by_issuer(&issuer)
                .await
                .map_err(store_err_to_http)
        }
        Err(e) => Err(store_err_to_http(e)),
    }
}

/// Authorize the caller against a specific target realm, returning the
/// resolved realm record on success.
///
/// Fleet authority passes for every realm. Tenant authority passes only
/// when the target realm's scope is exactly `Tenant{caller_tenant}`. Any
/// other pairing (including a tenant-admin reaching for the System realm
/// or a sibling tenant's realm) is a 403 — the cross-tenant isolation
/// property.
async fn authorize_realm(
    ctx: &Ctx,
    headers: &http::HeaderMap,
    realm_id: Uuid,
) -> Result<Realm, HttpError> {
    let authority = caller_authority(ctx, headers).await?;
    let realm = resolve_admin_realm(ctx, realm_id).await?;
    match authority {
        Authority::Fleet => Ok(realm),
        Authority::Tenant(tenant_id) => match realm.scope {
            RealmScope::Tenant { tenant_id: t } if t == tenant_id => Ok(realm),
            _ => Err(forbidden()),
        },
    }
}

/// Authorize a fleet-only operation (no specific realm yet).
async fn authorize_fleet(ctx: &Ctx, headers: &http::HeaderMap) -> Result<(), HttpError> {
    match caller_authority(ctx, headers).await? {
        Authority::Fleet => Ok(()),
        Authority::Tenant(_) => Err(forbidden()),
    }
}

// ===========================================================================
// View conversions (store types -> wire types). Secrets are never carried.
// ===========================================================================

fn realm_scope_view(scope: &RealmScope) -> RealmScopeView {
    match scope {
        RealmScope::Tenant { tenant_id } => RealmScopeView::Tenant {
            tenant_id: *tenant_id,
        },
        RealmScope::Silo { silo_id } => RealmScopeView::Silo { silo_id: *silo_id },
        RealmScope::System => RealmScopeView::System,
        // The store enum is `#[non_exhaustive]`; a future scope surfaces as
        // `unknown` rather than silently mismapping to a real scope.
        _ => RealmScopeView::Unknown,
    }
}

fn realm_view(realm: &Realm) -> RealmView {
    RealmView {
        id: realm.id,
        scope: realm_scope_view(&realm.scope),
        name: realm.name.clone(),
        description: realm.description.clone(),
        issuer_url: realm.issuer_url.clone(),
        created_at: realm.created_at.to_rfc3339(),
    }
}

fn user_status_view(status: UserStatus) -> UserStatusView {
    match status {
        UserStatus::Active => UserStatusView::Active,
        UserStatus::Disabled => UserStatusView::Disabled,
    }
}

/// Never carries `password_hash`.
fn user_view(user: &User) -> UserView {
    UserView {
        id: user.id,
        realm_id: user.realm_id,
        username: user.username.clone(),
        email: user.email.clone(),
        display_name: user.display_name.clone(),
        status: user_status_view(user.status),
        brokered: user.brokered.is_some(),
        created_at: user.created_at.to_rfc3339(),
    }
}

fn role_view(role: Role) -> RoleView {
    match role {
        Role::TenantAdmin => RoleView::TenantAdmin,
        Role::TenantMember => RoleView::TenantMember,
        Role::SiloAdmin => RoleView::SiloAdmin,
        Role::FleetAdmin => RoleView::FleetAdmin,
        Role::Operator => RoleView::Operator,
        Role::ReadOnly => RoleView::ReadOnly,
        _ => RoleView::Unknown,
    }
}

/// A client sending `unknown` for a coarse role/subject/target is a 400
/// (it cannot name something this version doesn't model).
fn bad_enum(what: &str) -> HttpError {
    HttpError::for_bad_request(None, format!("unsupported {what}"))
}

fn role_from_view(role: RoleView) -> Result<Role, HttpError> {
    Ok(match role {
        RoleView::TenantAdmin => Role::TenantAdmin,
        RoleView::TenantMember => Role::TenantMember,
        RoleView::SiloAdmin => Role::SiloAdmin,
        RoleView::FleetAdmin => Role::FleetAdmin,
        RoleView::Operator => Role::Operator,
        RoleView::ReadOnly => Role::ReadOnly,
        RoleView::Unknown => return Err(bad_enum("role")),
    })
}

fn subject_view(subject: &AssignmentSubject) -> AssignmentSubjectView {
    match subject {
        AssignmentSubject::User { user_id } => AssignmentSubjectView::User { user_id: *user_id },
        AssignmentSubject::Group { group_id } => {
            AssignmentSubjectView::Group { group_id: *group_id }
        }
        _ => AssignmentSubjectView::Unknown,
    }
}

fn subject_from_view(
    subject: &AssignmentSubjectView,
) -> Result<AssignmentSubject, HttpError> {
    Ok(match subject {
        AssignmentSubjectView::User { user_id } => AssignmentSubject::User { user_id: *user_id },
        AssignmentSubjectView::Group { group_id } => {
            AssignmentSubject::Group { group_id: *group_id }
        }
        AssignmentSubjectView::Unknown => return Err(bad_enum("assignment subject")),
    })
}

fn target_view(target: &AssignmentTarget) -> AssignmentTargetView {
    match target {
        AssignmentTarget::Tenant { tenant_id } => AssignmentTargetView::Tenant {
            tenant_id: *tenant_id,
        },
        AssignmentTarget::Silo { silo_id } => AssignmentTargetView::Silo { silo_id: *silo_id },
        AssignmentTarget::Fleet => AssignmentTargetView::Fleet,
        _ => AssignmentTargetView::Unknown,
    }
}

fn target_from_view(
    target: &AssignmentTargetView,
) -> Result<AssignmentTarget, HttpError> {
    Ok(match target {
        AssignmentTargetView::Tenant { tenant_id } => AssignmentTarget::Tenant {
            tenant_id: *tenant_id,
        },
        AssignmentTargetView::Silo { silo_id } => AssignmentTarget::Silo { silo_id: *silo_id },
        AssignmentTargetView::Fleet => AssignmentTarget::Fleet,
        AssignmentTargetView::Unknown => return Err(bad_enum("assignment target")),
    })
}

fn assignment_view(a: &identity_store::RoleAssignment) -> RoleAssignmentView {
    RoleAssignmentView {
        id: a.id,
        realm_id: a.realm_id,
        subject: subject_view(&a.subject),
        target: target_view(&a.target),
        role: role_view(a.role),
        created_at: a.created_at.to_rfc3339(),
    }
}

fn mapped_field_view(f: MappedField) -> MappedFieldView {
    match f {
        MappedField::Username => MappedFieldView::Username,
        MappedField::Email => MappedFieldView::Email,
        MappedField::DisplayName => MappedFieldView::DisplayName,
        MappedField::Group => MappedFieldView::Group,
        _ => MappedFieldView::Unknown,
    }
}

fn mapped_field_from_view(f: MappedFieldView) -> Result<MappedField, HttpError> {
    Ok(match f {
        MappedFieldView::Username => MappedField::Username,
        MappedFieldView::Email => MappedField::Email,
        MappedFieldView::DisplayName => MappedField::DisplayName,
        MappedFieldView::Group => MappedField::Group,
        MappedFieldView::Unknown => return Err(bad_enum("claim-mapping target")),
    })
}

/// Redacts the OIDC `client_secret` (it becomes `client_secret_set: true`).
fn connection_kind_view(kind: &ConnectionKind) -> ConnectionKindView {
    match kind {
        ConnectionKind::Oidc {
            issuer_url,
            client_id,
            scopes,
            audience,
            // Deliberately not bound: the secret never crosses the wire.
            client_secret: _,
        } => ConnectionKindView::Oidc {
            issuer_url: issuer_url.clone(),
            client_id: client_id.clone(),
            scopes: scopes.clone(),
            audience: audience.clone(),
            client_secret_set: true,
        },
        ConnectionKind::Saml {
            idp_metadata,
            sp_entity_id,
            sp_acs_url,
            want_signed_assertions,
        } => ConnectionKindView::Saml {
            idp_metadata: idp_metadata.clone(),
            sp_entity_id: sp_entity_id.clone(),
            sp_acs_url: sp_acs_url.clone(),
            want_signed_assertions: *want_signed_assertions,
        },
        _ => ConnectionKindView::Unknown,
    }
}

fn connection_view(c: &UpstreamConnection) -> ConnectionView {
    ConnectionView {
        id: c.id,
        realm_id: c.realm_id,
        name: c.name.clone(),
        kind: connection_kind_view(&c.kind),
        enabled: c.enabled,
        created_at: c.created_at.to_rfc3339(),
    }
}

fn connection_kind_from_input(input: ConnectionKindInput) -> ConnectionKind {
    match input {
        ConnectionKindInput::Oidc {
            issuer_url,
            client_id,
            client_secret,
            scopes,
            audience,
        } => ConnectionKind::Oidc {
            issuer_url,
            client_id,
            client_secret: RedactedString::from(client_secret),
            scopes,
            audience,
        },
        ConnectionKindInput::Saml {
            idp_metadata,
            sp_entity_id,
            sp_acs_url,
            want_signed_assertions,
        } => ConnectionKind::Saml {
            idp_metadata,
            sp_entity_id,
            sp_acs_url,
            want_signed_assertions,
        },
    }
}

// ===========================================================================
// The admin endpoint implementations.
// ===========================================================================

impl IdentitydImpl {
    pub(crate) async fn admin_list_realms_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        query: Query<ListRealmsQuery>,
    ) -> Result<HttpResponseOk<Vec<RealmView>>, HttpError> {
        let ctx = rqctx.context();
        authorize_fleet(ctx, rqctx.request.headers()).await?;

        let scope_filter = parse_scope_filter(query.into_inner().scope.as_deref())?;
        let realms = ctx.store.list_realms().await.map_err(store_err_to_http)?;
        let views = realms
            .iter()
            .filter(|r| scope_filter.as_ref().is_none_or(|s| &r.scope == s))
            .map(realm_view)
            .collect();
        Ok(HttpResponseOk(views))
    }

    pub(crate) async fn admin_get_realm_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<RealmView>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        Ok(HttpResponseOk(realm_view(&realm)))
    }

    pub(crate) async fn admin_create_tenant_realm_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminTenantPath>,
    ) -> Result<HttpResponseOk<RealmView>, HttpError> {
        let ctx = rqctx.context();
        authorize_fleet(ctx, rqctx.request.headers()).await?;
        let tenant_id = path.into_inner().tenant_id;

        let scope = RealmScope::Tenant { tenant_id };
        // Idempotent: return the existing realm if one is already scoped to
        // this tenant.
        match ctx.store.get_realm_by_scope(&scope).await {
            Ok(realm) => return Ok(HttpResponseOk(realm_view(&realm))),
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(store_err_to_http(e)),
        }

        let issuer_url = identifiers::realm_issuer_url(tenant_id);
        let now = Utc::now();
        let key = NewSigningKey {
            kid: identifiers::SIGNING_KID.to_string(),
            alg: identity_store::SigningAlg::Rs256,
            private_pem: RedactedString::from(format!("embedded-dev-key:tenant:{tenant_id}")),
            public_jwk: ctx.signing.public_jwk.clone(),
            status: identity_store::KeyStatus::Active,
            not_before: now,
            not_after: now + Duration::days(3650),
        };
        let realm = ctx
            .store
            .create_realm(
                NewRealm {
                    scope,
                    name: format!("tenant-{tenant_id}"),
                    description: Some("Tenant realm".to_string()),
                    issuer_url,
                    signing_alg: Some(identity_store::SigningAlg::Rs256),
                    access_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                    id_token_ttl_secs: Some(identifiers::ACCESS_TTL_SECS as u32),
                    refresh_token_ttl_secs: Some(identifiers::REFRESH_TTL_SECS as u32),
                    auth_code_ttl_secs: None,
                    device_code_ttl_secs: None,
                    login_policy: None,
                },
                vec![key],
            )
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(realm_view(&realm)))
    }

    pub(crate) async fn admin_list_users_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<UserView>>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let users = ctx
            .store
            .list_users_in_realm(realm.id)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(users.iter().map(user_view).collect()))
    }

    pub(crate) async fn admin_create_user_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateUserRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let req = body.into_inner();

        let password_hash = hash_password(&req.password)?;
        let user = ctx
            .store
            .create_user(NewUser {
                realm_id: realm.id,
                username: req.username,
                email: req.email,
                display_name: req.display_name,
                password_hash,
                // Privilege flags are never settable through this surface:
                // a tenant admin must not be able to mint a fleet root.
                is_root: false,
                fleet_admin: false,
                brokered: None,
            })
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(user_view(&user)))
    }

    pub(crate) async fn admin_get_user_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminUserPath>,
    ) -> Result<HttpResponseOk<UserView>, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let user = load_realm_user(ctx, &realm, p.user_id).await?;
        Ok(HttpResponseOk(user_view(&user)))
    }

    pub(crate) async fn admin_update_user_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminUserPath>,
        body: TypedBody<UpdateUserRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let user = load_realm_user(ctx, &realm, p.user_id).await?;
        let req = body.into_inner();

        // Status is the only persisted mutable field with a dedicated store
        // method. email/display_name have no store setter today, so we
        // surface that limitation rather than silently dropping the input.
        if req.email.is_some() || req.display_name.is_some() {
            return Err(HttpError::for_bad_request(
                None,
                "updating email/display_name is not yet supported".to_string(),
            ));
        }

        let updated = match req.status {
            Some(UserStatusView::Active) => ctx
                .store
                .set_user_status(user.id, UserStatus::Active)
                .await
                .map_err(store_err_to_http)?,
            Some(UserStatusView::Disabled) => ctx
                .store
                .set_user_status(user.id, UserStatus::Disabled)
                .await
                .map_err(store_err_to_http)?,
            None => user,
        };
        Ok(HttpResponseOk(user_view(&updated)))
    }

    pub(crate) async fn admin_set_user_password_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminUserPath>,
        body: TypedBody<SetPasswordRequest>,
    ) -> Result<HttpResponseOk<UserView>, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let user = load_realm_user(ctx, &realm, p.user_id).await?;

        let hash = hash_password(&body.into_inner().password)?;
        let updated = ctx
            .store
            .update_user_password_hash(user.id, hash)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(user_view(&updated)))
    }

    pub(crate) async fn admin_delete_user_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminUserPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let user = load_realm_user(ctx, &realm, p.user_id).await?;
        ctx.store
            .delete_user(user.id)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseDeleted())
    }

    pub(crate) async fn admin_list_role_assignments_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<RoleAssignmentView>>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        // The store indexes assignments by subject/target, not by realm, so
        // gather both seeded targets for this realm's scope and filter by
        // realm_id for an authoritative per-realm list.
        let assignments = list_realm_assignments(ctx, &realm).await?;
        Ok(HttpResponseOk(
            assignments.iter().map(assignment_view).collect(),
        ))
    }

    pub(crate) async fn admin_create_role_assignment_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateRoleAssignmentRequest>,
    ) -> Result<HttpResponseOk<RoleAssignmentView>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let req = body.into_inner();

        let subject = subject_from_view(&req.subject)?;
        let target = target_from_view(&req.target)?;
        let role = role_from_view(req.role)?;
        // `created_by` is recorded for audit. We do not have the acting
        // user's store id handy without a second lookup; the store accepts
        // the nil uuid as "system/admin surface", which is acceptable for
        // v1 and avoids leaking the pinned-vs-store id mismatch.
        let assignment = ctx
            .store
            .create_role_assignment(NewRoleAssignment {
                realm_id: realm.id,
                subject,
                target,
                role,
                created_by: Uuid::nil(),
            })
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(assignment_view(&assignment)))
    }

    pub(crate) async fn admin_delete_role_assignment_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminAssignmentPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        // Confirm the assignment belongs to the authorized realm before
        // deleting, so a tenant admin can't delete a sibling realm's grant
        // by id.
        let assignment = ctx
            .store
            .get_role_assignment(p.id)
            .await
            .map_err(store_err_to_http)?;
        if assignment.realm_id != realm.id {
            return Err(forbidden());
        }
        ctx.store
            .delete_role_assignment(p.id)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseDeleted())
    }

    pub(crate) async fn admin_get_identity_source_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<IdentitySourceView>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let connections = ctx
            .store
            .list_connections_in_realm(realm.id)
            .await
            .map_err(store_err_to_http)?;

        // The active upstream is the realm's single enabled connection (the
        // store enforces at-most-one-enabled per realm via
        // `set_connection_enabled`). Pick by lowest id so the choice is
        // deterministic even if a legacy realm somehow has more than one
        // enabled row. None enabled (or an unmodeled kind) => integrated.
        let active = connections
            .iter()
            .filter(|c| c.enabled)
            .min_by_key(|c| c.id);
        let view = match active.and_then(identity_source_of) {
            Some((mode, summary)) => IdentitySourceView {
                mode,
                connection: Some(summary),
            },
            None => IdentitySourceView {
                mode: IdentitySourceMode::Integrated,
                connection: None,
            },
        };
        Ok(HttpResponseOk(view))
    }

    pub(crate) async fn admin_list_connections_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
    ) -> Result<HttpResponseOk<Vec<ConnectionView>>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let connections = ctx
            .store
            .list_connections_in_realm(realm.id)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(
            connections.iter().map(connection_view).collect(),
        ))
    }

    pub(crate) async fn admin_create_connection_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminRealmPath>,
        body: TypedBody<CreateConnectionRequest>,
    ) -> Result<HttpResponseOk<ConnectionView>, HttpError> {
        let ctx = rqctx.context();
        let realm = authorize_realm(ctx, rqctx.request.headers(), path.into_inner().realm).await?;
        let req = body.into_inner();
        let connection = ctx
            .store
            .create_upstream_connection(NewUpstreamConnection {
                realm_id: realm.id,
                name: req.name,
                kind: connection_kind_from_input(req.kind),
                enabled: req.enabled,
            })
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseOk(connection_view(&connection)))
    }

    pub(crate) async fn admin_patch_connection_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminConnectionPath>,
        body: TypedBody<PatchConnectionRequest>,
    ) -> Result<HttpResponseOk<ConnectionView>, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let connection = load_realm_connection(ctx, &realm, p.id).await?;
        let req = body.into_inner();

        // The store exposes only `set_connection_enabled` for in-place edits;
        // name/kind changes have no setter, so reject them rather than drop
        // them silently. Enabling/disabling is the load-bearing toggle.
        if req.name.is_some() || req.kind.is_some() {
            return Err(HttpError::for_bad_request(
                None,
                "updating connection name/kind is not yet supported".to_string(),
            ));
        }

        let updated = match req.enabled {
            Some(enabled) => ctx
                .store
                .set_connection_enabled(connection.id, enabled)
                .await
                .map_err(store_err_to_http)?,
            None => connection,
        };
        Ok(HttpResponseOk(connection_view(&updated)))
    }

    pub(crate) async fn admin_delete_connection_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminConnectionPath>,
    ) -> Result<HttpResponseDeleted, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let connection = load_realm_connection(ctx, &realm, p.id).await?;
        ctx.store
            .delete_upstream_connection(connection.id)
            .await
            .map_err(store_err_to_http)?;
        Ok(HttpResponseDeleted())
    }

    pub(crate) async fn admin_put_claim_mappings_impl(
        rqctx: RequestContext<std::sync::Arc<Ctx>>,
        path: Path<AdminConnectionPath>,
        body: TypedBody<PutClaimMappingsRequest>,
    ) -> Result<HttpResponseOk<Vec<ClaimMappingView>>, HttpError> {
        let ctx = rqctx.context();
        let p = path.into_inner();
        let realm = authorize_realm(ctx, rqctx.request.headers(), p.realm).await?;
        let connection = load_realm_connection(ctx, &realm, p.id).await?;
        let req = body.into_inner();

        let mappings: Vec<ClaimMapping> = req
            .mappings
            .iter()
            .enumerate()
            .map(|(i, m)| {
                Ok(ClaimMapping {
                    connection_id: connection.id,
                    seq: u32::try_from(i).unwrap_or(u32::MAX),
                    source: m.source.clone(),
                    target: mapped_field_from_view(m.target)?,
                    group_value: m.group_value.clone(),
                })
            })
            .collect::<Result<Vec<_>, HttpError>>()?;
        ctx.store
            .put_claim_mappings(connection.id, mappings)
            .await
            .map_err(store_err_to_http)?;

        let stored = ctx
            .store
            .list_claim_mappings(connection.id)
            .await
            .map_err(store_err_to_http)?;
        let views = stored
            .iter()
            .map(|m| ClaimMappingView {
                source: m.source.clone(),
                target: mapped_field_view(m.target),
                group_value: m.group_value.clone(),
            })
            .collect();
        Ok(HttpResponseOk(views))
    }
}

// ===========================================================================
// Small shared helpers.
// ===========================================================================

/// Hash a plaintext password (bcrypt, default cost).
fn hash_password(password: &str) -> Result<String, HttpError> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| HttpError::for_internal_error(format!("hash password: {e}")))
}

/// Parse `?scope=tenant:{uuid}` (or `silo:{uuid}`, or `system`) into a
/// [`RealmScope`] filter. An unparseable value is a 400.
fn parse_scope_filter(raw: Option<&str>) -> Result<Option<RealmScope>, HttpError> {
    let Some(raw) = raw else { return Ok(None) };
    let bad = || HttpError::for_bad_request(None, format!("invalid scope filter {raw:?}"));
    if raw == "system" {
        return Ok(Some(RealmScope::System));
    }
    let (kind, id) = raw.split_once(':').ok_or_else(bad)?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    match kind {
        "tenant" => Ok(Some(RealmScope::Tenant { tenant_id: id })),
        "silo" => Ok(Some(RealmScope::Silo { silo_id: id })),
        _ => Err(bad()),
    }
}

/// Load a user and confirm it belongs to `realm` (else 404). Prevents a
/// tenant admin from poking at another realm's user by id.
async fn load_realm_user(ctx: &Ctx, realm: &Realm, user_id: Uuid) -> Result<User, HttpError> {
    let user = ctx.store.get_user(user_id).await.map_err(store_err_to_http)?;
    if user.realm_id != realm.id {
        return Err(HttpError::for_not_found(None, "user not found".to_string()));
    }
    Ok(user)
}

/// Load a connection and confirm it belongs to `realm` (else 404).
async fn load_realm_connection(
    ctx: &Ctx,
    realm: &Realm,
    connection_id: Uuid,
) -> Result<UpstreamConnection, HttpError> {
    let connection = ctx
        .store
        .get_upstream_connection(connection_id)
        .await
        .map_err(store_err_to_http)?;
    if connection.realm_id != realm.id {
        return Err(HttpError::for_not_found(
            None,
            "connection not found".to_string(),
        ));
    }
    Ok(connection)
}

/// All role assignments belonging to `realm`. The store indexes by
/// subject and target, so we union the assignments for this realm's
/// natural targets and filter by `realm_id`.
async fn list_realm_assignments(
    ctx: &Ctx,
    realm: &Realm,
) -> Result<Vec<identity_store::RoleAssignment>, HttpError> {
    let targets: Vec<AssignmentTarget> = match realm.scope {
        RealmScope::Tenant { tenant_id } => vec![AssignmentTarget::Tenant { tenant_id }],
        RealmScope::Silo { silo_id } => vec![
            AssignmentTarget::Silo { silo_id },
            // a silo realm may also grant tenant-scoped roles
        ],
        RealmScope::System => vec![AssignmentTarget::Fleet],
        _ => vec![],
    };
    let mut out = Vec::new();
    for target in &targets {
        let assignments = ctx
            .store
            .list_assignments_for_target(target)
            .await
            .map_err(store_err_to_http)?;
        out.extend(assignments.into_iter().filter(|a| a.realm_id == realm.id));
    }
    Ok(out)
}

/// Derive the identity-source mode + non-secret summary for an enabled
/// upstream connection. Returns `None` for a connection kind this version
/// does not model (the caller then falls back to `integrated`).
fn identity_source_of(
    c: &UpstreamConnection,
) -> Option<(IdentitySourceMode, IdentitySourceConnection)> {
    match &c.kind {
        ConnectionKind::Oidc {
            issuer_url,
            client_id,
            ..
        } => Some((
            IdentitySourceMode::Oidc,
            IdentitySourceConnection {
                id: c.id,
                name: c.name.clone(),
                enabled: c.enabled,
                issuer_url: Some(issuer_url.clone()),
                client_id: Some(client_id.clone()),
                idp_metadata: None,
                sp_entity_id: None,
            },
        )),
        ConnectionKind::Saml {
            idp_metadata,
            sp_entity_id,
            ..
        } => Some((
            IdentitySourceMode::Saml,
            IdentitySourceConnection {
                id: c.id,
                name: c.name.clone(),
                enabled: c.enabled,
                issuer_url: None,
                client_id: None,
                idp_metadata: Some(idp_metadata.clone()),
                sp_entity_id: Some(sp_entity_id.clone()),
            },
        )),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use identity_store::{IdentityStore, KeyStatus, MemStore, SigningAlg};
    use jsonwebtoken::{Algorithm, Header, encode};

    use crate::server::Ctx;

    /// A seeded MemStore-backed context plus the pinned dev signing
    /// material, matching the live boot path.
    async fn seeded_ctx() -> Ctx {
        let signing = crate::keys::load().expect("load dev signing key");
        let store: Arc<dyn IdentityStore> = Arc::new(MemStore::new());
        crate::bootstrap::seed(store.as_ref(), signing.public_jwk.clone())
            .await
            .expect("seed store");
        Ctx { store, signing }
    }

    /// Mint a tenant-admin access token whose `iss` names `issuer`, whose
    /// `realm` claim is the PINNED `realm_claim`, and whose `sub` is the
    /// store user id. Signed with the same dev key the server verifies
    /// against, so it passes `verify_token_with_realms` for the realm that
    /// owns `issuer`.
    fn mint_tenant_admin_token(
        ctx: &Ctx,
        issuer: &str,
        realm_claim: Uuid,
        tenant_id: Uuid,
        sub: Uuid,
    ) -> String {
        let now = Utc::now();
        let claims = AccessClaims {
            sub,
            iss: issuer.to_string(),
            aud: None,
            exp: (now + Duration::seconds(3600)).timestamp(),
            iat: now.timestamp(),
            nbf: None,
            realm: realm_claim,
            realm_scope: TokenRealmScope::Tenant,
            tenant_id: Some(tenant_id),
            silo_id: None,
            is_root: false,
            fleet_admin: false,
            groups: vec![],
            scope: Some("openid".to_string()),
            cnf: None,
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(identifiers::SIGNING_KID.to_string());
        encode(&header, &claims, &ctx.signing.encoding_key).expect("sign token")
    }

    fn bearer_headers(token: &str) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("header value"),
        );
        headers
    }

    /// The store-assigned id of the seeded tenant realm (its id differs
    /// from the pinned wire id).
    async fn seeded_tenant_realm(ctx: &Ctx) -> Realm {
        ctx.store
            .get_realm_by_issuer(&identifiers::tenant_issuer_url())
            .await
            .expect("seeded tenant realm")
    }

    /// The seeded TenantAdmin demo user in the tenant realm.
    async fn seeded_tenant_admin(ctx: &Ctx, realm_id: Uuid) -> User {
        ctx.store
            .get_user_by_username(realm_id, identifiers::DEMO_USERNAME)
            .await
            .expect("seeded demo user")
    }

    /// Stand up a second tenant ("tenant B") with its own realm, issuer,
    /// TenantAdmin user, and grant. Returns `(realm, admin_user,
    /// tenant_id)`.
    async fn seed_other_tenant(ctx: &Ctx) -> (Realm, User, Uuid) {
        let tenant_b = Uuid::from_u128(0x55555555_5555_4555_8555_555555555555);
        let issuer = format!("{}/realms/tenant-b", identifiers::ISSUER_BASE);
        let now = Utc::now();
        let realm = ctx
            .store
            .create_realm(
                NewRealm {
                    scope: RealmScope::Tenant {
                        tenant_id: tenant_b,
                    },
                    name: "tenant-b".to_string(),
                    description: None,
                    issuer_url: issuer.clone(),
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
                    public_jwk: ctx.signing.public_jwk.clone(),
                    status: KeyStatus::Active,
                    not_before: now,
                    not_after: now + Duration::days(1),
                }],
            )
            .await
            .expect("create tenant-b realm");

        let admin = ctx
            .store
            .create_user(NewUser {
                realm_id: realm.id,
                username: "tenant-b-admin".to_string(),
                email: None,
                display_name: None,
                password_hash: String::new(),
                is_root: false,
                fleet_admin: false,
                brokered: None,
            })
            .await
            .expect("create tenant-b admin");

        ctx.store
            .create_role_assignment(NewRoleAssignment {
                realm_id: realm.id,
                subject: AssignmentSubject::User { user_id: admin.id },
                target: AssignmentTarget::Tenant {
                    tenant_id: tenant_b,
                },
                role: Role::TenantAdmin,
                created_by: admin.id,
            })
            .await
            .expect("grant tenant-b admin");

        (realm, admin, tenant_b)
    }

    /// The pinned tenant realm id addresses the same realm on the admin
    /// surface as the store-assigned id: a tenant-admin token presenting
    /// the PINNED id resolves and authorizes against its own realm.
    #[tokio::test]
    async fn admin_accepts_pinned_tenant_realm_id() {
        let ctx = seeded_ctx().await;
        let realm = seeded_tenant_realm(&ctx).await;
        let admin = seeded_tenant_admin(&ctx, realm.id).await;
        let token = mint_tenant_admin_token(
            &ctx,
            &identifiers::tenant_issuer_url(),
            identifiers::TENANT_REALM_ID,
            identifiers::TENANT_ID,
            admin.id,
        );
        let headers = bearer_headers(&token);

        // The pinned id (what the BFF forwards) authorizes...
        let by_pinned = authorize_realm(&ctx, &headers, identifiers::TENANT_REALM_ID)
            .await
            .expect("pinned id must authorize on own realm");
        // ...and resolves to the same store realm as the store id.
        let by_store = authorize_realm(&ctx, &headers, realm.id)
            .await
            .expect("store id must authorize on own realm");
        assert_eq!(by_pinned.id, realm.id);
        assert_eq!(by_store.id, realm.id);
    }

    /// Listing users via the pinned id returns the tenant realm's users
    /// (regression: the BFF-forwarded pinned id previously 404'd).
    #[tokio::test]
    async fn admin_list_users_via_pinned_id_returns_realm_users() {
        let ctx = seeded_ctx().await;
        let realm = seeded_tenant_realm(&ctx).await;
        let admin = seeded_tenant_admin(&ctx, realm.id).await;
        let token = mint_tenant_admin_token(
            &ctx,
            &identifiers::tenant_issuer_url(),
            identifiers::TENANT_REALM_ID,
            identifiers::TENANT_ID,
            admin.id,
        );
        let headers = bearer_headers(&token);

        let authorized = authorize_realm(&ctx, &headers, identifiers::TENANT_REALM_ID)
            .await
            .expect("pinned id authorizes");
        let users = ctx
            .store
            .list_users_in_realm(authorized.id)
            .await
            .expect("list users");
        assert!(
            users.iter().any(|u| u.id == admin.id),
            "the demo admin must be listed in its own realm"
        );
    }

    /// Cross-tenant isolation holds under the pinned-id mapping: a
    /// tenant-B admin token is refused (403) against tenant-A's PINNED
    /// realm id, not silently mapped through.
    #[tokio::test]
    async fn admin_rejects_other_tenant_by_pinned_id() {
        let ctx = seeded_ctx().await;
        let (b_realm, b_admin, b_tenant) = seed_other_tenant(&ctx).await;
        let token = mint_tenant_admin_token(
            &ctx,
            &b_realm.issuer_url,
            // A real tenant-B token carries tenant-B's own realm claim; the
            // attack is targeting tenant-A's PINNED id in the path.
            b_realm.id,
            b_tenant,
            b_admin.id,
        );
        let headers = bearer_headers(&token);

        let err = authorize_realm(&ctx, &headers, identifiers::TENANT_REALM_ID)
            .await
            .expect_err("tenant-B admin must be forbidden on tenant-A's pinned realm id");
        assert_eq!(
            err.status_code.as_status(),
            http::StatusCode::FORBIDDEN,
            "cross-tenant access by pinned id is a 403"
        );
    }
}
