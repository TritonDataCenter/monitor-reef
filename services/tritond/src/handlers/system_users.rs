// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RFD 00007 `/v1/system/users/...` capability administration.
//!
//! The operator-facing surface for granting and revoking
//! `Capability`-typed permissions on a User row. These verbs are
//! the operator's tool for upgrading a `fleet_admin == true` user
//! to also carry `SystemConfigWrite` or `StorageAdmin` after the
//! AP-1c migration runs.
//!
//! Capability-gated: requires `SystemOperate` per RFD 00007
//! §3.1's enumeration. Capability changes are themselves an
//! `Operate`-class action - granting / revoking a capability is a
//! privilege-change, not a config-write, so it rides
//! `SystemOperate` rather than `SystemConfigWrite`.
//!
//! This file also hosts the placeholder for
//! `/v1/system/utilization/silos` per RFD 00007 D-Ap-13 + the
//! "resolved questions" entry on locked behaviour: an explicit
//! `501 UtilizationUnavailable` until quota accounting catches
//! up. Returning `[]` would falsely indicate "zero silos have
//! utilization data"; the 501 makes "not implemented" explicit.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
};
use tritond_store::{Capability, Store, StoreError};

use crate::auth::{Action, authenticate_and_authorize};
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

    // Capability gate. `Action::InstanceList` is the closest
    // existing Cedar action for "authenticated operator" - the
    // SystemOperate-class actions don't yet have dedicated Cedar
    // variants (that wave lands when the Cedar bundle is split per
    // capability in a later slice). The substantive check is
    // `require_capability` below.
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::InstanceList)
            .await?;
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

    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::InstanceList)
            .await?;
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
            "root operators implicitly carry every capability; revoking is meaningless"
                .to_string(),
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
    let _ = StoreError::NotFound; // suppress unused-import warning
    Ok(HttpResponseDeleted())
}

/// `GET /v1/system/utilization/silos`. Per RFD 00007 D-Ap-13's
/// resolved-questions entry, the endpoint is locked in the spec
/// but returns `501 UtilizationUnavailable` until quota accounting
/// catches up. The 501 (not `[]`) is intentional: an empty array
/// would falsely indicate "zero silos have utilization data" and
/// clients depending on the endpoint would silently render empty
/// dashboards instead of surfacing "not implemented" to the
/// operator.
///
/// Capability: `SystemRead`. The capability check still runs so
/// non-SystemRead callers get the same 404 they'd get on any other
/// /v1/system/ endpoint; SystemRead callers get the 501 carrying
/// the explicit unavailability message.
pub(crate) async fn get_system_utilization_silos_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<tritond_store::Silo>>, HttpError> {
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::InstanceList)
            .await?;
    crate::auth::require_capability(&principal, Capability::SystemRead)?;
    Err(HttpError::for_client_error(
        Some("UtilizationUnavailable".to_string()),
        ClientErrorStatusCode::from(http::StatusCode::NOT_IMPLEMENTED),
        "per-silo utilization accounting is not yet implemented; \
         the path is reserved per RFD 00007 D-Ap-13"
            .to_string(),
    ))
}
