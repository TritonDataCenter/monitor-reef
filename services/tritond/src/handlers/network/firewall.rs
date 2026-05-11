// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `network::firewall` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn list_vpc_firewall_rules(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError> {
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
        Action::FirewallRuleList,
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
    let rules = ctx
        .store
        .list_firewall_rules_in_vpc(vpc_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(rules))
}

pub(crate) async fn create_vpc_firewall_rule(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcPath>,
    body: TypedBody<NewFirewallRule>,
) -> Result<HttpResponseCreated<FirewallRule>, HttpError> {
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
        Action::FirewallRuleCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    match ctx
        .store
        .create_firewall_rule(tenant_id, project_id, vpc_id, req)
        .await
    {
        Ok(rule) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FirewallRuleCreate,
                    request_id,
                    Some(format!("FirewallRule::\"{}\"", rule.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("FirewallRule::\"{}\"", rule.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "vpc_id": vpc_id,
                        "name": rule.name,
                        "priority": rule.priority,
                        "direction": rule.direction,
                        "action": rule.action,
                        "protocol": rule.protocol,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(rule))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FirewallRuleCreate,
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

pub(crate) async fn delete_vpc_firewall_rule(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectVpcFirewallRulePath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectVpcFirewallRulePath {
        tenant_id,
        project_id,
        vpc_id,
        firewall_rule_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FirewallRuleDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let rule = ctx
        .store
        .get_firewall_rule(firewall_rule_id)
        .await
        .map_err(store_error_to_http)?;
    if rule.tenant_id != tenant_id || rule.project_id != project_id || rule.vpc_id != vpc_id {
        return Err(not_found());
    }
    match ctx.store.delete_firewall_rule(firewall_rule_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FirewallRuleDelete,
                    request_id,
                    Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
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
                    Action::FirewallRuleDelete,
                    request_id,
                    Some(format!("FirewallRule::\"{firewall_rule_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}
