// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `config` HTTP handlers (delegated to from the `TritondApi` impl).

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
use crate::service_impl::{build_config_entry, config_key_or_404};

pub(crate) async fn list_config(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<ConfigEntry>>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ConfigList,
    )
    .await?;
    let settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    let entries = ConfigKey::ALL
        .into_iter()
        .map(|k| build_config_entry(k, &settings))
        .collect();
    Ok(HttpResponseOk(entries))
}

pub(crate) async fn get_config(
    rqctx: RequestContext<ApiContext>,
    path: Path<ConfigKeyPath>,
) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::ConfigGet)
        .await?;
    let key = config_key_or_404(&path.into_inner().key)?;
    let settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(build_config_entry(key, &settings)))
}

pub(crate) async fn set_config(
    rqctx: RequestContext<ApiContext>,
    path: Path<ConfigKeyPath>,
    body: TypedBody<SetConfigRequest>,
) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::ConfigSet)
            .await?;
    let request_id = parse_request_id(&rqctx);
    let key = config_key_or_404(&path.into_inner().key)?;
    let new_value = body.into_inner().value;

    let mut settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    let previous = settings.get(key);
    settings.set(key, new_value).map_err(|e| match e {
        ConfigError::InvalidValue { key, message } => HttpError::for_bad_request(
            Some("BadRequest".to_string()),
            format!("invalid value for {key}: {message}"),
        ),
        ConfigError::UnknownKey(k) => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            format!("unknown config key: {k}"),
        ),
    })?;
    ctx.store
        .put_settings(settings.clone())
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::ConfigSet,
            request_id,
            Some(format!("Config::\"{}\"", key.as_str())),
            AuditOutcome::Success {
                resource: Some(format!("Config::\"{}\"", key.as_str())),
            },
            serde_json::json!({
                "key": key.as_str(),
                "previous": previous,
                "value": settings.get(key),
            }),
        )
        .await;
    Ok(HttpResponseOk(build_config_entry(key, &settings)))
}

pub(crate) async fn reset_config(
    rqctx: RequestContext<ApiContext>,
    path: Path<ConfigKeyPath>,
) -> Result<HttpResponseOk<ConfigEntry>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ConfigReset,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let key = config_key_or_404(&path.into_inner().key)?;

    let mut settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    let previous = settings.get(key);
    settings.reset(key);
    ctx.store
        .put_settings(settings.clone())
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::ConfigReset,
            request_id,
            Some(format!("Config::\"{}\"", key.as_str())),
            AuditOutcome::Success {
                resource: Some(format!("Config::\"{}\"", key.as_str())),
            },
            serde_json::json!({
                "key": key.as_str(),
                "previous": previous,
                "value": settings.get(key),
            }),
        )
        .await;
    Ok(HttpResponseOk(build_config_entry(key, &settings)))
}
