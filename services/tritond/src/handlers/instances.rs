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
use crate::service_impl::{image_visible_to, ssh_key_visible_to};

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

    // Cross-scope visibility check on the referenced image.
    // The store no longer enforces silo membership on images
    // (multi-scope as of slice F); the handler resolves
    // visibility against the principal and surfaces a
    // not-visible image as 404 to preserve the cross-tenant
    // probe invariant.
    match ctx.store.get_image(req.image_id).await {
        Ok(image) => {
            let visible = image_visible_to(&image, &principal, ctx.store.as_ref())
                .await
                .map_err(store_error_to_http)?;
            if !visible {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 404,
                            message: "image not visible".to_string(),
                        },
                        serde_json::json!({
                            "tenant_id": tenant_id,
                            "project_id": project_id,
                            "image_id": req.image_id,
                        }),
                    )
                    .await;
                return Err(not_found());
            }
        }
        Err(StoreError::NotFound) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::InstanceCreate,
                    request_id,
                    None,
                    AuditOutcome::ClientError {
                        code: 404,
                        message: "image not found".to_string(),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "project_id": project_id,
                        "image_id": req.image_id,
                    }),
                )
                .await;
            return Err(not_found());
        }
        Err(e) => return Err(store_error_to_http(e)),
    }

    // Cross-scope visibility check on every referenced SSH
    // key. The store no longer enforces silo membership on
    // SSH keys (multi-scope as of slice G); the handler
    // resolves visibility against the principal and surfaces
    // a not-visible (or not-found) key as 404 to preserve
    // the cross-tenant probe invariant.
    for key_id in &req.ssh_key_ids {
        match ctx.store.get_ssh_key(*key_id).await {
            Ok(key) => {
                let visible = ssh_key_visible_to(&key, &principal, ctx.store.as_ref())
                    .await
                    .map_err(store_error_to_http)?;
                if !visible {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::InstanceCreate,
                            request_id,
                            None,
                            AuditOutcome::ClientError {
                                code: 404,
                                message: "ssh key not visible".to_string(),
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
            }
            Err(StoreError::NotFound) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        None,
                        AuditOutcome::ClientError {
                            code: 404,
                            message: "ssh key not found".to_string(),
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
            Err(e) => return Err(store_error_to_http(e)),
        }
    }

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

    let mut instance = match ctx.store.create_instance(tenant_id, project_id, req).await {
        Ok(result) => result.instance,
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

    if let Some(host_cn_uuid) = target_cn_uuid {
        instance = match ctx
            .store
            .set_instance_host_cn(instance.id, Some(host_cn_uuid))
            .await
        {
            Ok(updated) => updated,
            Err(e) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::InstanceCreate,
                        request_id,
                        Some(format!("Instance::\"{}\"", instance.id)),
                        store_error_to_audit_outcome(&e),
                        serde_json::Value::Null,
                    )
                    .await;
                return Err(store_error_to_http(e));
            }
        };
    }

    // Enqueue the provisioning job. The stub provisioner (or
    // the selected per-CN agent) will pick it up and drive
    // Pending → Provisioning → Running. The response returns
    // the instance in `Pending` — clients poll the get endpoint
    // to observe the transition.
    if let Err(e) = ctx
        .store
        .enqueue_job(NewJob {
            kind: JobKind::Provision {
                instance_id: instance.id,
            },
            target_cn_uuid,
        })
        .await
    {
        // Failure to enqueue is operationally bad — the instance
        // record exists but will never provision. Surface as
        // 5xx; operators can retry by re-creating with a new
        // name (Phase 0 doesn't support requeue).
        ctx.audit
            .record_mutation(
                &principal,
                Action::InstanceCreate,
                request_id,
                Some(format!("Instance::\"{}\"", instance.id)),
                store_error_to_audit_outcome(&e),
                serde_json::Value::Null,
            )
            .await;
        return Err(store_error_to_http(e));
    }

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
