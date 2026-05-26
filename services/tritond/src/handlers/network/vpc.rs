// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::vpc` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_project_vpcs(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Vec<Vpc>>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::VpcList,
        tenant_id,
    )
    .await?;

    // Verify the project actually lives in the path's tenant. A
    // project_id that names some other tenant's project is treated
    // as not-found; this stops cross-tenant enumeration via the
    // VPC list endpoint.
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let vpcs = ctx
        .store
        .list_vpcs_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(vpcs))
}

/// RFD 00007 AP-2g: `GET /v1/vpcs?tenant=&project=`. Flat VPC list
/// scoped to a tenant + project. Both selectors required at AP-2g.
pub(crate) async fn list_vpcs_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::VpcQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Vpc>>, HttpError> {
    use tritond_api::v1::{ResultsPage, VpcQuery};
    let ctx = rqctx.context();
    let VpcQuery { scope } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    let tenant_id = scope.tenant.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/vpcs requires `?tenant=<uuid>&project=<uuid>` selectors".to_string(),
        )
    })?;
    let project_id = scope.project.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/vpcs requires `?tenant=<uuid>&project=<uuid>` selectors".to_string(),
        )
    })?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::VpcList,
        tenant_id,
    )
    .await?;
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let vpcs = ctx
        .store
        .list_vpcs_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(vpcs)))
}

/// RFD 00007 AP-2g: `GET /v1/vpcs/{vpc_id}`. Flat single-VPC read.
pub(crate) async fn get_vpc_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::VpcPath>,
) -> Result<HttpResponseOk<Vpc>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::VpcPath { vpc_id } = path.into_inner();
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
        Action::VpcGet,
        vpc.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(vpc))
}

/// RFD 00007 AP-3a-10: `POST /v1/vpcs?tenant=&project=`. Same
/// behaviour as the legacy `create_project_vpc`, but takes the
/// scope from query selectors. `silo=` is rejected at the customer
/// surface; both `tenant=` and `project=` are required.
pub(crate) async fn create_vpc_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::ScopeSelectors>,
    body: TypedBody<NewVpc>,
) -> Result<HttpResponseCreated<Vpc>, HttpError> {
    let scope = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is not accepted on the customer surface; \
             /v1/vpcs always creates inside the principal's silo"
                .to_string(),
        ));
    }
    let tenant_id = scope.tenant.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/vpcs requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;
    let project_id = scope.project.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/vpcs requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;

    let ctx = rqctx.context();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::VpcCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // At least one IP family is required (matches OPTE's IpCfg
    // enum: Ipv4, Ipv6, or DualStack — never neither).
    if req.ipv4_block.is_none() && req.ipv6_block.is_none() {
        let outcome = AuditOutcome::ClientError {
            code: 400,
            message: "vpc must specify ipv4_block, ipv6_block, or both".to_string(),
        };
        ctx.audit
            .record_mutation(
                &principal,
                Action::VpcCreate,
                request_id,
                None,
                outcome,
                serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
            )
            .await;
        return Err(HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            "vpc must specify ipv4_block, ipv6_block, or both".to_string(),
        ));
    }

    match ctx.store.create_vpc(tenant_id, project_id, req).await {
        Ok(vpc) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::VpcCreate,
                    request_id,
                    Some(format!("Vpc::\"{}\"", vpc.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Vpc::\"{}\"", vpc.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "name": vpc.name,
                        "vni": vpc.vni,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(vpc))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::VpcCreate,
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

/// RFD 00007 AP-3a-10: `DELETE /v1/vpcs/{vpc_id}`. Resolves the
/// owning tenant from the VPC row (no path-level defence-in-depth
/// because /v1/ singletons are by id only); the store enforces
/// the dependency gate (subnets, firewall rules, NAT gateways,
/// route tables) and returns 409 Conflict if anything still
/// references the VPC.
pub(crate) async fn delete_vpc_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::VpcPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::VpcPath { vpc_id } = path.into_inner();
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
        Action::VpcDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    match ctx.store.delete_vpc(vpc_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::VpcDelete,
                    request_id,
                    Some(format!("Vpc::\"{vpc_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Vpc::\"{vpc_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::VpcDelete,
                    request_id,
                    Some(format!("Vpc::\"{vpc_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_project_vpc(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vpc>, HttpError> {
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
        Action::VpcGet,
        tenant_id,
    )
    .await?;
    let vpc = ctx
        .store
        .get_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    // Defence-in-depth: the VPC must live in *both* the path's
    // silo and the path's project. Mismatch on either dimension is
    // a 404 so cross-tenant probes don't learn the resource exists
    // somewhere else.
    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
        return Err(not_found());
    }
    Ok(HttpResponseOk(vpc))
}
