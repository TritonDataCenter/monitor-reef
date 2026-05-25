// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::dhcp` HTTP handlers (delegated to from the `TritondApi` impl).

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

/// RFD 00007 AP-2k: `GET /v1/vpc-dhcp-pools/{vpc_id}`. Flat single
/// per-VPC DHCP-pool read. Returns 404 if no pool is set rather
/// than 200 with `null`; the singleton-per-X PUT/GET/DELETE shape
/// from Locked Decision #20 is preserved.
pub(crate) async fn get_vpc_dhcp_pool_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::VpcDhcpPoolPath>,
) -> Result<HttpResponseOk<DhcpPool>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::VpcDhcpPoolPath { vpc_id } = path.into_inner();
    let vpc = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpPoolGet,
        vpc.tenant_id,
    )
    .await?;
    let pool = ctx
        .store
        .get_dhcp_pool(vpc_id)
        .await
        .map_err(store_error_to_http)?
        .ok_or_else(not_found)?;
    Ok(HttpResponseOk(pool))
}

/// RFD 00007 AP-2k: `GET /v1/vpc-dhcp-leases?vpc=<uuid>`. Flat list
/// scoped to a VPC.
pub(crate) async fn list_dhcp_leases_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::VpcDhcpQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<DhcpLease>>, HttpError> {
    use tritond_api::v1::{ResultsPage, VpcDhcpQuery};
    let ctx = rqctx.context();
    let VpcDhcpQuery { scope, vpc } = query.into_inner();
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
            "GET /v1/vpc-dhcp-leases requires `?vpc=<uuid>`".to_string(),
        )
    })?;
    let vpc_row = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpLeaseList,
        vpc_row.tenant_id,
    )
    .await?;
    let leases = ctx
        .store
        .list_dhcp_leases(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(leases)))
}

/// RFD 00007 AP-2k: `GET /v1/vpc-dhcp-leases/{mac}`. Bare-MAC
/// lookup using the AP-1c `dhcp_lease/by_mac/` index - cross-VPC by
/// design (MAC is unique by invariant).
pub(crate) async fn get_dhcp_lease_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::DhcpMacPath>,
) -> Result<HttpResponseOk<DhcpLease>, HttpError> {
    let ctx = rqctx.context();
    let mac = path.into_inner().mac;
    let lease = ctx
        .store
        .find_dhcp_lease_by_mac(&mac)
        .await
        .map_err(store_error_to_http)?;
    // Recover the owning VPC -> project -> tenant for auth.
    let vpc = ctx
        .store
        .get_vpc(lease.vpc_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpLeaseGet,
        vpc.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(lease))
}

/// RFD 00007 AP-2k: `GET /v1/vpc-dhcp-reservations?vpc=<uuid>`.
pub(crate) async fn list_dhcp_reservations_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::VpcDhcpQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<DhcpReservation>>, HttpError> {
    use tritond_api::v1::{ResultsPage, VpcDhcpQuery};
    let ctx = rqctx.context();
    let VpcDhcpQuery { scope, vpc } = query.into_inner();
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
            "GET /v1/vpc-dhcp-reservations requires `?vpc=<uuid>`".to_string(),
        )
    })?;
    let vpc_row = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpReservationList,
        vpc_row.tenant_id,
    )
    .await?;
    let reservations = ctx
        .store
        .list_dhcp_reservations(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(reservations)))
}

pub(crate) async fn get_vpc_dhcp_pool(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Option<DhcpPool>>, HttpError> {
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
        Action::DhcpPoolGet,
        tenant_id,
    )
    .await?;
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let pool = ctx
        .store
        .get_dhcp_pool(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(pool))
}

pub(crate) async fn set_vpc_dhcp_pool(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
    body: TypedBody<NewDhcpPool>,
) -> Result<HttpResponseOk<DhcpPool>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcPath {
        tenant_id,
        project_id,
        vpc_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpPoolSet,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    match ctx.store.set_dhcp_pool(vpc_id, body.into_inner()).await {
        Ok(pool) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpPoolSet,
                    request_id,
                    Some(format!("DhcpPool::\"{vpc_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("DhcpPool::\"{vpc_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "lease_seconds_default": pool.lease_seconds_default,
                    }),
                )
                .await;
            Ok(HttpResponseOk(pool))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpPoolSet,
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

pub(crate) async fn clear_vpc_dhcp_pool(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcPath {
        tenant_id,
        project_id,
        vpc_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpPoolClear,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    match ctx.store.clear_dhcp_pool(vpc_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpPoolClear,
                    request_id,
                    Some(format!("DhcpPool::\"{vpc_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("DhcpPool::\"{vpc_id}\"")),
                    },
                    serde_json::json!({"vpc_id": vpc_id}),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpPoolClear,
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

pub(crate) async fn list_vpc_dhcp_reservations(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<DhcpReservation>>, HttpError> {
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
        Action::DhcpReservationList,
        tenant_id,
    )
    .await?;
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let rs = ctx
        .store
        .list_dhcp_reservations(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(rs))
}

pub(crate) async fn create_vpc_dhcp_reservation(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
    body: TypedBody<NewDhcpReservation>,
) -> Result<HttpResponseCreated<DhcpReservation>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcPath {
        tenant_id,
        project_id,
        vpc_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpReservationCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let req = body.into_inner();
    match ctx.store.create_dhcp_reservation(vpc_id, req).await {
        Ok(r) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpReservationCreate,
                    request_id,
                    Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, r.mac)),
                    AuditOutcome::Success {
                        resource: Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, r.mac)),
                    },
                    serde_json::json!({
                        "vpc_id": vpc_id,
                        "mac": r.mac,
                        "ipv4": r.ipv4.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseCreated(r))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpReservationCreate,
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

pub(crate) async fn get_vpc_dhcp_reservation(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcDhcpMacPath>,
) -> Result<HttpResponseOk<DhcpReservation>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcDhcpMacPath {
        tenant_id,
        project_id,
        vpc_id,
        mac,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpReservationGet,
        tenant_id,
    )
    .await?;
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let r = ctx
        .store
        .get_dhcp_reservation(vpc_id, &mac)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(r))
}

pub(crate) async fn delete_vpc_dhcp_reservation(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcDhcpMacPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcDhcpMacPath {
        tenant_id,
        project_id,
        vpc_id,
        mac,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpReservationDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    match ctx.store.delete_dhcp_reservation(vpc_id, &mac).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpReservationDelete,
                    request_id,
                    Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, mac)),
                    AuditOutcome::Success {
                        resource: Some(format!("DhcpReservation::\"{}/{}\"", vpc_id, mac)),
                    },
                    serde_json::json!({"vpc_id": vpc_id, "mac": mac}),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpReservationDelete,
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

pub(crate) async fn list_vpc_dhcp_leases(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<DhcpLease>>, HttpError> {
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
        Action::DhcpLeaseList,
        tenant_id,
    )
    .await?;
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let l = ctx
        .store
        .list_dhcp_leases(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(l))
}

pub(crate) async fn get_vpc_dhcp_lease(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcDhcpMacPath>,
) -> Result<HttpResponseOk<DhcpLease>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcDhcpMacPath {
        tenant_id,
        project_id,
        vpc_id,
        mac,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpLeaseGet,
        tenant_id,
    )
    .await?;
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    let l = ctx
        .store
        .get_dhcp_lease(vpc_id, &mac)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(l))
}

pub(crate) async fn delete_vpc_dhcp_lease(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcDhcpMacPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcDhcpMacPath {
        tenant_id,
        project_id,
        vpc_id,
        mac,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpLeaseDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    check_vpc_parentage(ctx.store.as_ref(), vpc_id, tenant_id, project_id).await?;
    match ctx.store.delete_dhcp_lease(vpc_id, &mac).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpLeaseDelete,
                    request_id,
                    Some(format!("DhcpLease::\"{}/{}\"", vpc_id, mac)),
                    AuditOutcome::Success {
                        resource: Some(format!("DhcpLease::\"{}/{}\"", vpc_id, mac)),
                    },
                    serde_json::json!({"vpc_id": vpc_id, "mac": mac}),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::DhcpLeaseDelete,
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
