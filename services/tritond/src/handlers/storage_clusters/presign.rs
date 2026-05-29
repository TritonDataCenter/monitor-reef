// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `storage_clusters::presign` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn set_storage_cluster_presigner(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<SetPresignerRequest>,
) -> Result<HttpResponseOk<StorageClusterView>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageClusterSetPresigner,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let req = body.into_inner();
    // Empty strings on the wire mean "clear the credentials" —
    // map them to None so the store layer's contract (Some+Some
    // or None+None) is honored.
    let akid = if req.access_key_id.is_empty() {
        None
    } else {
        Some(req.access_key_id.clone())
    };
    let secret = if req.secret_access_key.is_empty() {
        None
    } else {
        Some(req.secret_access_key)
    };
    // Audit payload deliberately captures only AKID + endpoint
    // — the secret is opaque to the audit chain just like
    // mantad's admin token.
    let audit_payload = serde_json::json!({
        "s3_endpoint": req.s3_endpoint,
        "access_key_id": akid,
    });
    match ctx
        .store
        .update_storage_cluster_presigner(id, req.s3_endpoint, akid, secret)
        .await
    {
        Ok(cluster) => {
            let view: StorageClusterView = cluster.into();
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterSetPresigner,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{id}\"")),
                    },
                    audit_payload,
                )
                .await;
            Ok(HttpResponseOk(view))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::StorageClusterSetPresigner,
                    request_id,
                    Some(format!("StorageCluster::\"{id}\"")),
                    store_error_to_audit_outcome(&e),
                    audit_payload,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn presign_storage_cluster_object_put(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<PresignPutRequest>,
) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageObjectPresignPut,
    )
    .await?;
    let _scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let request_id = parse_request_id(&rqctx);
    let id = path.into_inner().id;
    let req = body.into_inner();
    let resp = crate::storage::mint_presigned_url(
        &ctx.store,
        id,
        "PUT",
        &req.bucket,
        &req.key,
        req.expires_secs,
    )
    .await?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::StorageObjectPresignPut,
            request_id,
            Some(format!("StorageObject::\"{}/{}\"", req.bucket, req.key)),
            AuditOutcome::Success {
                resource: Some(format!("StorageObject::\"{}/{}\"", req.bucket, req.key)),
            },
            serde_json::json!({
                "bucket": req.bucket,
                "key": req.key,
                "expires_secs": req.expires_secs,
            }),
        )
        .await;
    Ok(HttpResponseOk(resp))
}

pub(crate) async fn presign_storage_cluster_object_get(
    rqctx: RequestContext<ApiContext>,
    path: Path<StorageClusterPath>,
    body: TypedBody<PresignGetRequest>,
) -> Result<HttpResponseOk<PresignResponse>, HttpError> {
    let ctx = rqctx.context();
    // Reads still get audited via authenticate_and_authorize
    // (Allow event), but we don't emit a record_mutation —
    // the GET URL doesn't change cluster state.
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::StorageObjectPresignGet,
    )
    .await?;
    let _scope = crate::storage::resolve_workspace_scope(&ctx.auth, &ctx.store, &principal).await?;
    let id = path.into_inner().id;
    let req = body.into_inner();
    let resp = crate::storage::mint_presigned_url(
        &ctx.store,
        id,
        "GET",
        &req.bucket,
        &req.key,
        req.expires_secs,
    )
    .await?;
    Ok(HttpResponseOk(resp))
}
