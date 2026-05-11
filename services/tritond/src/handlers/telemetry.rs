// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `telemetry` HTTP handlers (delegated to from the `TritondApi` impl).

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
use crate::service_impl::resolve_metrics_range;

pub(crate) async fn agent_metrics_ingest(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<tritond_metrics::SampleBatch>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let ctx = rqctx.context();
    // Piggyback on AgentStatus's authz envelope: same scope
    // (Agent), same auditing characteristics (high-frequency
    // sample stream, no per-call audit). When per-action
    // granularity is needed for forensics we'll add a dedicated
    // AgentMetricsIngest variant.
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentStatus,
    )
    .await?;
    let _server_uuid = require_bound_cn(&principal)?;

    let batch = body.into_inner();
    if batch.samples.len() > tritond_metrics::SampleBatch::MAX_SAMPLES {
        return Err(HttpError::for_client_error(
            None,
            dropshot::ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "metrics batch of {} samples exceeds limit of {}",
                batch.samples.len(),
                tritond_metrics::SampleBatch::MAX_SAMPLES,
            ),
        ));
    }

    // Best-effort: a metrics-store hiccup must not 5xx the agent
    // and put it into backoff. Log and ack.
    if let Err(e) = ctx.metrics.insert(&batch.samples).await {
        tracing::warn!(error = %e, count = batch.samples.len(), "metrics insert failed");
    }
    Ok(HttpResponseUpdatedNoContent())
}

pub(crate) async fn instance_metrics_range(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    query: Query<MetricsRangeQuery>,
) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    // Reuse InstanceGet authz: read access to the named
    // instance is the same trust envelope as the metrics view.
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceGet,
        tenant_id,
    )
    .await?;

    // Verify the instance actually belongs to this tenant +
    // project. Mirrors `get_project_instance` -- we never want
    // to leak metrics across the tenant boundary even if the
    // metrics store happens to hold matching samples.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }

    let q = query.into_inner();
    let (since, until, step) = resolve_metrics_range(q.range.as_deref())?;
    let schema = q
        .schema
        .unwrap_or_else(|| tritond_metrics::schema::schemas::CPU_PER_ZONE.to_string());

    let range_query = tritond_metrics::RangeQuery {
        schema,
        // Filter on instance_id only: it's globally unique, and
        // the agent's per-zone samples don't carry tenant_id in
        // their identity (the agent doesn't know it). The
        // tenant/project ownership check above already gates
        // access to this instance's data.
        instance_id: Some(instance_id),
        tenant_id: None,
        cn_id: None,
        since,
        until,
        step,
    };
    let result = ctx
        .metrics
        .query_range(&range_query)
        .await
        .map_err(metrics_error_to_http)?;
    Ok(HttpResponseOk(result))
}

pub(crate) async fn cn_metrics_range(
    rqctx: RequestContext<ApiContext>,
    path: Path<CnPath>,
    query: Query<MetricsRangeQuery>,
) -> Result<HttpResponseOk<tritond_metrics::RangeResult>, HttpError> {
    let ctx = rqctx.context();
    let server_uuid = path.into_inner().server_uuid;
    // Same authz envelope as `get_cn` -- fleet-read access to
    // CN inventory implies read access to per-CN metrics.
    authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::CnGet).await?;
    // Verify the CN actually exists. Without this, a stale UUID
    // returns an empty series with no signal that the CN is
    // unknown to the control plane.
    ctx.store
        .get_cn(server_uuid)
        .await
        .map_err(store_error_to_http)?;

    let q = query.into_inner();
    let (since, until, step) = resolve_metrics_range(q.range.as_deref())?;
    let schema = q
        .schema
        .unwrap_or_else(|| tritond_metrics::schema::schemas::CPU_PER_CN.to_string());

    let range_query = tritond_metrics::RangeQuery {
        schema,
        instance_id: None,
        tenant_id: None,
        cn_id: Some(server_uuid),
        since,
        until,
        step,
    };
    let result = ctx
        .metrics
        .query_range(&range_query)
        .await
        .map_err(metrics_error_to_http)?;
    Ok(HttpResponseOk(result))
}

pub(crate) async fn agent_logs_ingest(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<tritond_logs::LogBatch>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let ctx = rqctx.context();
    // Same authz envelope as metrics ingest: Agent scope, bound
    // CN. We don't dedicate a Cedar action for now -- log batches
    // and status reports are the same trust shape.
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentStatus,
    )
    .await?;
    let _server_uuid = require_bound_cn(&principal)?;

    let batch = body.into_inner();
    if batch.lines.len() > tritond_logs::LogBatch::MAX_LINES {
        return Err(HttpError::for_client_error(
            None,
            dropshot::ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "log batch of {} lines exceeds limit of {}",
                batch.lines.len(),
                tritond_logs::LogBatch::MAX_LINES,
            ),
        ));
    }

    // Fail-open: a log-store hiccup must not put the agent into
    // backoff. Log + ack.
    if let Err(e) = ctx.logs.insert(batch).await {
        tracing::warn!(error = %e, "log batch insert failed");
    }
    Ok(HttpResponseUpdatedNoContent())
}

pub(crate) async fn instance_logs_tail(
    rqctx: RequestContext<ApiContext>,
    path: Path<InstanceLogsPath>,
    query: Query<LogTailQuery>,
) -> Result<HttpResponseOk<tritond_logs::LogTailResult>, HttpError> {
    let ctx = rqctx.context();
    let InstanceLogsPath {
        tenant_id,
        project_id,
        instance_id,
        source,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceGet,
        tenant_id,
    )
    .await?;

    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }

    let parsed_source: tritond_logs::LogSource =
        source
            .parse()
            .map_err(|e: tritond_logs::types::UnknownLogSource| {
                HttpError::for_bad_request(None, e.to_string())
            })?;

    let q = query.into_inner();
    let lines_req = q.lines.unwrap_or(500);
    let tq = tritond_logs::LogTailQuery {
        instance_id,
        source: parsed_source,
        lines: lines_req,
        before_seq: q.before_seq,
    };
    let result = ctx.logs.tail(&tq).await.map_err(logs_error_to_http)?;
    Ok(HttpResponseOk(result))
}
