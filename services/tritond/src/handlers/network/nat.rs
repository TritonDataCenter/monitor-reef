// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::nat` HTTP handlers (delegated to from the `TritondApi` impl).

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

/// RFD 00007 AP-2j: `GET /v1/nat-gateways?vpc=<uuid>`. Flat list.
pub(crate) async fn list_nat_gateways_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::NatGatewayQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<NatGateway>>, HttpError> {
    use tritond_api::v1::{NatGatewayQuery, ResultsPage};
    let ctx = rqctx.context();
    let NatGatewayQuery { scope, vpc } = query.into_inner();
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
            "GET /v1/nat-gateways requires `?vpc=<uuid>`".to_string(),
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
        Action::NatGatewayList,
        vpc_row.tenant_id,
    )
    .await?;
    let nats = ctx
        .store
        .list_nat_gateways_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(nats)))
}

/// RFD 00007 AP-2j: `GET /v1/nat-gateways/{nat_gateway_id}`.
pub(crate) async fn get_nat_gateway_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::NatGatewayPath>,
) -> Result<HttpResponseOk<NatGateway>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::NatGatewayPath { nat_gateway_id } = path.into_inner();
    let nat = ctx
        .store
        .get_nat_gateway(nat_gateway_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NatGatewayGet,
        nat.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(nat))
}

/// RFD 00007 AP-3a-13: `POST /v1/nat-gateways?vpc=<uuid>`.
/// Same `nat_gateway_create` saga as the legacy v2 path; the
/// tenant+project are resolved from the parent VPC.
pub(crate) async fn create_nat_gateway_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::NatGatewayQuery>,
    body: TypedBody<NewNatGateway>,
) -> Result<HttpResponseCreated<NatGateway>, HttpError> {
    let q = query.into_inner();
    let vpc_id = q.vpc.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/nat-gateways requires `?vpc=<uuid>`".to_string(),
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
        Action::NatGatewayCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let saga_params = crate::sagas::nat_gateway::NatGatewayCreateParams {
        tenant_id,
        project_id,
        vpc_id,
        request: req,
    };
    let saga_dag = crate::sagas::nat_gateway::build_create_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("nat-gateway-create saga dag build: {e}"))
    })?;
    let saga_refs = crate::sagas::nat_gateway::build_create_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::nat_gateway::SAGA_NAME_CREATE,
            crate::sagas::nat_gateway::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("nat-gateway-create saga executor: {e}"))
        })?;
    match steno_result.kind {
        Ok(ok) => {
            let nat_gateway: NatGateway = ok.lookup_node_output("nat_gateway").map_err(|e| {
                HttpError::for_internal_error(format!(
                    "nat-gateway-create saga finished but output missing: {e}"
                ))
            })?;
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::NatGatewayCreate,
                    request_id,
                    Some(format!("NatGateway::\"{}\"", nat_gateway.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("NatGateway::\"{}\"", nat_gateway.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "name": nat_gateway.name,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseCreated(nat_gateway))
        }
        Err(err) => {
            map_nat_saga_err(
                &ctx.audit,
                &principal,
                Action::NatGatewayCreate,
                request_id,
                None,
                saga_id,
                &err,
            )
            .await
        }
    }
}

/// RFD 00007 AP-3a-13: `DELETE /v1/nat-gateways/{nat_gateway_id}`.
pub(crate) async fn delete_nat_gateway_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::NatGatewayPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::NatGatewayPath { nat_gateway_id } = path.into_inner();
    let nat_gateway = ctx
        .store
        .get_nat_gateway(nat_gateway_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = nat_gateway.tenant_id;
    let project_id = nat_gateway.project_id;
    let vpc_id = nat_gateway.vpc_id;

    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NatGatewayDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let saga_params = crate::sagas::nat_gateway::NatGatewayDeleteParams {
        tenant_id,
        project_id,
        vpc_id,
        nat_gateway_id,
    };
    let saga_dag = crate::sagas::nat_gateway::build_delete_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("nat-gateway-delete saga dag build: {e}"))
    })?;
    let saga_refs = crate::sagas::nat_gateway::build_delete_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::nat_gateway::SAGA_NAME_DELETE,
            crate::sagas::nat_gateway::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("nat-gateway-delete saga executor: {e}"))
        })?;
    match steno_result.kind {
        Ok(_) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::NatGatewayDelete,
                    request_id,
                    Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(err) => {
            map_nat_saga_err(
                &ctx.audit,
                &principal,
                Action::NatGatewayDelete,
                request_id,
                Some(format!("NatGateway::\"{nat_gateway_id}\"")),
                saga_id,
                &err,
            )
            .await
        }
    }
}

pub(crate) async fn list_vpc_nat_gateways(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<NatGateway>>, HttpError> {
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
        Action::NatGatewayList,
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
    let nat_gateways = ctx
        .store
        .list_nat_gateways_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(nat_gateways))
}

/// Map a NAT-saga failure back to an HTTP error using the same
/// kind-string convention shared across the saga catalog.
async fn map_nat_saga_err<T>(
    audit: &crate::audit::AuditService,
    principal: &crate::auth::Principal,
    action: Action,
    request_id: Option<Uuid>,
    resource: Option<String>,
    saga_id: tritond_saga::SagaId,
    err: &tritond_saga::SagaResultErr,
) -> Result<T, HttpError> {
    let kind_msg: Option<(&'static str, String)> = match &err.error_source {
        tritond_saga::ActionError::ActionFailed { source_error } => {
            let kind = crate::sagas::nat_gateway::decode_store_error_kind(source_error);
            let msg = source_error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            kind.map(|k| (k, msg))
        }
        _ => None,
    };
    let http_err = match kind_msg.as_ref().map(|(k, _)| *k) {
        Some("not_found") => not_found(),
        Some("conflict") => HttpError::for_client_error(
            Some("Conflict".to_string()),
            dropshot::ClientErrorStatusCode::CONFLICT,
            kind_msg
                .as_ref()
                .map(|(_, m)| m.clone())
                .unwrap_or_default(),
        ),
        _ => HttpError::for_internal_error(format!(
            "nat-gateway saga failed at {:?}: {:?}",
            err.error_node_name, err.error_source
        )),
    };
    audit
        .record_mutation(
            principal,
            action,
            request_id,
            resource,
            tritond_audit::Outcome::ServerError {
                message: format!("{:?}", err.error_source),
            },
            serde_json::json!({
                "operation_id": saga_id.0.to_string(),
            }),
        )
        .await;
    Err(http_err)
}

pub(crate) async fn get_vpc_nat_gateway(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcNatGatewayPath>,
) -> Result<HttpResponseOk<NatGateway>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcNatGatewayPath {
        tenant_id,
        project_id,
        vpc_id,
        nat_gateway_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NatGatewayGet,
        tenant_id,
    )
    .await?;
    let nat_gateway = ctx
        .store
        .get_nat_gateway(nat_gateway_id)
        .await
        .map_err(store_error_to_http)?;
    if nat_gateway.tenant_id != tenant_id
        || nat_gateway.project_id != project_id
        || nat_gateway.vpc_id != vpc_id
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(nat_gateway))
}
