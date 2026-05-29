// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Operator-cardinal networking handlers: the nic_tag registry,
//! network pools, external subnets, and the aggregate per-CN nic_tag
//! inventory. These are fleet-scoped `/v1/system/*` surfaces (RFD
//! 00008 doc-04), gated by `Capability` like the other `/v1/system/`
//! handlers (`cns`, `system_users`) rather than the tenant Cedar path
//! — there is no tenant/project scope to authorize against.
//!
//! Reads require `SystemRead`; mutations require `SystemConfigWrite`
//! (changing cluster-wide network topology, same gate as config
//! writes). `require_capability` returns 404-on-miss so a probe can't
//! distinguish "no capability" from "no resource".

use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, RequestContext,
    TypedBody,
};
use tritond_api::v1::ResultsPage;
use tritond_store::{
    Capability, CnNicTagInventory, NetworkPool, NewExternalSubnet, NewNetworkPool, NewNicTag,
    NicTag, Subnet,
};

use crate::auth::{authenticate_only, require_capability};
use crate::context::ApiContext;
use crate::error::store_error_to_http;

/// `GET /v1/system/nic-tags`. Fleet nic_tag registry. `SystemRead`.
pub(crate) async fn list_system_nic_tags_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<ResultsPage<NicTag>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let tags = ctx
        .store
        .list_nic_tags()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(tags)))
}

/// `GET /v1/system/nic-tags/{nic_tag_id}`. `SystemRead`.
pub(crate) async fn get_system_nic_tag_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemNicTagPath>,
) -> Result<HttpResponseOk<NicTag>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let tag = ctx
        .store
        .get_nic_tag(path.into_inner().nic_tag_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(tag))
}

/// `POST /v1/system/nic-tags`. Register a nic_tag. `SystemConfigWrite`.
pub(crate) async fn create_system_nic_tag_v1(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewNicTag>,
) -> Result<HttpResponseCreated<NicTag>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemConfigWrite)?;
    let tag = ctx
        .store
        .create_nic_tag(body.into_inner())
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseCreated(tag))
}

/// `DELETE /v1/system/nic-tags/{nic_tag_id}`. 409 `NicTagInUse` while
/// an external subnet still references the tag. `SystemConfigWrite`.
pub(crate) async fn delete_system_nic_tag_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemNicTagPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemConfigWrite)?;
    ctx.store
        .delete_nic_tag(path.into_inner().nic_tag_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseDeleted())
}

/// `GET /v1/system/network-pools`. `SystemRead`.
pub(crate) async fn list_system_network_pools_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<ResultsPage<NetworkPool>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let pools = ctx
        .store
        .list_network_pools()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(pools)))
}

/// `GET /v1/system/network-pools/{pool_id}`. `SystemRead`.
pub(crate) async fn get_system_network_pool_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemNetworkPoolPath>,
) -> Result<HttpResponseOk<NetworkPool>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let pool = ctx
        .store
        .get_network_pool(path.into_inner().pool_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(pool))
}

/// `POST /v1/system/network-pools`. `SystemConfigWrite`.
pub(crate) async fn create_system_network_pool_v1(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewNetworkPool>,
) -> Result<HttpResponseCreated<NetworkPool>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemConfigWrite)?;
    let pool = ctx
        .store
        .create_network_pool(body.into_inner())
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseCreated(pool))
}

/// `DELETE /v1/system/network-pools/{pool_id}`. `SystemConfigWrite`.
pub(crate) async fn delete_system_network_pool_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemNetworkPoolPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemConfigWrite)?;
    ctx.store
        .delete_network_pool(path.into_inner().pool_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseDeleted())
}

/// `POST /v1/system/external-subnets`. Create an operator-scoped
/// External subnet (FlatL2 public space). The store rejects an
/// overlapping CIDR (409 `SubnetCidrOverlap`). `SystemConfigWrite`.
pub(crate) async fn create_system_external_subnet_v1(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NewExternalSubnet>,
) -> Result<HttpResponseCreated<Subnet>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemConfigWrite)?;
    let subnet = ctx
        .store
        .create_external_subnet(body.into_inner())
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseCreated(subnet))
}

/// `GET /v1/system/external-subnets`. `SystemRead`.
pub(crate) async fn list_system_external_subnets_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<ResultsPage<Subnet>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let subnets = ctx
        .store
        .list_external_subnets()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(subnets)))
}

/// `GET /v1/system/cn-nic-tags`. Aggregate per-CN nic_tag inventory.
/// Token-scoped to `SystemRead` so the U-6 view consumes a real
/// contract instead of fanning out per-CN reads under admin-backend
/// privilege.
pub(crate) async fn list_system_cn_nic_tags_v1(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<ResultsPage<CnNicTagInventory>>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    require_capability(&principal, Capability::SystemRead)?;
    let inventory = ctx
        .store
        .list_cn_nic_tags()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(inventory)))
}
