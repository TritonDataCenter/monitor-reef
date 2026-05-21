// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `instances` HTTP handlers (delegated to from the `TritondApi` impl).

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
    AUTO_APPROVE_WINDOW_MAX, ApiKey, CnState, ConfigError, ConfigKey, IdpConfig, MetaScope,
    MetaValue, Store, StoreError, normalize_claim_code,
};
use uuid::Uuid;

use crate::auth::{
    Action, AuthService, Principal, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant, require_authenticated,
};

use crate::VERSION;

use crate::context::ApiContext;
use crate::scope::{image_visible_to, ssh_key_visible_to};

/// Maximum allowed length for an `Idempotency-Key` header value.
/// Stripe uses 255 (the de-facto upper bound for an HTTP header
/// value clients are likely to honour); we mirror that. A longer
/// value is treated as absent rather than rejecting the request —
/// idempotency is best-effort metadata, not a precondition.
const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// Read the `Idempotency-Key` request header (case-insensitive),
/// trim whitespace, drop empty / oversize values. Returns `None`
/// when the header is absent or unusable so the saga records "no
/// key" rather than a malformed string.
fn idempotency_key_from_headers(rqctx: &RequestContext<ApiContext>) -> Option<String> {
    let raw = rqctx.request.headers().get("idempotency-key")?;
    let s = raw.to_str().ok()?.trim();
    if s.is_empty() || s.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return None;
    }
    Some(s.to_string())
}

pub(crate) async fn list_project_instances(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Vec<Instance>>, HttpError> {
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
        Action::InstanceList,
        tenant_id,
    )
    .await?;
    // Project must exist + be in this silo (matches the
    // list_project_vpcs / list_vpc_subnets pattern).
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let instances = ctx
        .store
        .list_instances_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(instances))
}

pub(crate) async fn create_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
    body: TypedBody<NewInstance>,
) -> Result<HttpResponseCreated<Instance>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // API-edge size invariants (the store doesn't re-validate).
    if req.cpu == 0 {
        return Err(reject_audit(
            ctx,
            &principal,
            Action::InstanceCreate,
            request_id,
            "cpu must be greater than zero",
            serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
        )
        .await);
    }
    if req.memory_bytes == 0 {
        return Err(reject_audit(
            ctx,
            &principal,
            Action::InstanceCreate,
            request_id,
            "memory_bytes must be greater than zero",
            serde_json::json!({ "tenant_id": tenant_id, "project_id": project_id }),
        )
        .await);
    }

    // Cross-scope visibility on the referenced image and SSH
    // keys. The store no longer enforces silo membership on
    // images / SSH keys (multi-scope as of slices F & G); the
    // handler resolves visibility against the principal and
    // surfaces a not-visible (or not-found) resource as 404 to
    // preserve the cross-tenant probe invariant.
    require_image_visible_for_instance(
        ctx,
        &principal,
        request_id,
        tenant_id,
        project_id,
        req.image_id,
    )
    .await?;
    require_ssh_keys_visible_for_instance(
        ctx,
        &principal,
        request_id,
        tenant_id,
        project_id,
        &req.ssh_key_ids,
    )
    .await?;

    let target_cn_uuid =
        match select_tenant_cn_for_instance(ctx.store.as_ref(), ctx.spawn_in_process_provisioner)
            .await
        {
            Ok(target) => target,
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(store_error_to_http(e));
            }
        };

    // From here on, instance allocation + host-CN pin + root_pw
    // meta + Provision-job enqueue run as a `tritond-saga`
    // `instance-create` saga. The saga has explicit per-action undo
    // so any failure unwinds cleanly (no leaked Instance / NIC / IP
    // / Disk / DhcpLease rows). See
    // `services/tritond/src/sagas/instance_create.rs` for the chain
    // and the unwind matrix; the RFD is 00004 SG-2.
    // RFD 00004 D-Sg-5 / SG-4: the `Idempotency-Key` request header
    // rides into the saga's Params so a future request whose key
    // matches an in-flight or completed saga can be projected back
    // to the original operation handle (store-side dedup is the
    // 202-conversion piece; for now the key is captured durably on
    // the saga record so operators can correlate retries).
    let saga_params = crate::sagas::instance_create::InstanceCreateParams {
        tenant_id,
        project_id,
        request: req,
        target_cn_uuid,
        idempotency_key: idempotency_key_from_headers(&rqctx),
        await_provision_terminal: ctx.saga_wait_for_agent,
    };
    let saga_dag = match crate::sagas::instance_create::build_dag(&saga_params) {
        Ok(d) => d,
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceCreate,
                    request_id,
                    None,
                    tritond_audit::Outcome::ServerError {
                        message: format!("saga dag build: {e}"),
                    },
                    serde_json::Value::Null,
                )
                .await;
            return Err(HttpError::for_internal_error(format!(
                "instance-create saga dag build failed: {e}"
            )));
        }
    };
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    // Resource refs known at create time. The by_ref index makes
    // these sagas discoverable from per-resource detail pages
    // (RFD 00004 SG-4 resource indexing).
    let saga_refs = crate::sagas::instance_create::build_references(&saga_params);
    let saga_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::instance_create::SAGA_NAME,
            crate::sagas::instance_create::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await;
    let steno_result = match saga_result {
        Ok(r) => r,
        Err(e) => {
            // Engine error before the saga even started running
            // (registry mismatch, persistence layer failure, etc.).
            // This shouldn't happen on the well-known catalog at
            // SG-2; surface as 500 with the error string.
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceCreate,
                    request_id,
                    None,
                    tritond_audit::Outcome::ServerError {
                        message: format!("saga executor error: {e}"),
                    },
                    serde_json::Value::Null,
                )
                .await;
            return Err(HttpError::for_internal_error(format!(
                "instance-create saga executor error: {e}"
            )));
        }
    };
    let instance: Instance = match steno_result.kind {
        Ok(ok) => match ok.lookup_node_output::<Instance>("final_instance") {
            Ok(i) => i,
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        tritond_audit::Outcome::ServerError {
                            message: format!("saga output lookup: {e}"),
                        },
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(HttpError::for_internal_error(format!(
                    "instance-create saga finished but final_instance output missing: {e}"
                )));
            }
        },
        Err(err) => {
            // Saga unwound. The unwind ran (or attempted to run)
            // each committed action's undo in reverse; zero rows
            // leak on the happy unwind path. Pull the StoreError
            // variant out of the failing action's `action_failed`
            // payload so duplicate-name / not-found etc preserve
            // their original 4xx status instead of collapsing to
            // 500 (SG-2 keeps the pre-saga handler's HTTP
            // semantics). Operators see the full step list via
            // `tcadm sagas get` (SG-4).
            let store_kind_msg: Option<(&'static str, String)> = match &err.error_source {
                tritond_saga::ActionError::ActionFailed { source_error } => {
                    let kind = crate::sagas::instance_create::decode_store_error_kind(source_error);
                    let msg = source_error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    kind.map(|k| (k, msg))
                }
                _ => None,
            };
            let full_msg = format!(
                "instance-create saga unwound at node `{:?}`: {:?}",
                err.error_node_name, err.error_source
            );
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceCreate,
                    request_id,
                    None,
                    tritond_audit::Outcome::ServerError {
                        message: full_msg.clone(),
                    },
                    serde_json::Value::Null,
                )
                .await;
            return Err(match store_kind_msg {
                Some(("conflict", msg)) => HttpError::for_client_error(
                    Some("Conflict".to_string()),
                    ClientErrorStatusCode::CONFLICT,
                    msg,
                ),
                Some(("not_found", _msg)) => not_found(),
                _ => HttpError::for_internal_error(full_msg),
            });
        }
    };

    ctx.audit
        .record_mutation(
            &principal,
            Action::InstanceCreate,
            request_id,
            Some(format!("Instance::\"{}\"", instance.id)),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{}\"", instance.id)),
            },
            serde_json::json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "name": instance.name,
                "image_id": instance.image_id,
                "primary_subnet_id": instance.primary_subnet_id,
                "saga_id": saga_id.0,
            }),
        )
        .await;
    Ok(HttpResponseCreated(instance))
}

pub(crate) async fn get_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
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
    Ok(HttpResponseOk(instance))
}

pub(crate) async fn delete_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    query: Query<InstanceDeleteQuery>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let _force = query.into_inner().force;
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    let target_cn_uuid = instance.host_cn_uuid;

    // v2p invalidation push (PROTEUS_PLAN §11.7.1 item 8). Lives
    // here (not in the saga action body) because the global
    // `peer_invalidations` ring isn't reachable from a
    // SagaContext yet. Done before the saga so the
    // release_record action can drop the NIC rows safely.
    if let Ok(nics) = ctx.store.list_nics_for_instance(instance_id).await {
        for nic in nics {
            let Ok(vpc) = ctx.store.get_vpc(nic.vpc_id).await else {
                continue;
            };
            if let Some(v4) = nic.primary_ipv4 {
                ctx.peer_invalidations.push(vpc.vni, v4.to_string());
            }
            if let Some(v6) = nic.primary_ipv6 {
                ctx.peer_invalidations.push(vpc.vni, v6.to_string());
            }
        }
    }

    // RFD 00004 SG-3: instance-delete saga. Detaches FIPs,
    // enqueues a Delete job, awaits the agent's terminal status,
    // then releases the record. Unwinds via the detach undo if
    // anything fails before the record is released; lands Stuck
    // if release_record fails after the agent acked Delete.
    let saga_params = crate::sagas::instance_delete::InstanceDeleteParams {
        tenant_id,
        project_id,
        instance_id,
        target_cn_uuid,
        await_delete_terminal: ctx.saga_wait_for_agent,
    };
    let saga_dag = match crate::sagas::instance_delete::build_dag(&saga_params) {
        Ok(d) => d,
        Err(e) => {
            return Err(HttpError::for_internal_error(format!(
                "instance-delete saga dag build: {e}"
            )));
        }
    };
    let saga_refs = crate::sagas::instance_delete::build_references(&saga_params, target_cn_uuid);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let saga_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::instance_delete::SAGA_NAME,
            crate::sagas::instance_delete::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await;
    let steno_result = match saga_result {
        Ok(r) => r,
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceDelete,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    tritond_audit::Outcome::ServerError {
                        message: format!("saga executor error: {e}"),
                    },
                    serde_json::Value::Null,
                )
                .await;
            return Err(HttpError::for_internal_error(format!(
                "instance-delete saga executor error: {e}"
            )));
        }
    };
    match steno_result.kind {
        Ok(_) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceDelete,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Instance::\"{instance_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseDeleted())
        }
        Err(err) => {
            let kind_msg: Option<(&'static str, String)> = match &err.error_source {
                tritond_saga::ActionError::ActionFailed { source_error } => {
                    let kind = crate::sagas::instance_delete::decode_store_error_kind(source_error);
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
                    ClientErrorStatusCode::CONFLICT,
                    kind_msg
                        .as_ref()
                        .map(|(_, m)| m.clone())
                        .unwrap_or_default(),
                ),
                _ => HttpError::for_internal_error(format!(
                    "instance-delete saga failed at {:?}: {:?}",
                    err.error_node_name, err.error_source
                )),
            };
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceDelete,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
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
    }
}

pub(crate) async fn start_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    lifecycle_saga_entry(
        rqctx,
        path,
        Action::InstanceStart,
        crate::sagas::instance_lifecycle::LifecycleOp::Start,
    )
    .await
}

pub(crate) async fn stop_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    lifecycle_saga_entry(
        rqctx,
        path,
        Action::InstanceStop,
        crate::sagas::instance_lifecycle::LifecycleOp::Stop,
    )
    .await
}

pub(crate) async fn restart_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    lifecycle_saga_entry(
        rqctx,
        path,
        Action::InstanceRestart,
        crate::sagas::instance_lifecycle::LifecycleOp::Restart,
    )
    .await
}

/// Common saga-dispatch entrypoint shared by start / stop /
/// restart. Resolves the instance, captures its current state for
/// the undo, builds the saga, runs it, and maps the result back to
/// an HTTP response. The action chain itself lives in
/// `sagas::instance_lifecycle`.
async fn lifecycle_saga_entry(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    action: Action,
    op: crate::sagas::instance_lifecycle::LifecycleOp,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    let target_cn_uuid = instance.host_cn_uuid;
    let original_kind = Some(
        crate::sagas::instance_lifecycle::OriginalLifecycle::from_kind(instance.lifecycle.kind()),
    );

    let saga_params = crate::sagas::instance_lifecycle::InstanceLifecycleParams {
        op,
        tenant_id,
        project_id,
        instance_id,
        target_cn_uuid,
        await_job_terminal: ctx.saga_wait_for_agent,
        original_state_kind: original_kind,
    };
    let saga_dag = crate::sagas::instance_lifecycle::build_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("{} saga dag build: {e}", op.saga_name()))
    })?;
    let saga_refs = crate::sagas::instance_lifecycle::build_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            op.saga_name(),
            crate::sagas::instance_lifecycle::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("{} saga executor error: {e}", op.saga_name()))
        })?;

    match steno_result.kind {
        Ok(ok) => {
            let final_instance: Instance = ok.lookup_node_output("final").map_err(|e| {
                HttpError::for_internal_error(format!(
                    "{} saga finished but output missing: {e}",
                    op.saga_name()
                ))
            })?;
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("Instance::\"{instance_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseOk(final_instance))
        }
        Err(err) => {
            let kind_msg: Option<(&'static str, String)> = match &err.error_source {
                tritond_saga::ActionError::ActionFailed { source_error } => {
                    let kind =
                        crate::sagas::instance_lifecycle::decode_store_error_kind(source_error);
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
                    ClientErrorStatusCode::CONFLICT,
                    kind_msg
                        .as_ref()
                        .map(|(_, m)| m.clone())
                        .unwrap_or_default(),
                ),
                _ => HttpError::for_internal_error(format!(
                    "{} saga failed at {:?}: {:?}",
                    op.saga_name(),
                    err.error_node_name,
                    err.error_source
                )),
            };
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
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
    }
}

// ──────────────────────────────────────────────────────────────────
// LM-5 — Live migration: POST .../instances/{id}/migrate
//
// Creates a MigrationRecord (atomic active-key guard against
// concurrent migrations of the same VM), kicks off the
// `migrate-instance` saga, and returns 202 with both ids. The
// other MigrationActions (estimate / pause / abort / rollback /
// finalize / sync) return 501 until LM-6 / LM-8 wire the sub-
// sagas.
// ──────────────────────────────────────────────────────────────────

pub(crate) async fn migrate_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
    body: dropshot::TypedBody<tritond_api::MigrateInstanceBody>,
) -> Result<HttpResponseCreated<tritond_api::MigrateInstanceResponse>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceMigrate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let body = body.into_inner();

    // LM-5 ships Begin only; the other actions surface 400 with
    // a pointer to the slice that wires them. (Dropshot's
    // `ClientErrorStatusCode` doesn't include 501; modelling
    // "deferred action" as a structured 400 is fine until the
    // sub-sagas land and these become real verbs.)
    use tritond_store::MigrationAction;
    match body.action {
        MigrationAction::Begin => {}
        MigrationAction::Estimate => {
            return Err(HttpError::for_client_error(
                Some("NotImplemented".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "migrate: action=estimate lands in LM-6 (designate-only pre-flight)".to_string(),
            ));
        }
        MigrationAction::Pause
        | MigrationAction::Abort
        | MigrationAction::Rollback
        | MigrationAction::Finalize
        | MigrationAction::Sync
        | MigrationAction::Switch => {
            return Err(HttpError::for_client_error(
                Some("NotImplemented".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                format!(
                    "migrate: action={:?} lands in LM-8 (pause / abort / rollback sub-sagas)",
                    body.action,
                ),
            ));
        }
        _ => {
            return Err(HttpError::for_client_error(
                Some("BadRequest".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                "migrate: unknown action".to_string(),
            ));
        }
    }

    // Verify the instance exists + belongs to this tenant/project.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    let source_cn = instance.host_cn_uuid.ok_or_else(|| {
        HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            "instance has no host_cn_uuid (was it ever provisioned?)".to_string(),
        )
    })?;

    // Atomic create_migration: takes the
    // migration/active/<instance> guard so a second concurrent
    // migrate against the same VM is rejected as 409.
    let record = ctx
        .store
        .create_migration(tritond_store::NewMigration {
            instance_id,
            tenant_id,
            project_id,
            source_cn,
            action_requested: MigrationAction::Begin,
            automatic: false,
        })
        .await
        .map_err(store_error_to_http)?;

    let saga_params = crate::sagas::migration::MigrationSagaParams {
        migration_id: record.id,
        instance_id,
        tenant_id,
        project_id,
        source_cn,
        target_cn_hint: body.target_server_uuid,
        automatic: false,
        cold: body.cold,
    };
    let saga_dag = crate::sagas::migration::build_dag(&saga_params)
        .map_err(|e| HttpError::for_internal_error(format!("migrate saga dag build: {e}")))?;
    let saga_refs = crate::sagas::migration::build_references(&saga_params);
    let saga_id = tritond_saga::SagaId(Uuid::new_v4());

    // saga_execute spawns the saga; we don't block on its
    // terminal state for migration (it can take minutes) — the
    // operator polls /v2/operations/{id} for saga state and
    // /v2/migrations/{id}/progress for the per-phase event log.
    let _ = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::migration::SAGA_NAME,
            crate::sagas::migration::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| HttpError::for_internal_error(format!("migrate saga execute: {e}")))?;

    ctx.audit
        .record_mutation(
            &principal,
            Action::InstanceMigrate,
            request_id,
            Some(format!("Instance::\"{instance_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Instance::\"{instance_id}\"")),
            },
            serde_json::json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "migration_id": record.id.to_string(),
                "operation_id": saga_id.0.to_string(),
                "source_cn": source_cn.to_string(),
                "target_cn_hint": body.target_server_uuid.map(|u| u.to_string()),
            }),
        )
        .await;

    Ok(HttpResponseCreated(tritond_api::MigrateInstanceResponse {
        migration_id: record.id,
        operation_id: saga_id.0,
    }))
}

pub(crate) async fn list_instance_nics(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Vec<Nic>>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NicList,
        tenant_id,
    )
    .await?;
    // Defence-in-depth: instance must live in path's silo+project.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    let nics = ctx
        .store
        .list_nics_for_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(nics))
}

pub(crate) async fn get_instance_nic(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstanceNicPath>,
) -> Result<HttpResponseOk<Nic>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstanceNicPath {
        tenant_id,
        project_id,
        instance_id,
        nic_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NicGet,
        tenant_id,
    )
    .await?;
    let nic = ctx
        .store
        .get_nic(nic_id)
        .await
        .map_err(store_error_to_http)?;
    // Defence-in-depth: NIC must live under all three path levels.
    if nic.tenant_id != tenant_id || nic.project_id != project_id || nic.instance_id != instance_id
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(nic))
}

pub(crate) async fn list_instance_disks(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Vec<Disk>>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DiskList,
        tenant_id,
    )
    .await?;
    // Defence-in-depth: instance must live in path silo+project.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if instance.tenant_id != tenant_id || instance.project_id != project_id {
        return Err(not_found());
    }
    let disks = ctx
        .store
        .list_disks_for_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(disks))
}

pub(crate) async fn get_instance_disk(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstanceDiskPath>,
) -> Result<HttpResponseOk<Disk>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectInstanceDiskPath {
        tenant_id,
        project_id,
        instance_id,
        disk_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DiskGet,
        tenant_id,
    )
    .await?;
    let disk = ctx
        .store
        .get_disk(disk_id)
        .await
        .map_err(store_error_to_http)?;
    // Defence-in-depth on all three parent ids.
    if disk.tenant_id != tenant_id
        || disk.project_id != project_id
        || disk.instance_id != instance_id
    {
        return Err(not_found());
    }
    Ok(HttpResponseOk(disk))
}

pub(crate) async fn list_project_floating_ips(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
) -> Result<HttpResponseOk<Vec<FloatingIp>>, HttpError> {
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
        Action::FloatingIpList,
        tenant_id,
    )
    .await?;
    // Defence-in-depth: project must live in path's silo.
    let project = ctx
        .store
        .get_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    if project.tenant_id != tenant_id {
        return Err(not_found());
    }
    let fips = ctx
        .store
        .list_floating_ips_in_project(project_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(fips))
}

pub(crate) async fn create_project_floating_ip(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectPath>,
    body: TypedBody<NewFloatingIp>,
) -> Result<HttpResponseCreated<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectPath {
        tenant_id,
        project_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let saga_params = crate::sagas::floating_ip::FloatingIpAllocateParams {
        tenant_id,
        project_id,
        request: req,
    };
    let saga_dag = crate::sagas::floating_ip::build_allocate_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("floating-ip-allocate saga dag build: {e}"))
    })?;
    let saga_refs = crate::sagas::floating_ip::build_allocate_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::floating_ip::SAGA_NAME_ALLOCATE,
            crate::sagas::floating_ip::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("floating-ip-allocate saga executor: {e}"))
        })?;
    match steno_result.kind {
        Ok(ok) => {
            let fip: FloatingIp = ok.lookup_node_output("fip").map_err(|e| {
                HttpError::for_internal_error(format!(
                    "floating-ip-allocate saga finished but output missing: {e}"
                ))
            })?;
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpCreate,
                    request_id,
                    Some(format!("FloatingIp::\"{}\"", fip.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("FloatingIp::\"{}\"", fip.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "name": fip.name,
                        "address": fip.address.to_string(),
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseCreated(fip))
        }
        Err(err) => {
            map_fip_saga_err(
                &ctx.audit,
                &principal,
                Action::FloatingIpCreate,
                request_id,
                None,
                saga_id,
                &err,
            )
            .await
        }
    }
}

/// Shared error-mapping for FIP saga failures. Re-derives the
/// matching HTTP status from the action payload (using
/// `decode_store_error_kind`) and emits the audit row.
async fn map_fip_saga_err<T>(
    audit: &crate::audit::AuditService,
    principal: &Principal,
    action: Action,
    request_id: Option<Uuid>,
    resource: Option<String>,
    saga_id: tritond_saga::SagaId,
    err: &tritond_saga::SagaResultErr,
) -> Result<T, HttpError> {
    let kind_msg: Option<(&'static str, String)> = match &err.error_source {
        tritond_saga::ActionError::ActionFailed { source_error } => {
            let kind = crate::sagas::floating_ip::decode_store_error_kind(source_error);
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
            ClientErrorStatusCode::CONFLICT,
            kind_msg
                .as_ref()
                .map(|(_, m)| m.clone())
                .unwrap_or_default(),
        ),
        _ => HttpError::for_internal_error(format!(
            "fip saga failed at {:?}: {:?}",
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

pub(crate) async fn get_project_floating_ip(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectFloatingIpPath>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectFloatingIpPath {
        tenant_id,
        project_id,
        floating_ip_id,
    } = path.into_inner();
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpGet,
        tenant_id,
    )
    .await?;
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    if fip.tenant_id != tenant_id || fip.project_id != project_id {
        return Err(not_found());
    }
    Ok(HttpResponseOk(fip))
}

pub(crate) async fn delete_project_floating_ip(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectFloatingIpPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectFloatingIpPath {
        tenant_id,
        project_id,
        floating_ip_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpDelete,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Defence-in-depth: confirm the FloatingIp lives under
    // path's silo+project before invoking delete.
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    if fip.tenant_id != tenant_id || fip.project_id != project_id {
        return Err(not_found());
    }
    match ctx.store.delete_floating_ip(floating_ip_id).await {
        Ok(()) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpDelete,
                    request_id,
                    Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
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
                    Action::FloatingIpDelete,
                    request_id,
                    Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn attach_project_floating_ip(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectFloatingIpPath>,
    body: TypedBody<AttachFloatingIpRequest>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectFloatingIpPath {
        tenant_id,
        project_id,
        floating_ip_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpAttach,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    if fip.tenant_id != tenant_id || fip.project_id != project_id {
        return Err(not_found());
    }
    // Capture prior binding so the undo can restore it.
    let prior_nic_id = fip.attached_to.as_ref().map(|a| a.nic_id);
    let target_instance_id = match ctx.store.get_nic(req.nic_id).await {
        Ok(n) => Some(n.instance_id),
        Err(_) => None,
    };
    let saga_params = crate::sagas::floating_ip::FloatingIpAttachParams {
        tenant_id,
        project_id,
        fip_id: floating_ip_id,
        target_nic_id: req.nic_id,
        prior_nic_id,
        target_instance_id,
    };
    let saga_dag = crate::sagas::floating_ip::build_attach_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("floating-ip-attach saga dag build: {e}"))
    })?;
    let saga_refs = crate::sagas::floating_ip::build_attach_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::floating_ip::SAGA_NAME_ATTACH,
            crate::sagas::floating_ip::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("floating-ip-attach saga executor: {e}"))
        })?;
    match steno_result.kind {
        Ok(ok) => {
            let updated: FloatingIp = ok.lookup_node_output("attached").map_err(|e| {
                HttpError::for_internal_error(format!(
                    "floating-ip-attach saga finished but output missing: {e}"
                ))
            })?;
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpAttach,
                    request_id,
                    Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "nic_id": req.nic_id,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(err) => {
            map_fip_saga_err(
                &ctx.audit,
                &principal,
                Action::FloatingIpAttach,
                request_id,
                Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                saga_id,
                &err,
            )
            .await
        }
    }
}

pub(crate) async fn detach_project_floating_ip(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectFloatingIpPath>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let TenantProjectFloatingIpPath {
        tenant_id,
        project_id,
        floating_ip_id,
    } = path.into_inner();
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpDetach,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    if fip.tenant_id != tenant_id || fip.project_id != project_id {
        return Err(not_found());
    }
    let prior_nic_id = fip.attached_to.as_ref().map(|a| a.nic_id);
    let prior_instance_id = fip.attached_to.as_ref().map(|a| a.instance_id);
    let saga_params = crate::sagas::floating_ip::FloatingIpDetachParams {
        tenant_id,
        project_id,
        fip_id: floating_ip_id,
        prior_nic_id,
        prior_instance_id,
    };
    let saga_dag = crate::sagas::floating_ip::build_detach_dag(&saga_params).map_err(|e| {
        HttpError::for_internal_error(format!("floating-ip-detach saga dag build: {e}"))
    })?;
    let saga_refs = crate::sagas::floating_ip::build_detach_references(&saga_params);
    let saga_id = tritond_saga::SagaId(uuid::Uuid::new_v4());
    let steno_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::floating_ip::SAGA_NAME_DETACH,
            crate::sagas::floating_ip::SAGA_VERSION,
            saga_dag,
            &saga_refs,
        )
        .await
        .map_err(|e| {
            HttpError::for_internal_error(format!("floating-ip-detach saga executor: {e}"))
        })?;
    match steno_result.kind {
        Ok(ok) => {
            let updated: FloatingIp = ok.lookup_node_output("detached").map_err(|e| {
                HttpError::for_internal_error(format!(
                    "floating-ip-detach saga finished but output missing: {e}"
                ))
            })?;
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpDetach,
                    request_id,
                    Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    AuditOutcome::Success {
                        resource: Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "operation_id": saga_id.0.to_string(),
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(err) => {
            map_fip_saga_err(
                &ctx.audit,
                &principal,
                Action::FloatingIpDetach,
                request_id,
                Some(format!("FloatingIp::\"{floating_ip_id}\"")),
                saga_id,
                &err,
            )
            .await
        }
    }
}

/// Resolve cross-scope visibility for the image an instance-create
/// references. Surfaces a not-found or not-visible image as 404 (and
/// audits the deny) to preserve the cross-tenant probe invariant.
async fn require_image_visible_for_instance(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    tenant_id: Uuid,
    project_id: Uuid,
    image_id: Uuid,
) -> Result<(), HttpError> {
    let message = match ctx.store.get_image(image_id).await {
        Ok(image) => {
            if image_visible_to(&image, principal, ctx.store.as_ref())
                .await
                .map_err(store_error_to_http)?
            {
                return Ok(());
            }
            "image not visible"
        }
        Err(StoreError::NotFound) => "image not found",
        Err(e) => return Err(store_error_to_http(e)),
    };
    ctx.audit
        .record_mutation(
            principal,
            Action::InstanceCreate,
            request_id,
            None,
            AuditOutcome::ClientError {
                code: 404,
                message: message.to_string(),
            },
            serde_json::json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "image_id": image_id,
            }),
        )
        .await;
    Err(not_found())
}

/// Resolve cross-scope visibility for every SSH key an
/// instance-create references. Surfaces the first not-found or
/// not-visible key as 404 (and audits the deny) to preserve the
/// cross-tenant probe invariant.
async fn require_ssh_keys_visible_for_instance(
    ctx: &ApiContext,
    principal: &Principal,
    request_id: Option<Uuid>,
    tenant_id: Uuid,
    project_id: Uuid,
    key_ids: &[Uuid],
) -> Result<(), HttpError> {
    for key_id in key_ids {
        let message = match ctx.store.get_ssh_key(*key_id).await {
            Ok(key) => {
                if ssh_key_visible_to(&key, principal, ctx.store.as_ref())
                    .await
                    .map_err(store_error_to_http)?
                {
                    continue;
                }
                "ssh key not visible"
            }
            Err(StoreError::NotFound) => "ssh key not found",
            Err(e) => return Err(store_error_to_http(e)),
        };
        ctx.audit
            .record_mutation(
                principal,
                Action::InstanceCreate,
                request_id,
                None,
                AuditOutcome::ClientError {
                    code: 404,
                    message: message.to_string(),
                },
                serde_json::json!({
                    "tenant_id": tenant_id,
                    "project_id": project_id,
                    "ssh_key_id": *key_id,
                }),
            )
            .await;
        return Err(not_found());
    }
    Ok(())
}
