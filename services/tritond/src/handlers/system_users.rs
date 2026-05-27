// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Grant/revoke `Capability` on a User. Privilege change so requires
//! `SystemOperate`, not `SystemConfigWrite`. Also hosts the
//! `/v1/system/utilization/silos` placeholder: returns 501 (not `[]`,
//! which would falsely look like "zero silos have data").

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
};
use tritond_store::Capability;

use crate::context::ApiContext;
use crate::error::store_error_to_http;

/// `PUT /v1/system/users/{user_id}/capabilities/{capability}`.
/// Grant a capability to a user. Idempotent: granting an
/// already-present capability is a no-op (the persisted set is
/// re-written with the same content). Returns the updated user
/// view.
pub(crate) async fn grant_user_capability_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemUserCapabilityPath>,
) -> Result<HttpResponseOk<tritond_store::UserView>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::SystemUserCapabilityPath {
        user_id,
        capability,
    } = path.into_inner();

    // Authentication only - the capability gate is the operator
    // surface's only authorization check. Cross-scope-deny shape
    // (404 NotFound for missing capability) is preserved by
    // `require_capability`.
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, Capability::SystemOperate)?;

    let existing = ctx
        .store
        .get_user_by_id(user_id)
        .await
        .map_err(store_error_to_http)?;
    let mut caps = existing.capabilities.clone();
    let inserted = caps.insert(capability);
    if !inserted {
        // Already present. Return the existing view; no store
        // write needed.
        return Ok(HttpResponseOk(existing.into()));
    }
    let updated = ctx
        .store
        .update_user_capabilities(user_id, caps)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(updated.into()))
}

/// `DELETE /v1/system/users/{user_id}/capabilities/{capability}`.
/// Revoke a capability from a user. Idempotent: revoking an
/// absent capability is a no-op. Returns 204 No Content (success
/// without a body) so the operator's `tcadm system user revoke`
/// flow doesn't have to parse a payload on a removal.
///
/// Refuses to revoke from `is_root` users with `400 BadRequest`
/// `RootIsRoot` - root is the bootstrap operator and is defined
/// as carrying every capability implicitly; revoking from root
/// would create a partial-root state that is meaningless.
pub(crate) async fn revoke_user_capability_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemUserCapabilityPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::SystemUserCapabilityPath {
        user_id,
        capability,
    } = path.into_inner();

    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, Capability::SystemOperate)?;

    let existing = ctx
        .store
        .get_user_by_id(user_id)
        .await
        .map_err(store_error_to_http)?;
    if existing.is_root {
        return Err(HttpError::for_client_error(
            Some("RootIsRoot".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "root operators implicitly carry every capability; revoking is meaningless".to_string(),
        ));
    }
    let mut caps = existing.capabilities.clone();
    let removed = caps.remove(&capability);
    if !removed {
        // Already absent.
        return Ok(HttpResponseDeleted());
    }
    ctx.store
        .update_user_capabilities(user_id, caps)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseDeleted())
}

/// Returns 501 (not `[]`) until quota accounting lands so dashboards
/// surface "not implemented" instead of "zero silos". Non-SystemRead
/// callers still get 404 first.
pub(crate) async fn get_system_utilization_silos_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<tritond_store::Silo>>, HttpError> {
    let ctx = rqctx.context();
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, Capability::SystemRead)?;
    // Dropshot's HttpError helpers don't expose 501, so we ride 503
    // (`for_unavail`); clients dispatch on `UtilizationUnavailable`
    // in the body rather than the numeric status.
    Err(HttpError::for_unavail(
        Some("UtilizationUnavailable".to_string()),
        "per-silo utilization accounting is not yet implemented".to_string(),
    ))
}
