// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `agents` HTTP handlers (delegated to from the `TritondApi` impl).

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
    AgentConfigResponse, AgentJobPath, AgentPortBlueprint, AgentPortBlueprintPath,
    AgentStatusRequest, ApiKeyCreated,
    ApiKeyPath, ApproveCnRequest, AttachFloatingIpRequest, AuditEventList, AuditEventPath,
    AuditListQuery, AuditVerifyQuery, AuditVerifyResponse, ClaimJobRequest, ClaimJobResponse,
    CnListQuery, CnPath, CompleteJobRequest, ConfigEntry, ConfigKeyPath, DhcpLeaseActivityReport,
    HealthResponse, ImagePath, InstanceDeleteQuery, InstanceLogsPath, LegacyCnSummary,
    LegacyVmListQuery, LegacyVmPath, LogTailQuery, LoginRequest, MetricsRangeQuery,
    NetworkRealizationRequest, NewApiKey, NewIdpConfig, NewImageFromBundle, OpenAutoApproveRequest,
    ProvisioningBlueprint, RefreshRequest, RegisterCnRequest, RegisterCnResponse,
    RegisterNicTagProvision, RegisterStatusQuery, RegisterStatusResponse, SetCnRoleRequest,
    SetConfigRequest, SiloPath,
    SiloTenantPath, SshKeyPath, StorageClusterAccessKeyPath, StorageClusterBucketPath,
    StorageClusterNodePath, StorageClusterPath, StorageClusterUserPath,
    StorageClusterUserPolicyPath, TenantIdpPath, TenantPath, TenantProjectFloatingIpPath,
    TenantProjectInstanceDiskPath, TenantProjectInstanceNicPath, TenantProjectInstancePath,
    TenantProjectPath, TenantProjectVpcDhcpMacPath, TenantProjectVpcFirewallRulePath,
    TenantProjectVpcNatGatewayPath, TenantProjectVpcPath, TenantProjectVpcRouteTablePath,
    TenantProjectVpcRouteTableRoutePath, TenantProjectVpcSubnetPath, TokenResponse, TritondApi,
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
    AUTO_APPROVE_WINDOW_MAX, ApiKey, CnNicTagInventory, CnState, ConfigError, ConfigKey, IdpConfig,
    NicTagProvision, Store, StoreError, normalize_claim_code,
};
use uuid::Uuid;

use crate::auth::{
    Action, AuthService, Principal, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant, require_authenticated,
};

use crate::VERSION;

/// Concrete implementor of [`TritondApi`].
use crate::context::ApiContext;

pub(crate) async fn agent_claim_job(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<ClaimJobRequest>,
) -> Result<HttpResponseOk<ClaimJobResponse>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentClaim,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    // Per-CN binding: a key minted for CN-A cannot claim as
    // CN-B. The string `claimed_by` must parse as the bound
    // server_uuid. Unbound keys (operator-minted) skip the
    // check; their `claimed_by` stays free-text.
    if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
        let claimed_uuid = Uuid::parse_str(&req.claimed_by).map_err(|_| {
            HttpError::for_client_error(
                Some("Forbidden".to_string()),
                ClientErrorStatusCode::FORBIDDEN,
                "bound api key requires claimed_by to be a uuid".to_string(),
            )
        })?;
        crate::auth::enforce_cn_binding(Some(bound), claimed_uuid)?;
    }
    // The store returns NotFound when the queue is empty; we
    // turn that into the wire-level "no work" signal so the
    // agent can poll on a timer without 404 noise.
    // Pass the bound CN through as the claimer identity.
    // Unbound claimers (the in-process stub or a legacy
    // operator-minted Agent key) get only unrouted jobs.
    let claimer_cn = crate::auth::principal_bound_cn(&principal);
    let job = match ctx.store.claim_next_job(&req.claimed_by, claimer_cn).await {
        Ok(job) => Some(job),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };
    // Audit only successful claims — empty-queue polls are noise.
    if let Some(j) = &job {
        ctx.audit
            .record_mutation(
                &principal,
                Action::AgentClaim,
                request_id,
                Some(format!("ProvisioningJob::\"{}\"", j.id)),
                AuditOutcome::Success {
                    resource: Some(format!("ProvisioningJob::\"{}\"", j.id)),
                },
                serde_json::json!({
                    "job_id": j.id,
                    "claimed_by": req.claimed_by,
                    "kind": j.kind,
                }),
            )
            .await;
        // Drive the instance lifecycle forward. For a Provision
        // job this advances Pending → Provisioning so operators
        // see the in-flight state. Stop / Restart already moved
        // the instance to Stopping in the operator-facing
        // handler, so claim has nothing to advance there. CAS
        // failures (instance gone, lifecycle drift) are logged
        // but don't fail the claim — the agent has the job and
        // will fail at vmadm time if the instance really is
        // gone, surfacing a clean Failed back to the operator.
        drive_lifecycle_for_claim(ctx.store.as_ref(), j).await;
    }
    Ok(HttpResponseOk(ClaimJobResponse { job }))
}

pub(crate) async fn agent_job_blueprint(
    rqctx: RequestContext<ApiContext>,
    path: Path<AgentJobPath>,
) -> Result<HttpResponseOk<ProvisioningBlueprint>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;
    let job_id = path.into_inner().job_id;
    let job = ctx
        .store
        .get_job(job_id)
        .await
        .map_err(store_error_to_http)?;
    // Per-CN binding: a bound key may only fetch blueprints
    // for jobs it itself claimed. Unbound keys see anything.
    if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
        enforce_job_belongs_to_bound_cn(&job, bound)?;
    }
    let blueprint = build_blueprint(ctx.store.as_ref(), &ctx.identity_hmac_key, &job).await?;
    Ok(HttpResponseOk(blueprint))
}

pub(crate) async fn agent_port_blueprint(
    rqctx: RequestContext<ApiContext>,
    path: Path<AgentPortBlueprintPath>,
) -> Result<HttpResponseOk<AgentPortBlueprint>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;
    let bound_cn = require_bound_cn(&principal)?;
    let port_id = path.into_inner().port_id;
    let blueprint = build_port_blueprint(ctx.store.as_ref(), port_id, bound_cn).await?;
    Ok(HttpResponseOk(blueprint))
}

/// Called on every kmod v2p cache miss; 404 lets the agent install a
/// negative-cache entry.
pub(crate) async fn agent_peer_resolve(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::AgentPeerResolveQuery>,
) -> Result<HttpResponseOk<tritond_api::AgentPeerResolveResponse>, HttpError> {
    use std::net::IpAddr;
    use std::str::FromStr;
    let ctx = rqctx.context();
    // No instance-claim check: peer resolution only discloses
    // {mac, host CN} which the agent already sees in its own
    // port blueprints.
    let _principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;
    let q = query.into_inner();
    let peer_ip = IpAddr::from_str(&q.ip).map_err(|_| {
        HttpError::for_client_error(
            Some("BadRequest".to_string()),
            dropshot::ClientErrorStatusCode::BAD_REQUEST,
            format!("invalid peer ip: {}", q.ip),
        )
    })?;
    crate::blueprint::resolve_peer(ctx.store.as_ref(), q.vni, peer_ip)
        .await
        .map(HttpResponseOk)
}

/// Long-poll: returns events strictly after the `since` cursor plus
/// a fresh `tail_seq`.
pub(crate) async fn agent_peer_invalidations(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::AgentPeerInvalidationsQuery>,
) -> Result<HttpResponseOk<tritond_api::AgentPeerInvalidationsResponse>, HttpError> {
    let ctx = rqctx.context();
    let _principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;
    let q = query.into_inner();
    let (invalidations, tail_seq) = ctx.peer_invalidations.drain_since(q.since);
    Ok(HttpResponseOk(
        tritond_api::AgentPeerInvalidationsResponse {
            invalidations,
            tail_seq,
        },
    ))
}

pub(crate) async fn agent_complete_job(
    rqctx: RequestContext<ApiContext>,
    path: Path<AgentJobPath>,
    body: TypedBody<CompleteJobRequest>,
) -> Result<HttpResponseOk<ProvisioningJob>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentComplete,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let job_id = path.into_inner().job_id;
    let req = body.into_inner();
    // Per-CN binding: a bound key may only complete jobs it
    // itself claimed. We look up the job, check the binding,
    // and only then issue the terminal write.
    if let Some(bound) = crate::auth::principal_bound_cn(&principal) {
        let job = ctx
            .store
            .get_job(job_id)
            .await
            .map_err(store_error_to_http)?;
        enforce_job_belongs_to_bound_cn(&job, bound)?;
    }
    let outcome_label = match &req.outcome {
        JobOutcome::Completed => "completed",
        JobOutcome::Failed { .. } => "failed",
        _ => "unknown",
    };
    match ctx.store.complete_job(job_id, req.outcome.clone()).await {
        Ok(updated) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::AgentComplete,
                    request_id,
                    Some(format!("ProvisioningJob::\"{job_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("ProvisioningJob::\"{job_id}\"")),
                    },
                    serde_json::json!({
                        "job_id": job_id,
                        "outcome": outcome_label,
                    }),
                )
                .await;
            // Drive the instance lifecycle to its terminal
            // state for this job. Provisioning → Running on
            // success; Stopping → Stopped (or Running for
            // Restart); any → Failed{reason} on failure. The
            // job is already terminal regardless of whether
            // the lifecycle CAS succeeds, so a stale or
            // missing instance just gets logged.
            drive_lifecycle_for_complete(ctx.store.as_ref(), &updated, &req.outcome).await;
            Ok(HttpResponseOk(updated))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::AgentComplete,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({
                        "job_id": job_id,
                        "outcome": outcome_label,
                    }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn agent_heartbeat(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<()>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentHeartbeat,
    )
    .await?;
    // Heartbeat REQUIRES a bound key — there's no other way
    // to know which CN to attribute the ping to. Unbound
    // keys (legacy operator-minted) get 403.
    let server_uuid = require_bound_cn(&principal)?;
    ctx.store
        .update_cn_last_seen(server_uuid, chrono::Utc::now())
        .await
        .map_err(store_error_to_http)?;
    // Heartbeat is a hot path; we deliberately don't audit
    // every ping. The Cn record's `last_seen` is the
    // observable signal an operator cares about.
    Ok(HttpResponseOk(()))
}

pub(crate) async fn agent_status(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<AgentStatusRequest>,
) -> Result<HttpResponseOk<()>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentStatus,
    )
    .await?;
    let server_uuid = require_bound_cn(&principal)?;
    let req = body.into_inner();
    let now = chrono::Utc::now();
    let payload = req.payload;
    ctx.store
        .update_cn_status(server_uuid, payload.clone(), now)
        .await
        .map_err(store_error_to_http)?;
    // Status updates are also hot (~once per minute or
    // when zoneevent fires); no per-update audit. A future
    // slice may sample at low frequency for forensics.
    //
    // Classifier pass is best-effort: parse the report, run the
    // pure classifier, and fold per-VM outcomes (LegacyVm
    // upsert, Orphan/StaleFingerprint warnings) into the store.
    // Any failure is logged but does NOT fail the agent's
    // status post -- the heartbeater retries on its own cadence
    // and we'd rather drop one classifier pass than 503 an
    // operational heartbeat.
    if let Err(e) = run_classifier_pass(ctx, server_uuid, &payload, now).await {
        tracing::warn!(
            error = %e,
            server_uuid = %server_uuid,
            "classifier pass failed; status post still acked",
        );
    }
    // Best-effort: backfill `Instance.brand` for managed instances that
    // are still `NotApplicable`, using the live zone brand the agent
    // just reported. Logs internally; never fails the status post.
    backfill_instance_brands(ctx, &payload).await;
    Ok(HttpResponseOk(()))
}

/// `GET /v1/agent/config` — effective per-CN agent config for the bound
/// CN. Resolves the per-CN reservoir override against the cluster
/// defaults and returns flat values the agent applies directly.
pub(crate) async fn agent_get_config(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<AgentConfigResponse>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentConfig,
    )
    .await?;
    let server_uuid = require_bound_cn(&principal)?;

    let settings = ctx.store.get_settings().await.map_err(store_error_to_http)?;
    let placement = ctx
        .store
        .get_cn_placement(server_uuid)
        .await
        .map_err(store_error_to_http)?;
    let (reservoir_enabled, reservoir_percent) = placement.effective_reservoir(
        settings.reservoir_enabled_default,
        settings.reservoir_percent_default,
    );
    Ok(HttpResponseOk(AgentConfigResponse {
        reservoir_enabled,
        reservoir_percent,
    }))
}

pub(crate) async fn agent_report_network_realization(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<NetworkRealizationRequest>,
) -> Result<HttpResponseOk<()>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NetworkRealizationReport,
    )
    .await?;
    let bound_cn = require_bound_cn(&principal)?;
    let req = body.into_inner();
    enforce_realizer_belongs_to_bound_cn(req.realizer, bound_cn)?;
    ensure_realization_resource_exists(ctx.store.as_ref(), req.resource).await?;
    ctx.store
        .record_network_realization(
            req.resource,
            req.realizer,
            req.generation,
            req.status,
            req.message,
        )
        .await
        .map_err(store_error_to_http)?;
    // Realization reports are state-sample traffic, not an
    // operator mutation stream. The per-resource realization
    // rows are the durable signal; auditing every periodic
    // report would make the audit chain noisy.
    Ok(HttpResponseOk(()))
}

pub(crate) async fn agent_report_dhcp_lease_activity(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<DhcpLeaseActivityReport>,
) -> Result<HttpResponseOk<()>, HttpError> {
    let ctx = rqctx.context();
    // Authorisation only confirms the caller is an Agent-scoped,
    // CN-bound key. We deliberately do NOT verify the reported ports
    // belong to *this* CN: a stale or cross-CN report can at most
    // bump `last_renewed_at` on a lease whose instance still exists —
    // which the reconciler keeps regardless — so the extra
    // `list_recent_jobs` scan that check would cost (per item, per
    // poll) buys nothing.
    let _principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DhcpLeaseActivityReport,
    )
    .await?;
    let report = body.into_inner();
    let now = chrono::Utc::now();
    for item in &report.items {
        // port id == NIC id. A port that no longer resolves is a
        // stale event for a deleted NIC; drop it.
        let nic = match ctx.store.get_nic(item.port_id).await {
            Ok(n) => n,
            Err(tritond_store::StoreError::NotFound) => continue,
            Err(e) => return Err(store_error_to_http(e)),
        };
        if nic.mac != item.client_mac {
            tracing::warn!(
                port_id = %item.port_id,
                stored_mac = %nic.mac,
                reported_mac = %item.client_mac,
                "DHCP activity report MAC does not match stored NIC; skipping"
            );
            continue;
        }
        let lease = match ctx.store.get_dhcp_lease(nic.vpc_id, &nic.mac).await {
            Ok(l) => l,
            // No lease record yet — the pre-assignment hook writes one
            // at instance create, so this is only hit during the boot
            // race or for a NIC created before the IPAM landing.
            Err(tritond_store::StoreError::NotFound) => continue,
            Err(e) => return Err(store_error_to_http(e)),
        };
        let mut updated = lease.clone();
        updated.last_msg_type = Some(item.msg_type);
        updated.last_xid = Some(item.xid);
        // Persistent-lease policy: RELEASE (7) / DECLINE (4) are
        // recorded but never expire the lease; only DISCOVER (1) and
        // REQUEST (3) advance the renewal clock.
        if matches!(item.msg_type, 1 | 3) {
            updated.last_renewed_at = Some(now);
        }
        if updated != lease {
            ctx.store
                .record_dhcp_lease(updated)
                .await
                .map_err(store_error_to_http)?;
        }
    }
    // State-sample traffic, not an operator mutation — not audited,
    // for the same reason `agent_report_network_realization` isn't.
    Ok(HttpResponseOk(()))
}

pub(crate) async fn agent_register(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<RegisterCnRequest>,
) -> Result<HttpResponseOk<RegisterCnResponse>, HttpError> {
    let ctx = rqctx.context();
    // Cedar gate (anonymous → public-actions list).
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentRegister,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    let now = chrono::Utc::now();

    // Decode the agent's reported console-listener TLS SPKI
    // fingerprint up front so a malformed value 400s before we
    // touch the store. A port without a fingerprint (or a
    // fingerprint without a port) is also rejected: tritond pins
    // the SPKI when it dials the listener, so half a configuration
    // is no configuration.
    let console_tls_spki = match req.console_tls_spki_sha256_hex.as_deref() {
        Some(s) => {
            let mut buf = [0u8; 32];
            hex::decode_to_slice(s, &mut buf).map_err(|_| {
                bad_request("console_tls_spki_sha256_hex must be 64 lowercase hex chars (32 bytes)")
            })?;
            Some(buf)
        }
        None => None,
    };
    if console_tls_spki.is_some() != req.console_listen_port.is_some() {
        return Err(bad_request(
            "console_listen_port and console_tls_spki_sha256_hex must be supplied together",
        ));
    }

    let cn = ctx
        .store
        .register_cn(
            req.server_uuid,
            req.hostname.clone(),
            req.admin_ip,
            req.sysinfo.clone(),
            now,
        )
        .await
        .map_err(store_error_to_http)?;

    // Record the on-host console listener endpoint right after
    // register. The agent re-reports it on every (re-)registration,
    // so this is an idempotent update; a CN that has no console
    // listener clears the fields.
    ctx.store
        .set_cn_console_endpoint(req.server_uuid, req.console_listen_port, console_tls_spki)
        .await
        .map_err(store_error_to_http)?;

    // Auto-approve path: register_cn returned a fresh Approved
    // record without a bound key. Mint the key + wire it in so
    // the agent's first long-poll can retrieve it.
    let mut effective = cn.clone();
    if effective.state == CnState::Approved && effective.bound_api_key_id.is_none() {
        match mint_and_attach_cn_credential(ctx, &principal, request_id, &effective).await {
            Ok(updated) => effective = updated,
            Err(http) => return Err(http),
        }
    }

    // The CN's nic_tag inventory is published *separately*, on the
    // authenticated `POST /v1/agent/nic-tags` endpoint, once the agent
    // holds its bound credential. The inventory is a placement input
    // (floating-IP attach fail-closes on it), so it must be keyed by
    // the authenticated CN, never by `server_uuid` on this anonymous
    // path — an attacker could otherwise overwrite any Approved CN's
    // inventory and subvert the placement gate.

    ctx.audit
        .record_mutation(
            &principal,
            Action::AgentRegister,
            request_id,
            Some(format!("Cn::\"{}\"", effective.server_uuid)),
            AuditOutcome::Success {
                resource: Some(format!("Cn::\"{}\"", effective.server_uuid)),
            },
            serde_json::json!({
                "server_uuid": effective.server_uuid,
                "hostname": req.hostname,
                "admin_ip": req.admin_ip,
                "state": effective.state,
                "auto_approved": effective.state == CnState::Approved
                    && effective.approved_at == Some(now),
            }),
        )
        .await;

    Ok(HttpResponseOk(RegisterCnResponse {
        server_uuid: effective.server_uuid,
        state: effective.state,
        claim_code: effective
            .claim_code
            .as_deref()
            .map(tritond_store::format_claim_code),
        claim_code_expires_at: effective.claim_code_expires_at,
        poll_token: effective.poll_token,
    }))
}

/// Publish the calling bound CN's nic_tag inventory.
///
/// The inventory row is keyed by [`require_bound_cn`] — the CN identity
/// proven by the per-CN API key minted at approval — never by anything
/// in the request body. A caller authenticated as CN-A therefore can
/// only ever write CN-A's inventory; an unauthenticated (or unbound)
/// caller is rejected outright. This is the authenticated counterpart
/// to the (former) anonymous register-path publish: the inventory is a
/// floating-IP placement input, so the write must be authenticated to
/// the publishing CN.
pub(crate) async fn agent_report_nic_tags(
    rqctx: RequestContext<ApiContext>,
    body: TypedBody<tritond_api::NicTagInventoryReport>,
) -> Result<HttpResponseOk<()>, HttpError> {
    let ctx = rqctx.context();
    let principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NicTagInventoryReport,
    )
    .await?;
    // The row key is the credential's bound CN, not the request body:
    // a bound key can only ever publish its own CN's inventory.
    let cn = require_bound_cn(&principal)?;
    let report = body.into_inner();
    let now = chrono::Utc::now();
    resolve_and_publish_nic_tags(ctx.store.as_ref(), cn, &report.nic_tags, now)
        .await
        .map_err(store_error_to_http)?;
    // Reconcile hook (C-4b invariant 13): this is the authenticated,
    // CN-bound (re-)registration point, so re-drive a FipClaim for
    // every FIP this CN is supposed to host. An InProgress claim
    // stranded by a crashed agent becomes re-claimable; the re-claim is
    // idempotent at the kmod via the generation fence. Best-effort: a
    // reconcile failure must not fail the nic-tag publish (the next
    // re-register retries).
    if let Err(e) = reconcile_hosted_fips(ctx.store.as_ref(), cn).await {
        tracing::warn!(
            cn = %cn,
            error = %e,
            "fip reconcile on register failed (best-effort); will retry on next register",
        );
    }
    // State-sample traffic, not an operator mutation — not audited,
    // matching `agent_report_network_realization`.
    Ok(HttpResponseOk(()))
}

/// Enqueue a `FipClaim` for every FIP hosted on `cn` (C-4b invariant
/// 13). Split out from the handler so it is unit-testable against a
/// `MemStore`. Skips a hosted FIP that has lost its `attached_to`
/// binding (an orphaned hosted_cn the FipRelease cascade will clean up)
/// rather than enqueue a claim with no port to recompute.
async fn reconcile_hosted_fips(store: &dyn Store, cn: Uuid) -> Result<usize, StoreError> {
    let hosted = store.list_floating_ips_hosted_on_cn(cn).await?;
    let mut enqueued = 0usize;
    for fip in &hosted {
        let Some(attachment) = fip.attached_to.as_ref() else {
            // Hosted but unattached: nothing to recompute. The
            // delete/instance-delete cascade enqueues the FipRelease;
            // re-claiming here would have no port.
            continue;
        };
        let external_nic_tag = resolve_external_nic_tag_name(store, fip).await;
        let vlan_id = crate::sagas::floating_ip::resolve_external_subnet_vlan(store, fip).await;
        // Bump the port generation so the re-applied blueprint lands at
        // a strictly-greater generation and is not swallowed as a
        // same-generation no-op.
        let generation = store.bump_port_generation(attachment.nic_id).await?;
        store
            .enqueue_job(NewJob {
                kind: JobKind::FipClaim {
                    floating_ip_id: fip.id,
                    nic_id: attachment.nic_id,
                    instance_id: attachment.instance_id,
                    fip_addr: fip.address.to_string(),
                    external_nic_tag,
                    vlan_id,
                    generation,
                },
                target_cn_uuid: Some(cn),
            })
            .await?;
        enqueued += 1;
    }
    Ok(enqueued)
}

/// Resolve reported nic_tag names against the registry and publish the
/// CN's inventory. Split out from the handler so it is unit-testable
/// against a `MemStore` without a Dropshot request context.
async fn resolve_and_publish_nic_tags(
    store: &dyn Store,
    cn: Uuid,
    reported: &[RegisterNicTagProvision],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), StoreError> {
    if reported.is_empty() {
        return Ok(());
    }

    // Build a name -> id map once; the registry is small and fleet-wide.
    let registry = store.list_nic_tags().await?;
    let by_name: std::collections::HashMap<&str, Uuid> =
        registry.iter().map(|t| (t.name.as_str(), t.id)).collect();

    let provides: Vec<NicTagProvision> = reported
        .iter()
        .filter_map(|r| match by_name.get(r.name.as_str()) {
            Some(&nic_tag) => Some(NicTagProvision {
                nic_tag,
                physical_nic: r.physical_nic.clone(),
                vlan_id: r.vlan_id,
                mtu: r.mtu,
            }),
            None => {
                tracing::warn!(
                    cn = %cn,
                    nic_tag_name = %r.name,
                    "CN reported a nic_tag that resolves to no registered NicTag; \
                     skipping (fail-closed, not invented)",
                );
                None
            }
        })
        .collect();

    store
        .publish_cn_nic_tags(CnNicTagInventory {
            cn,
            provides,
            published_at: now,
        })
        .await
}

pub(crate) async fn agent_register_status(
    rqctx: RequestContext<ApiContext>,
    query: Query<RegisterStatusQuery>,
) -> Result<HttpResponseOk<RegisterStatusResponse>, HttpError> {
    let ctx = rqctx.context();
    // Cedar gate (anonymous → public-actions list).
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentRegisterStatus,
    )
    .await?;
    let q = query.into_inner();

    // Long-poll: spin until state flips, an Approved record has
    // a credential to retrieve, or we hit the deadline. The
    // 30s wall-clock cap matches typical operator-side approve
    // latency and keeps idle connections from accumulating.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let poll_interval = std::time::Duration::from_millis(500);

    loop {
        let cn = match ctx.store.get_cn_by_poll_token(&q.poll_token).await {
            Ok(c) => c,
            Err(StoreError::NotFound) => {
                return Err(HttpError::for_client_error(
                    Some("NotFound".to_string()),
                    ClientErrorStatusCode::NOT_FOUND,
                    "unknown poll token".to_string(),
                ));
            }
            Err(e) => return Err(store_error_to_http(e)),
        };

        if cn.state == CnState::Approved {
            let credential = ctx
                .store
                .consume_cn_pending_credential(&q.poll_token)
                .await
                .map_err(store_error_to_http)?;
            // The per-CN console-ticket key is delivered exactly
            // when the API key plaintext is — i.e. on the first
            // long-poll after approval. (Unlike `pending_credential`
            // it stays on the Cn record permanently; we just only
            // hand it to the agent in the same response.)
            let console_ticket_key_hex = credential
                .as_ref()
                .map(|_| hex::encode(cn.console_ticket_key.unwrap_or_default()));
            let imds_token_key_hex = credential
                .as_ref()
                .map(|_| hex::encode(cn.imds_token_key.unwrap_or_default()));
            let migrate_ticket_key_hex = credential
                .as_ref()
                .map(|_| hex::encode(cn.migrate_ticket_key.unwrap_or_default()));
            return Ok(HttpResponseOk(RegisterStatusResponse {
                state: cn.state,
                api_key: credential,
                console_ticket_key_hex,
                imds_token_key_hex,
                migrate_ticket_key_hex,
            }));
        }
        if cn.state == CnState::Disabled {
            return Ok(HttpResponseOk(RegisterStatusResponse {
                state: cn.state,
                api_key: None,
                console_ticket_key_hex: None,
                imds_token_key_hex: None,
                migrate_ticket_key_hex: None,
            }));
        }

        if std::time::Instant::now() >= deadline {
            return Ok(HttpResponseOk(RegisterStatusResponse {
                state: cn.state,
                api_key: None,
                console_ticket_key_hex: None,
                imds_token_key_hex: None,
                migrate_ticket_key_hex: None,
            }));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod nic_tag_publish_tests {
    use super::*;
    use tritond_store::{MemStore, NewNicTag};

    fn reported(name: &str, link: &str) -> RegisterNicTagProvision {
        RegisterNicTagProvision {
            name: name.to_string(),
            physical_nic: link.to_string(),
            vlan_id: 0,
            mtu: 1500,
        }
    }

    #[tokio::test]
    async fn resolves_known_names_and_publishes() {
        let store = MemStore::new();
        let external = store
            .create_nic_tag(NewNicTag {
                name: "external".to_string(),
                description: None,
                mtu: 1500,
            })
            .await
            .expect("create external nic_tag");
        let cn = Uuid::new_v4();
        let now = chrono::Utc::now();

        resolve_and_publish_nic_tags(
            &store,
            cn,
            &[reported("external", "igb2")],
            now,
        )
        .await
        .expect("publish");

        let inv = store
            .get_cn_nic_tags(cn)
            .await
            .expect("get inventory")
            .expect("inventory present");
        assert_eq!(inv.cn, cn);
        assert_eq!(inv.provides.len(), 1);
        assert_eq!(inv.provides[0].nic_tag, external.id);
        assert_eq!(inv.provides[0].physical_nic, "igb2");
    }

    #[tokio::test]
    async fn skips_unresolved_names_fail_closed() {
        let store = MemStore::new();
        store
            .create_nic_tag(NewNicTag {
                name: "external".to_string(),
                description: None,
                mtu: 1500,
            })
            .await
            .expect("create external nic_tag");
        let cn = Uuid::new_v4();
        let now = chrono::Utc::now();

        // "bogus" resolves to nothing and must be dropped, never invented.
        resolve_and_publish_nic_tags(
            &store,
            cn,
            &[reported("external", "igb2"), reported("bogus", "igb9")],
            now,
        )
        .await
        .expect("publish");

        let inv = store
            .get_cn_nic_tags(cn)
            .await
            .expect("get inventory")
            .expect("inventory present");
        assert_eq!(inv.provides.len(), 1);
        assert_eq!(inv.provides[0].physical_nic, "igb2");
    }

    #[tokio::test]
    async fn empty_report_is_noop_does_not_clobber() {
        let store = MemStore::new();
        let external = store
            .create_nic_tag(NewNicTag {
                name: "external".to_string(),
                description: None,
                mtu: 1500,
            })
            .await
            .expect("create external nic_tag");
        let cn = Uuid::new_v4();
        let now = chrono::Utc::now();

        // Seed an inventory, then a (older-agent) empty report must not
        // overwrite it.
        resolve_and_publish_nic_tags(&store, cn, &[reported("external", "igb2")], now)
            .await
            .expect("seed publish");
        resolve_and_publish_nic_tags(&store, cn, &[], now)
            .await
            .expect("empty publish");

        let inv = store
            .get_cn_nic_tags(cn)
            .await
            .expect("get inventory")
            .expect("inventory still present");
        assert_eq!(inv.provides.len(), 1);
        assert_eq!(inv.provides[0].nic_tag, external.id);
    }
}

#[cfg(test)]
mod reconcile_hosted_fips_tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tritond_store::{
        MemStore, NewExternalSubnet, NewFloatingIp, NewImage, NewInstance, NewNicTag, NewProject,
        NewSilo, NewSshKey, NewSubnet, NewVpc,
    };

    /// Build an instance with a NIC on a host CN plus a network-allocated
    /// FIP, attach the FIP (stamping `hosted_cn`), and return
    /// `(store, fip_id, nic_id, instance_id, host_cn)`.
    async fn hosted_fip_fixture() -> (MemStore, Uuid, Uuid, Uuid, Uuid) {
        let store = MemStore::new();
        let silo = store
            .create_silo(NewSilo {
                name: "s".into(),
                description: None,
            })
            .await
            .unwrap();
        let tenant_id = silo.default_tenant_id;
        let project = store
            .create_project(
                tenant_id,
                NewProject {
                    name: "p".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let project_id = project.id;
        let vpc = store
            .create_vpc(
                tenant_id,
                project_id,
                NewVpc {
                    name: "v".into(),
                    description: None,
                    ipv4_block: Some("10.0.0.0/16".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let subnet = store
            .create_subnet(
                tenant_id,
                project_id,
                vpc.id,
                NewSubnet {
                    name: "primary".into(),
                    description: None,
                    ipv4_block: Some("10.0.1.0/24".parse().unwrap()),
                    ipv6_block: None,
                },
            )
            .await
            .unwrap();
        let image = store
            .create_image_silo(
                silo.id,
                NewImage {
                    name: "img".into(),
                    description: None,
                    os: "linux".into(),
                    version: "1".into(),
                    size_bytes: 1_000_000,
                    sha256: "a".repeat(64),
                    source_url: Some("mantafs://i".into()),
                    id: None,
                    compatibility: None,
                },
            )
            .await
            .unwrap();
        let ssh = store
            .create_ssh_key_silo(
                silo.id,
                NewSshKey {
                    name: "k".into(),
                    description: None,
                    public_key: "ssh-ed25519 AAAA".into(),
                },
                "SHA256:fixture".into(),
            )
            .await
            .unwrap();
        let created = store
            .create_instance(
                tenant_id,
                project_id,
                NewInstance {
                    name: "web".into(),
                    description: None,
                    image_id: image.id,
                    primary_subnet_id: subnet.id,
                    ssh_key_ids: vec![ssh.id],
                    cpu: 1,
                    memory_bytes: 1024 * 1024 * 1024,
                    mac: None,
                    extra_nics: Vec::new(),
                },
            )
            .await
            .unwrap();
        let host_cn = Uuid::new_v4();
        store
            .set_instance_host_cn(created.instance.id, Some(host_cn))
            .await
            .unwrap();
        let tag = store
            .create_nic_tag(NewNicTag {
                name: "external".into(),
                description: None,
                mtu: 1500,
            })
            .await
            .unwrap();
        let ext = store
            .create_external_subnet(NewExternalSubnet {
                name: "pub".into(),
                description: None,
                ipv4_block: Some("192.0.2.0/24".parse().unwrap()),
                ipv6_block: None,
                nic_tag: tag.id,
                vlan_id: Some(100),
                provision_start_ipv4: Some(Ipv4Addr::new(192, 0, 2, 10)),
                provision_end_ipv4: Some(Ipv4Addr::new(192, 0, 2, 12)),
                provision_start_ipv6: None,
                provision_end_ipv6: None,
                owner_silos: Vec::new(),
            })
            .await
            .unwrap();
        let fip = store
            .create_floating_ip(
                tenant_id,
                project_id,
                NewFloatingIp {
                    name: "fip".into(),
                    description: None,
                    family: None,
                    network_id: Some(ext.id),
                    pool_id: None,
                },
            )
            .await
            .unwrap();
        let nic_id = created.nics[0].id;
        let attached = store.attach_floating_ip(fip.id, nic_id).await.unwrap();
        assert_eq!(attached.hosted_cn, Some(host_cn));
        (store, fip.id, nic_id, created.instance.id, host_cn)
    }

    #[tokio::test]
    async fn reconcile_enqueues_fip_claim_for_each_hosted_fip() {
        let (store, fip_id, nic_id, instance_id, host_cn) = hosted_fip_fixture().await;

        let enqueued = reconcile_hosted_fips(&store, host_cn).await.unwrap();
        assert_eq!(enqueued, 1, "one hosted FIP -> one FipClaim");

        // Exactly one FipClaim, pinned to the host CN, carrying the
        // attached instance + nic at a bumped generation.
        let jobs = store.list_recent_jobs(50).await.unwrap();
        let claims: Vec<_> = jobs
            .iter()
            .filter(|j| matches!(j.kind, JobKind::FipClaim { .. }))
            .collect();
        assert_eq!(claims.len(), 1);
        match &claims[0].kind {
            JobKind::FipClaim {
                floating_ip_id,
                nic_id: claimed_nic,
                instance_id: claimed_instance,
                generation,
                ..
            } => {
                assert_eq!(*floating_ip_id, fip_id);
                assert_eq!(*claimed_nic, nic_id);
                assert_eq!(*claimed_instance, instance_id);
                // bump_port_generation moved 1 -> 2.
                assert_eq!(*generation, 2);
            }
            other => panic!("expected FipClaim, got {other:?}"),
        }
        assert_eq!(claims[0].target_cn_uuid, Some(host_cn));
    }

    #[tokio::test]
    async fn reconcile_for_other_cn_enqueues_nothing() {
        let (store, _fip_id, _nic_id, _instance_id, _host_cn) = hosted_fip_fixture().await;
        let other_cn = Uuid::new_v4();
        let enqueued = reconcile_hosted_fips(&store, other_cn).await.unwrap();
        assert_eq!(enqueued, 0, "no FIP hosted on the other CN");
    }

    #[tokio::test]
    async fn reconcile_after_detach_enqueues_nothing() {
        // Detach clears hosted_cn, so a subsequently-registering CN has
        // no hosted FIP to re-claim. (Closes the "detached FIP enqueues
        // nothing" half of the reconcile contract via the public API,
        // without an internal test-only mutation hook.)
        let (store, fip_id, _nic_id, _instance_id, host_cn) = hosted_fip_fixture().await;
        store.detach_floating_ip(fip_id).await.unwrap();
        let enqueued = reconcile_hosted_fips(&store, host_cn).await.unwrap();
        assert_eq!(enqueued, 0, "detached FIP is no longer hosted; nothing to reconcile");
    }
}
