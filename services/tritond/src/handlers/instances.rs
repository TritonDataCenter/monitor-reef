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
    let saga_params = crate::sagas::instance_create::InstanceCreateParams {
        tenant_id,
        project_id,
        request: req,
        target_cn_uuid,
        // SG-4 will plumb the Idempotency-Key header here.
        idempotency_key: None,
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
    let saga_result = ctx
        .saga
        .saga_execute(
            saga_id,
            crate::sagas::instance_create::SAGA_NAME,
            crate::sagas::instance_create::SAGA_VERSION,
            saga_dag,
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
    let force = query.into_inner().force;
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

    // v2p invalidation push (PROTEUS_PLAN §11.7.1 item 8): before
    // delete_instance drops the NIC records, snapshot each NIC's
    // (vni, primary_ip) and push an invalidation onto the global
    // ring so every CN's next peer-invalidations poll picks them
    // up. NIC churn is low-frequency; the broadcast cost is bounded.
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

    match ctx.store.delete_instance(instance_id, force).await {
        Ok(()) => {
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
                    }),
                )
                .await;
            // Best-effort agent cleanup of the SmartOS zone.
            // Failure here is logged but doesn't fail the
            // operator-visible delete — the tritond record
            // is already gone.
            if let Err(e) = ctx
                .store
                .enqueue_job(NewJob {
                    kind: JobKind::Delete { instance_id },
                    target_cn_uuid,
                })
                .await
            {
                tracing::warn!(
                    %instance_id,
                    error = %e,
                    "instance delete record cleared, but enqueue of Delete job failed; zone may leak on the host",
                );
            }
            Ok(HttpResponseDeleted())
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceDelete,
                    request_id,
                    Some(format!("Instance::\"{instance_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::Value::Null,
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn start_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    // Stopped → Pending; agent then drives Pending → Provisioning
    // → Running. The response shows Pending; clients poll for
    // the final state.
    instance_lifecycle_transition(
        rqctx,
        path,
        Action::InstanceStart,
        &[LifecycleStateKind::Stopped],
        LifecycleState::Pending,
        Some(JobKindTemplate::Provision),
    )
    .await
}

pub(crate) async fn stop_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    // Running → Stopping; agent then drives Stopping → Stopped.
    instance_lifecycle_transition(
        rqctx,
        path,
        Action::InstanceStop,
        &[LifecycleStateKind::Running],
        LifecycleState::Stopping,
        Some(JobKindTemplate::Stop),
    )
    .await
}

pub(crate) async fn restart_project_instance(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantProjectInstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    // Running → Stopping; agent then drives the full restart
    // cycle Stopping → Pending → Provisioning → Running.
    instance_lifecycle_transition(
        rqctx,
        path,
        Action::InstanceRestart,
        &[LifecycleStateKind::Running],
        LifecycleState::Stopping,
        Some(JobKindTemplate::Restart),
    )
    .await
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

    match ctx
        .store
        .create_floating_ip(tenant_id, project_id, req)
        .await
    {
        Ok(fip) => {
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
                    }),
                )
                .await;
            Ok(HttpResponseCreated(fip))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpCreate,
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

    // Defence-in-depth on the FloatingIp itself.
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    if fip.tenant_id != tenant_id || fip.project_id != project_id {
        return Err(not_found());
    }
    match ctx
        .store
        .attach_floating_ip(floating_ip_id, req.nic_id)
        .await
    {
        Ok(updated) => {
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
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpAttach,
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
    match ctx.store.detach_floating_ip(floating_ip_id).await {
        Ok(updated) => {
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
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::FloatingIpDetach,
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
