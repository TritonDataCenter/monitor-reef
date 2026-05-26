// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::routes` HTTP handlers (delegated to from the `TritondApi` impl).

#![allow(unused_imports)]

use crate::blueprint::*;
use crate::bundle::*;
use crate::cn_credential::*;
use crate::error::*;
use crate::lifecycle::*;
use crate::principal::*;
use crate::validate::*;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use dropshot::{
    ApiDescription, ClientErrorStatusCode, ConfigDropshot, ConfigLogging, ConfigLoggingLevel,
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, HttpServer, HttpServerStarter, Path, Query, RequestContext,
    TypedBody,
};
use tritond_api::{
    AgentJobPath, AgentPortBlueprint, AgentPortBlueprintPath, AgentStatusRequest, ApiKeyCreated,
    ApiKeyPath, ApproveCnRequest, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, ClaimJobRequest, ClaimJobResponse,
    CnListQuery, CnPath, CompleteJobRequest, ConfigEntry, ConfigKeyPath, HealthResponse, ImagePath,
    InstanceDeleteQuery, InstanceLogsPath, LegacyCnSummary, LegacyVmListQuery, LegacyVmPath,
    LogTailQuery, LoginRequest, MetricsRangeQuery, NetworkRealizationRequest, NewApiKey,
    NewIdpConfig, NewImageFromBundle, OpenAutoApproveRequest, ProvisioningBlueprint,
    RefreshRequest, RegisterCnRequest, RegisterCnResponse, RegisterStatusQuery,
    RegisterStatusResponse, SetCnRoleRequest, SetConfigRequest, SiloPath, SiloTenantPath,
    SshKeyPath, StorageClusterAccessKeyPath, StorageClusterBucketPath, StorageClusterNodePath,
    StorageClusterPath, StorageClusterUserPath, StorageClusterUserPolicyPath, TenantIdpPath,
    TenantPath, TenantProjectFloatingIpPath, TenantProjectInstanceDiskPath,
    TenantProjectInstanceNicPath, TenantProjectInstancePath, TenantProjectPath,
    TenantProjectVpcDhcpMacPath, TenantProjectVpcFirewallRulePath, TenantProjectVpcNatGatewayPath,
    TenantProjectVpcPath, TenantProjectVpcRouteTablePath, TenantProjectVpcRouteTableRoutePath,
    TenantProjectVpcSubnetPath, TokenResponse, TritondApi,
    types::{
        ApiKeyView, AuditEvent, AutoApproveWindow, CnView, DhcpLease, DhcpPool, DhcpReservation,
        Disk, FirewallRule, FloatingIp, IdpConfigView, Image, ImageScope, Instance, JobKind,
        JobOutcome, LegacyVm, LifecycleState, LifecycleStateKind, NatGateway, NewDhcpPool,
        NewDhcpReservation, NewFirewallRule, NewFloatingIp, NewImage, NewInstance, NewJob,
        NewNatGateway, NewProject, NewQuota, NewRoute, NewRouteTable, NewSilo, NewSshKey,
        NewStorageCluster, NewSubnet, NewTenant, NewVpc, Nic, PresignGetRequest, PresignPutRequest,
        PresignResponse, Project, ProvisioningJob, Quota, Route, RouteTable, RouteTarget,
        SetPresignerRequest, Silo, SshKey, SshKeyScope, StorageAccessKey, StorageBucket,
        StorageClusterSummary, StorageClusterView, StorageMembership, StorageNode,
        StorageObjectsPage, StorageUser, Subnet, Tenant, Vpc,
    },
};
use tritond_audit::{Actor as AuditActor, Outcome as AuditOutcome};
use tritond_auth::OidcConfig;
use tritond_auth::{
    TokenKind, generate_api_key, mint_access, mint_refresh, verify, verify_password,
};
use tritond_store::{
    AUTO_APPROVE_WINDOW_MAX, ApiKey, CnState, ConfigError, ConfigKey, IdpConfig, Store, StoreError,
    normalize_claim_code,
};
use uuid::Uuid;

use crate::auth::{
    Action, AuthService, Principal, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant, require_authenticated,
};

use crate::VERSION;

/// Concrete implementor of [`TritondApi`].
use crate::context::ApiContext;

/// RFD 00007 AP-2j: `GET /v1/route-tables?vpc=<uuid>`. Flat list.
pub(crate) async fn list_route_tables_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::RouteTableQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<RouteTable>>, HttpError> {
    use tritond_api::v1::{ResultsPage, RouteTableQuery};
    let ctx = rqctx.context();
    let RouteTableQuery { scope, vpc } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    let vpc_id = vpc.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/route-tables requires `?vpc=<uuid>`".to_string(),
        )
    })?;
    let vpc_row = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    if let Some(t) = scope.tenant
        && vpc_row.tenant_id != t
    {
        return Err(not_found());
    }
    if let Some(p) = scope.project
        && vpc_row.project_id != p
    {
        return Err(not_found());
    }
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableList,
        vpc_row.tenant_id,
    )
    .await?;
    let tables = ctx
        .store
        .list_route_tables_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(tables)))
}

/// RFD 00007 AP-2j: `GET /v1/route-tables/{route_table_id}`.
pub(crate) async fn get_route_table_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::RouteTablePath>,
) -> Result<HttpResponseOk<RouteTable>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::RouteTablePath { route_table_id } = path.into_inner();
    let rt = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableGet,
        rt.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(rt))
}

/// RFD 00007 AP-2j: `GET /v1/routes?route_table=<uuid>`. Flat list.
pub(crate) async fn list_routes_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::RouteQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Route>>, HttpError> {
    use tritond_api::v1::{ResultsPage, RouteQuery};
    let ctx = rqctx.context();
    let RouteQuery { scope, route_table } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    let route_table_id = route_table.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/routes requires `?route_table=<uuid>`".to_string(),
        )
    })?;
    let rt = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    if let Some(t) = scope.tenant
        && rt.tenant_id != t
    {
        return Err(not_found());
    }
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteList,
        rt.tenant_id,
    )
    .await?;
    let routes = ctx
        .store
        .list_routes_in_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(routes)))
}

/// RFD 00007 AP-2j: `GET /v1/routes/{route_id}`.
pub(crate) async fn get_route_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::RoutePath>,
) -> Result<HttpResponseOk<Route>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::RoutePath { route_id } = path.into_inner();
    let route = ctx
        .store
        .get_route(route_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteGet,
        route.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(route))
}

/// RFD 00007 AP-3a-13: `POST /v1/route-tables?vpc=<uuid>`.
pub(crate) async fn create_route_table_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::RouteTableQuery>,
    body: TypedBody<NewRouteTable>,
) -> Result<HttpResponseCreated<RouteTable>, HttpError> {
    let q = query.into_inner();
    let vpc_id = q.vpc.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/route-tables requires `?vpc=<uuid>`".to_string(),
        )
    })?;

    let ctx = rqctx.context();
    let vpc = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = vpc.tenant_id;
    let project_id = vpc.project_id;

    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    match ctx
        .store
        .create_route_table(tenant_id, project_id, vpc_id, req)
        .await
    {
        Ok(rt) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteTableCreate,
                    request_id,
                    Some(format!("RouteTable::\"{}\"", rt.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("RouteTable::\"{}\"", rt.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "name": rt.name,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(rt))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteTableCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

/// RFD 00007 AP-3a-13: `DELETE /v1/route-tables/{route_table_id}`.
pub(crate) async fn delete_route_table_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::RouteTablePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::RouteTablePath { route_table_id } = path.into_inner();
    let rt = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = rt.tenant_id;
    let project_id = rt.project_id;
    let vpc_id = rt.vpc_id;

    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    match ctx.store.delete_route_table(route_table_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteTableDelete,
                    request_id,
                    Some(format!("RouteTable::\"{route_table_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("RouteTable::\"{route_table_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteTableDelete,
                    request_id,
                    Some(format!("RouteTable::\"{route_table_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

/// RFD 00007 AP-3a-13: `POST /v1/routes?route_table=<uuid>`.
pub(crate) async fn create_route_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::RouteQuery>,
    body: TypedBody<NewRoute>,
) -> Result<HttpResponseCreated<Route>, HttpError> {
    let q = query.into_inner();
    let route_table_id = q.route_table.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/routes requires `?route_table=<uuid>`".to_string(),
        )
    })?;

    let ctx = rqctx.context();
    let rt = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = rt.tenant_id;
    let project_id = rt.project_id;
    let vpc_id = rt.vpc_id;

    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    if matches!(req.target, RouteTarget::FloatingIp { .. }) {
        return Err(bad_request(
            "floating ip route targets are system-installed only in v1".to_string(),
        ));
    }
    if let RouteTarget::NatGateway { nat_gateway_id } = &req.target {
        let nat = ctx
            .store
            .get_nat_gateway(*nat_gateway_id)
            .await
            .map_err(store_error_to_http)?;
        if nat.tenant_id != tenant_id || nat.project_id != project_id {
            return Err(not_found());
        }
        if nat.vpc_id != vpc_id {
            return Err(bad_request(format!(
                "nat gateway {nat_gateway_id} is not in vpc {vpc_id}"
            )));
        }
    }

    match ctx
        .store
        .create_route(tenant_id, project_id, vpc_id, route_table_id, req)
        .await
    {
        Ok(route) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteCreate,
                    request_id,
                    Some(format!("Route::\"{}\"", route.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Route::\"{}\"", route.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "route_table_id": route_table_id,
                        "destination": route.destination.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseCreated(route))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

/// RFD 00007 AP-3a-13: `DELETE /v1/routes/{route_id}`.
pub(crate) async fn delete_route_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::RoutePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::RoutePath { route_id } = path.into_inner();
    let route = ctx
        .store
        .get_route(route_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = route.tenant_id;
    let project_id = route.project_id;
    let vpc_id = route.vpc_id;
    let route_table_id = route.route_table_id;

    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    match ctx.store.delete_route(route_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteDelete,
                    request_id,
                    Some(format!("Route::\"{route_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Route::\"{route_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "route_table_id": route_table_id,
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::RouteDelete,
                    request_id,
                    Some(format!("Route::\"{route_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_vpc_route_tables(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<RouteTable>>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcPath {
        tenant_id,
        project_id,
        vpc_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableList,
        tenant_id,
    )
    .await?;

    let vpc = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
        return Err(not_found());
    }
    let route_tables = ctx
        .store
        .list_route_tables_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(route_tables))
}

/// RFD 00007 AP-3e: moved to `POST /v1/route-tables?vpc=<uuid>`.
pub(crate) async fn create_vpc_route_table(
    _rqctx: RequestContext<ApiContext>,
    _path: Path<TenantProjectVpcPath>,
    _body: TypedBody<NewRouteTable>,
) -> Result<HttpResponseCreated<RouteTable>, HttpError> {
    Err(crate::error::gone("POST /v1/route-tables?vpc=<uuid>"))
}

pub(crate) async fn get_vpc_route_table(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcRouteTablePath>,
) -> Result<HttpResponseOk<RouteTable>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcRouteTablePath {
        tenant_id,
        project_id,
        vpc_id,
        route_table_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteTableGet,
        tenant_id,
    )
    .await?;
    let route_table = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    if route_table.tenant_id != tenant_id
        || route_table.project_id != project_id
        || route_table.vpc_id != vpc_id
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(route_table))
}

/// RFD 00007 AP-3e: moved to `DELETE /v1/route-tables/{route_table_id}`.
pub(crate) async fn delete_vpc_route_table(
    _rqctx: RequestContext<ApiContext>,
    _path: Path<TenantProjectVpcRouteTablePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    Err(crate::error::gone(
        "DELETE /v1/route-tables/{route_table_id}",
    ))
}

pub(crate) async fn list_vpc_route_table_routes(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcRouteTablePath>,
) -> Result<HttpResponseOk<Vec<Route>>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcRouteTablePath {
        tenant_id,
        project_id,
        vpc_id,
        route_table_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteList,
        tenant_id,
    )
    .await?;

    let route_table = ctx
        .store
        .get_route_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    if route_table.tenant_id != tenant_id
        || route_table.project_id != project_id
        || route_table.vpc_id != vpc_id
    {
        return Err(not_found());
    }
    let routes = ctx
        .store
        .list_routes_in_table(route_table_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(routes))
}

/// RFD 00007 AP-3e: moved to `POST /v1/routes?route_table=<uuid>`.
pub(crate) async fn create_vpc_route_table_route(
    _rqctx: RequestContext<ApiContext>,
    _path: Path<TenantProjectVpcRouteTablePath>,
    _body: TypedBody<NewRoute>,
) -> Result<HttpResponseCreated<Route>, HttpError> {
    Err(crate::error::gone("POST /v1/routes?route_table=<uuid>"))
}

pub(crate) async fn get_vpc_route_table_route(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcRouteTableRoutePath>,
) -> Result<HttpResponseOk<Route>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcRouteTableRoutePath {
        tenant_id,
        project_id,
        vpc_id,
        route_table_id,
        route_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::RouteGet,
        tenant_id,
    )
    .await?;
    let route = ctx
        .store
        .get_route(route_id)
        .await
        .map_err(store_error_to_http)?;
    if route.tenant_id != tenant_id
        || route.project_id != project_id
        || route.vpc_id != vpc_id
        || route.route_table_id != route_table_id
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(route))
}

/// RFD 00007 AP-3e: moved to `DELETE /v1/routes/{route_id}`.
pub(crate) async fn delete_vpc_route_table_route(
    _rqctx: RequestContext<ApiContext>,
    _path: Path<TenantProjectVpcRouteTableRoutePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    Err(crate::error::gone("DELETE /v1/routes/{route_id}"))
}
