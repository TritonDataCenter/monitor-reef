// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::subnet` HTTP handlers (delegated to from the `TritondApi` impl).

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

/// RFD 00007 AP-2g: `GET /v1/subnets?vpc=<uuid>`. Flat subnet list
/// scoped to a VPC. The handler reads the parent VPC to recover the
/// owning tenant for auth (matches the legacy /v2 invariant).
pub(crate) async fn list_subnets_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::SubnetQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Subnet>>, HttpError> {
    use tritond_api::v1::{ResultsPage, SubnetQuery};
    let ctx = rqctx.context();
    let SubnetQuery { scope, vpc } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints"
                .to_string(),
        ));
    }
    let vpc_id = vpc.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/subnets requires `?vpc=<uuid>`".to_string(),
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
        Action::SubnetList,
        vpc_row.tenant_id,
    )
    .await?;
    let subnets = ctx
        .store
        .list_subnets_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(subnets)))
}

/// RFD 00007 AP-2g: `GET /v1/subnets/{subnet_id}`. Flat single-subnet
/// read; recovers the owning tenant from the row.
pub(crate) async fn get_subnet_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SubnetPath>,
) -> Result<HttpResponseOk<Subnet>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::SubnetPath { subnet_id } = path.into_inner();
    let subnet = ctx
        .store
        .get_subnet(subnet_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SubnetGet,
        subnet.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(subnet))
}

pub(crate) async fn list_vpc_subnets(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<Subnet>>, HttpError> {
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
        Action::SubnetList,
        tenant_id,
    )
    .await?;

    // Verify the parent VPC actually lives under the path's
    // silo+project. Cross-silo or cross-project list paths must
    // 404 — the cross-tenant enumeration invariant extends to
    // VPCs the way it does for projects in `list_project_vpcs`.
    let vpc = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
        return Err(not_found());
    }
    let subnets = ctx
        .store
        .list_subnets_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(subnets))
}

pub(crate) async fn create_vpc_subnet(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
    body: TypedBody<NewSubnet>,
) -> Result<HttpResponseCreated<Subnet>, HttpError> {
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
        Action::SubnetCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // At least one IP family is required, mirroring the VPC
    // create-time invariant. Same OPTE rationale: an `IpCfg`
    // must be Ipv4, Ipv6, or DualStack.
    if req.ipv4_block.is_none() && req.ipv6_block.is_none() {
        let outcome = AuditOutcome::ClientError {
            code: 400,
            message: "subnet must specify ipv4_block, ipv6_block, or both".to_string(),
        };
        ctx.audit
            .record_mutation(
                &principal,
                Action::SubnetCreate,
                request_id,
                None,
                outcome,
                serde_json::json!({
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "vpc_id": vpc_id,
                }),
            )
            .await;
        return Err(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            "subnet must specify ipv4_block, ipv6_block, or both".to_string(),
        ));
    }

    match ctx
        .store
        .create_subnet(tenant_id, project_id, vpc_id, req)
        .await
    {
        Ok(subnet) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::SubnetCreate,
                    request_id,
                    Some(format!("Subnet::\"{}\"", subnet.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Subnet::\"{}\"", subnet.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "name": subnet.name,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(subnet))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::SubnetCreate,
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

pub(crate) async fn get_vpc_subnet(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcSubnetPath>,
) -> Result<HttpResponseOk<Subnet>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcSubnetPath {
        tenant_id,
        project_id,
        vpc_id,
        subnet_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SubnetGet,
        tenant_id,
    )
    .await?;
    let subnet = ctx
        .store
        .get_subnet(subnet_id)
        .await
        .map_err(store_error_to_http)?;
    // Defence-in-depth: subnet must live in path silo + project + vpc.
    if subnet.tenant_id != tenant_id || subnet.project_id != project_id || subnet.vpc_id != vpc_id {
        return Err(not_found());
    }
    Ok(HttpResponseOk(subnet))
}

pub(crate) async fn delete_vpc_subnet(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcSubnetPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcSubnetPath {
        tenant_id,
        project_id,
        vpc_id,
        subnet_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::SubnetDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let subnet = ctx
        .store
        .get_subnet(subnet_id)
        .await
        .map_err(store_error_to_http)?;
    if subnet.tenant_id != tenant_id || subnet.project_id != project_id || subnet.vpc_id != vpc_id {
        return Err(not_found());
    }
    ctx.store
        .delete_subnet(subnet_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::SubnetDelete,
            request_id,
            Some(format!("Subnet::\"{subnet_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Subnet::\"{subnet_id}\"")),
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
