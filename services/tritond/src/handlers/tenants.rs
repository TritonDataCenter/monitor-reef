// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tenants` HTTP handlers (delegated to from the `TritondApi` impl).

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

pub(crate) async fn put_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
    body: TypedBody<NewIdpConfig>,
) -> Result<HttpResponseCreated<IdpConfigView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpSet,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    // Confirm the tenant exists; reject 404 cleanly rather
    // than dangling an IdP config off a non-existent tenant.
    ctx.store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;

    let req = body.into_inner();
    let config = IdpConfig {
        issuer_url: req.issuer_url,
        client_id: req.client_id,
        client_secret: req.client_secret.expose().to_string(),
        audience: req.audience,
    };

    // Eager discovery: populate the verifier cache (and prove the
    // IdP is reachable + speaks OIDC) before persisting. A failed
    // discovery returns 4xx with the upstream error.
    let oidc_cfg = OidcConfig {
        issuer_url: config.issuer_url.clone(),
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        audience: config.audience.clone(),
    };
    ctx.auth
        .oidc()
        .discover(&tenant_id.to_string(), &oidc_cfg)
        .await
        .map_err(|e| {
            HttpError::for_client_error(
                Some("IdpUnreachable".to_string()),
                ClientErrorStatusCode::BAD_REQUEST,
                format!("idp discovery failed: {e}"),
            )
        })?;

    let saved = ctx
        .store
        .put_idp_config(tenant_id, config)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseCreated(saved.into()))
}

pub(crate) async fn get_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
) -> Result<HttpResponseOk<IdpConfigView>, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpGet,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    let config = ctx
        .store
        .get_idp_config(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(config.into()))
}

pub(crate) async fn delete_tenant_idp(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantIdpPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantIdpDelete,
    )
    .await?;
    let tenant_id = path.into_inner().tenant_id;
    ctx.store
        .delete_idp_config(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.auth.oidc().invalidate(&tenant_id.to_string()).await;
    Ok(HttpResponseDeleted())
}

pub(crate) async fn list_silo_tenants(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
) -> Result<HttpResponseOk<Vec<Tenant>>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantList,
        silo_id,
    )
    .await?;
    let tenants = ctx
        .store
        .list_tenants_in_silo(silo_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(tenants))
}

pub(crate) async fn create_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloPath>,
    body: TypedBody<NewTenant>,
) -> Result<HttpResponseCreated<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let silo_id = path.into_inner().silo_id;
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // Pre-generate the tenant id so we can derive the mantad
    // workspace name (`t-{tenant_id_simple}`) and carry it as the
    // idempotency key on the workspace-create RPC. A retried
    // request hits mantad with the same key and gets the existing
    // workspace back, then the Tenant row commits with the
    // binding populated. No two-store transaction; the failure
    // contract is "no Tenant row unless the workspace exists."
    let tenant_id = Uuid::new_v4();

    // Read the fleet's default S3 cluster. `None` means no
    // cluster has been registered as the default yet — tenant is
    // created without a workspace binding and the forwarder
    // layer returns 412 on any S3 op against it.
    let settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    let default_cluster = settings.storage_default_s3_cluster_id;

    let (storage_workspace_id, storage_cluster_id) = match default_cluster {
        None => (None, None),
        Some(cluster_id) => {
            // `client_for` resolves the cluster row, refuses non-S3
            // surfaces with 409, and builds a ready-to-call mantad
            // client.
            let (cluster, client) = match crate::storage::client_for(&ctx.store, cluster_id).await {
                Ok(pair) => pair,
                Err(http_err) => {
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::TenantCreate,
                            request_id,
                            None,
                            AuditOutcome::ServerError {
                                message: format!(
                                    "fleet default storage cluster {cluster_id} unresolvable: \
                                     {http_err}"
                                ),
                            },
                            serde_json::json!({
                                "silo_id": silo_id,
                                "name": req.name,
                                "default_s3_cluster_id": cluster_id,
                            }),
                        )
                        .await;
                    return Err(http_err);
                }
            };

            // Pre-flight: refuse early if the cluster's last probe
            // failed. The handler's contract is "fail loudly with
            // no Tenant row" so the operator sees the cluster
            // problem before any state is written.
            if cluster.status == tritond_store::StorageClusterStatus::Unreachable {
                let msg = format!(
                    "fleet default storage cluster {} ({}) last health probe was Unreachable; \
                     refresh with `tcadm storage health {}` and retry",
                    cluster.name, cluster.id, cluster.id
                );
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::TenantCreate,
                        request_id,
                        None,
                        AuditOutcome::ServerError {
                            message: msg.clone(),
                        },
                        serde_json::json!({
                            "silo_id": silo_id,
                            "name": req.name,
                            "default_s3_cluster_id": cluster.id,
                            "cluster_status": "unreachable",
                        }),
                    )
                    .await;
                return Err(HttpError::for_unavail(
                    Some("StorageClusterUnreachable".to_string()),
                    msg,
                ));
            }

            // Mint the workspace. Mantad's create endpoint is
            // idempotent keyed on `tenant_uuid`, so retries of
            // this RPC return the existing workspace rather than
            // failing with 409 — that's the cross-daemon
            // retry-safety contract.
            let workspace_name = format!("t-{}", tenant_id.simple());
            let mantad_req = mantad_client::types::CreateWorkspaceRequest {
                name: workspace_name,
                description: Some(req.name.clone()),
                quota_bytes: Some(settings.storage_default_workspace_quota_bytes),
                quota_objects: None,
                tenant_uuid: Some(tenant_id),
            };
            match client.create_workspace(&mantad_req).await {
                Ok(_workspace) => (Some(tenant_id), Some(cluster.id)),
                Err(e) => {
                    let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
                    ctx.audit
                        .record_mutation(
                            &principal,
                            Action::TenantCreate,
                            request_id,
                            None,
                            audit_outcome,
                            serde_json::json!({
                                "silo_id": silo_id,
                                "name": req.name,
                                "default_s3_cluster_id": cluster.id,
                                "stage": "mantad.create_workspace",
                            }),
                        )
                        .await;
                    return Err(http_err);
                }
            }
        }
    };

    match ctx
        .store
        .create_tenant_with_binding(
            silo_id,
            tenant_id,
            req,
            storage_workspace_id,
            storage_cluster_id,
        )
        .await
    {
        Ok(tenant) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{}\"", tenant.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Tenant::\"{}\"", tenant.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "name": tenant.name,
                        "storage_cluster_id": tenant.storage_cluster_id,
                        "storage_workspace_id": tenant.storage_workspace_id,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(tenant))
        }
        Err(e) => {
            // The workspace already exists on mantad (when we got
            // this far via the bound path). On retry, the mantad
            // idempotency key resolves it; the operator's next
            // attempt will hit the same workspace and commit
            // the Tenant row.
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({
                        "silo_id": silo_id,
                        "storage_cluster_id": storage_cluster_id,
                        "storage_workspace_id": storage_workspace_id,
                        "stage": "store.create_tenant_with_binding",
                    }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn get_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseOk<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantGet,
        silo_id,
    )
    .await?;
    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    // Defence-in-depth: a tenant from another silo must surface as
    // 404, not as a successful read of a sibling silo's resource.
    if tenant.silo_id != silo_id {
        return Err(not_found());
    }
    Ok(HttpResponseOk(tenant))
}

pub(crate) async fn delete_silo_tenant(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantDelete,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(not_found());
    }

    // Archive the mantad workspace first. The Tenant row drops
    // only after the workspace is gone (or was never there) so
    // a 409 from mantad ("non-empty workspace") preserves the
    // tenant for retry. Mirrors the create-side ordering: state
    // on mantad leads, tritond's row follows.
    if let (Some(workspace_uuid), Some(cluster_id)) =
        (tenant.storage_workspace_id, tenant.storage_cluster_id)
    {
        let (_, client) = match crate::storage::client_for(&ctx.store, cluster_id).await {
            Ok(pair) => pair,
            Err(http_err) => {
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::TenantDelete,
                        request_id,
                        None,
                        AuditOutcome::ServerError {
                            message: format!(
                                "bound storage cluster {cluster_id} unresolvable: {http_err}"
                            ),
                        },
                        serde_json::json!({
                            "silo_id": silo_id,
                            "tenant_id": tenant_id,
                            "storage_cluster_id": cluster_id,
                            "stage": "storage.client_for",
                        }),
                    )
                    .await;
                return Err(http_err);
            }
        };
        let workspace_name = format!("t-{}", workspace_uuid.simple());
        match client.delete_workspace(&workspace_name).await {
            Ok(()) => {}
            // Workspace already archived on mantad (manual cleanup
            // or earlier partial-retry success): proceed to drop
            // the Tenant row. Anything else (including 409 for a
            // non-empty workspace) surfaces unchanged.
            Err(mantad_client::MantadClientError::Status { status: 404, .. }) => {}
            Err(e) => {
                let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
                ctx.audit
                    .record_mutation(
                        &principal,
                        Action::TenantDelete,
                        request_id,
                        None,
                        audit_outcome,
                        serde_json::json!({
                            "silo_id": silo_id,
                            "tenant_id": tenant_id,
                            "storage_cluster_id": cluster_id,
                            "storage_workspace_id": workspace_uuid,
                            "stage": "mantad.delete_workspace",
                        }),
                    )
                    .await;
                return Err(http_err);
            }
        }
    }

    // TODO: today's `Store::delete_tenant` is permissive — it
    // does not block the delete when child projects (or other
    // descendant resources) still exist. The block-on-children
    // guard belongs in a future cleanup so a careless operator
    // can't orphan a project graph by deleting its tenant.
    ctx.store
        .delete_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    ctx.audit
        .record_mutation(
            &principal,
            Action::TenantDelete,
            request_id,
            Some(format!("Tenant::\"{tenant_id}\"")),
            AuditOutcome::Success {
                resource: Some(format!("Tenant::\"{tenant_id}\"")),
            },
            serde_json::json!({
                "silo_id": silo_id,
                "storage_cluster_id": tenant.storage_cluster_id,
                "storage_workspace_id": tenant.storage_workspace_id,
            }),
        )
        .await;
    Ok(HttpResponseDeleted())
}

/// Drop the storage workspace binding from a tenant.
///
/// Flow:
///   1. Authn/authz as TenantCreate in silo (same right that lets
///      the operator init the binding; symmetric).
///   2. Cross-silo defence + 412 if the tenant has no binding.
///   3. Resolve the bound cluster + build a mantad client.
///   4. Pre-flight the cluster's last health probe; 503 if
///      Unreachable.
///   5. mantad.delete_workspace — 409 propagates if the
///      workspace still has buckets.
///   6. clear_tenant_storage_binding — write None/None on the
///      tenant row.
pub(crate) async fn drop_silo_tenant_storage(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseOk<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }
    let (workspace_uuid, cluster_id) =
        match (tenant.storage_workspace_id, tenant.storage_cluster_id) {
            (Some(w), Some(c)) => (w, c),
            _ => {
                return Err(HttpError::for_client_error(
                    Some("TenantStorageUnbound".to_string()),
                    ClientErrorStatusCode::PRECONDITION_FAILED,
                    format!("tenant {tenant_id} has no storage binding to drop"),
                ));
            }
        };

    let (cluster, client) = match crate::storage::client_for(&ctx.store, cluster_id).await {
        Ok(pair) => pair,
        Err(http_err) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{tenant_id}\"")),
                    AuditOutcome::ServerError {
                        message: format!("storage cluster {cluster_id} unresolvable: {http_err}"),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "storage_cluster_id": cluster_id,
                        "stage": "drop_storage.client_for",
                    }),
                )
                .await;
            return Err(http_err);
        }
    };

    if cluster.status == tritond_store::StorageClusterStatus::Unreachable {
        let msg = format!(
            "storage cluster {} ({}) last health probe was Unreachable; refresh with `tcadm \
             storage health {}` and retry",
            cluster.name, cluster.id, cluster.id
        );
        ctx.audit
            .record_mutation(
                &principal,
                Action::TenantCreate,
                request_id,
                Some(format!("Tenant::\"{tenant_id}\"")),
                AuditOutcome::ServerError {
                    message: msg.clone(),
                },
                serde_json::json!({
                    "silo_id": silo_id,
                    "tenant_id": tenant_id,
                    "storage_cluster_id": cluster.id,
                    "cluster_status": "unreachable",
                    "stage": "drop_storage.preflight",
                }),
            )
            .await;
        return Err(HttpError::for_unavail(
            Some("StorageClusterUnreachable".to_string()),
            msg,
        ));
    }

    let workspace_name = format!("t-{}", workspace_uuid.simple());
    if let Err(e) = client.delete_workspace(&workspace_name).await {
        let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
        ctx.audit
            .record_mutation(
                &principal,
                Action::TenantCreate,
                request_id,
                Some(format!("Tenant::\"{tenant_id}\"")),
                audit_outcome,
                serde_json::json!({
                    "silo_id": silo_id,
                    "tenant_id": tenant_id,
                    "storage_cluster_id": cluster.id,
                    "workspace_name": workspace_name,
                    "stage": "drop_storage.mantad.delete_workspace",
                }),
            )
            .await;
        return Err(http_err);
    }

    match ctx.store.clear_tenant_storage_binding(tenant_id).await {
        Ok(updated) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{}\"", updated.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Tenant::\"{}\"", updated.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": updated.id,
                        "former_storage_cluster_id": cluster_id,
                        "former_storage_workspace_id": workspace_uuid,
                        "stage": "drop_storage.complete",
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{tenant_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "stage": "drop_storage.store.clear_tenant_storage_binding",
                    }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

/// Create a tenant-bound operator account.
///
/// Operator-driven user creation: lands a User with
/// `tenant_id: Some(tenant_id)`, `is_root: false`, empty
/// capability set, and a bcrypt-hashed password. The plaintext
/// password is taken from the request body, hashed via
/// `tritond_auth::hash_password`, and never persisted in cleartext.
///
/// Used to mint test / non-federated tenant principals for
/// end-to-end forwarder verification. Federated users continue to
/// land via JIT-on-OIDC-login.
pub(crate) async fn create_silo_tenant_user(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
    body: TypedBody<tritond_api::types::NewSiloTenantUser>,
) -> Result<HttpResponseCreated<tritond_api::types::UserView>, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();

    // Cross-silo defence: tenant must live in the silo on the URL.
    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }

    // Hash the password before it reaches the store. We deliberately
    // drop the plaintext as soon as the hash is computed so the
    // audit payload below can't accidentally capture it.
    let password = tritond_auth::RedactedString::new(req.password);
    let password_hash = tritond_auth::hash_password(&password)
        .await
        .map_err(|e| HttpError::for_internal_error(format!("hash password: {e}")))?;
    let username = req.username.clone();
    let user = tritond_store::User {
        id: uuid::Uuid::new_v4(),
        username: req.username,
        password_hash,
        is_root: false,
        fleet_admin: false,
        created_at: chrono::Utc::now(),
        tenant_id: Some(tenant_id),
        federation: None,
        capabilities: Default::default(),
    };

    match ctx.store.create_user(user).await {
        Ok(created) => {
            let view: tritond_api::types::UserView = created.into();
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("User::\"{}\"", view.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("User::\"{}\"", view.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "username": view.username,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(view))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    None,
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "username": username,
                        "stage": "store.create_user",
                    }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

/// Retrofit a workspace binding onto an existing tenant. Used to
/// rescue tenants created before any `storage.default_s3_cluster_id`
/// was registered.
///
/// The flow mirrors `create_silo_tenant` minus the row insert:
///   1. Authn/authz as `TenantCreate` in the silo (same right used
///      to create a tenant — binding init is part of provisioning).
///   2. Cross-silo defence: ensure the addressed tenant lives in
///      the silo named on the URL; otherwise 404.
///   3. Read the default cluster id from settings; 412 if unset.
///   4. Pre-flight the cluster's last health probe; 503 on
///      Unreachable.
///   5. Call `mantad.create_workspace` with the existing
///      `tenant_id` as the idempotency key — if a prior attempt
///      partially succeeded the existing workspace is returned.
///   6. `set_tenant_storage_binding` writes both columns; the
///      store-level Conflict check refuses to overwrite a binding
///      already in place (409).
pub(crate) async fn init_silo_tenant_storage(
    rqctx: RequestContext<ApiContext>,
    path: Path<SiloTenantPath>,
) -> Result<HttpResponseOk<Tenant>, HttpError> {
    let ctx = rqctx.context();
    let SiloTenantPath { silo_id, tenant_id } = path.into_inner();
    let principal = authenticate_and_authorize_in_silo(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::TenantCreate,
        silo_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);

    // Cross-silo defence: a tenant in silo B reached via silo A's
    // URL must look "missing" not "forbidden".
    let tenant = ctx
        .store
        .get_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    if tenant.silo_id != silo_id {
        return Err(HttpError::for_not_found(
            Some("NotFound".to_string()),
            format!("tenant {tenant_id} not found in silo {silo_id}"),
        ));
    }
    if tenant.storage_workspace_id.is_some() || tenant.storage_cluster_id.is_some() {
        return Err(HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            format!(
                "tenant {tenant_id} already has a storage binding; drop the existing binding \
                 before rebinding"
            ),
        ));
    }

    let settings = ctx
        .store
        .get_settings()
        .await
        .map_err(store_error_to_http)?;
    let cluster_id = settings.storage_default_s3_cluster_id.ok_or_else(|| {
        HttpError::for_client_error(
            Some("NoDefaultStorageCluster".to_string()),
            ClientErrorStatusCode::PRECONDITION_FAILED,
            "no default S3 cluster is configured; set storage.default_s3_cluster_id via `tcadm \
             config set` first"
                .to_string(),
        )
    })?;

    let (cluster, client) = match crate::storage::client_for(&ctx.store, cluster_id).await {
        Ok(pair) => pair,
        Err(http_err) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{tenant_id}\"")),
                    AuditOutcome::ServerError {
                        message: format!(
                            "fleet default storage cluster {cluster_id} unresolvable: {http_err}"
                        ),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "default_s3_cluster_id": cluster_id,
                        "stage": "init_storage.client_for",
                    }),
                )
                .await;
            return Err(http_err);
        }
    };

    if cluster.status == tritond_store::StorageClusterStatus::Unreachable {
        let msg = format!(
            "fleet default storage cluster {} ({}) last health probe was Unreachable; refresh \
             with `tcadm storage health {}` and retry",
            cluster.name, cluster.id, cluster.id
        );
        ctx.audit
            .record_mutation(
                &principal,
                Action::TenantCreate,
                request_id,
                Some(format!("Tenant::\"{tenant_id}\"")),
                AuditOutcome::ServerError {
                    message: msg.clone(),
                },
                serde_json::json!({
                    "silo_id": silo_id,
                    "tenant_id": tenant_id,
                    "default_s3_cluster_id": cluster.id,
                    "cluster_status": "unreachable",
                    "stage": "init_storage.preflight",
                }),
            )
            .await;
        return Err(HttpError::for_unavail(
            Some("StorageClusterUnreachable".to_string()),
            msg,
        ));
    }

    let workspace_name = format!("t-{}", tenant_id.simple());
    let mantad_req = mantad_client::types::CreateWorkspaceRequest {
        name: workspace_name,
        description: Some(tenant.name.clone()),
        quota_bytes: Some(settings.storage_default_workspace_quota_bytes),
        quota_objects: None,
        tenant_uuid: Some(tenant_id),
    };
    if let Err(e) = client.create_workspace(&mantad_req).await {
        let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
        ctx.audit
            .record_mutation(
                &principal,
                Action::TenantCreate,
                request_id,
                Some(format!("Tenant::\"{tenant_id}\"")),
                audit_outcome,
                serde_json::json!({
                    "silo_id": silo_id,
                    "tenant_id": tenant_id,
                    "default_s3_cluster_id": cluster.id,
                    "stage": "init_storage.mantad.create_workspace",
                }),
            )
            .await;
        return Err(http_err);
    }

    match ctx
        .store
        .set_tenant_storage_binding(tenant_id, tenant_id, cluster.id)
        .await
    {
        Ok(updated) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{}\"", updated.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Tenant::\"{}\"", updated.id)),
                    },
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": updated.id,
                        "storage_cluster_id": updated.storage_cluster_id,
                        "storage_workspace_id": updated.storage_workspace_id,
                        "stage": "init_storage.complete",
                    }),
                )
                .await;
            Ok(HttpResponseOk(updated))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::TenantCreate,
                    request_id,
                    Some(format!("Tenant::\"{tenant_id}\"")),
                    store_error_to_audit_outcome(&e),
                    serde_json::json!({
                        "silo_id": silo_id,
                        "tenant_id": tenant_id,
                        "default_s3_cluster_id": cluster.id,
                        "stage": "init_storage.store.set_tenant_storage_binding",
                    }),
                )
                .await;
            Err(store_error_to_http(e))
        }
    }
}

pub(crate) async fn list_tenant_projects(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
) -> Result<HttpResponseOk<Vec<Project>>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectList,
        tenant_id,
    )
    .await?;
    let projects = ctx
        .store
        .list_projects_in_tenant(tenant_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(projects))
}

pub(crate) async fn create_tenant_project(
    rqctx: RequestContext<ApiContext>,
    path: Path<TenantPath>,
    body: TypedBody<NewProject>,
) -> Result<HttpResponseCreated<Project>, HttpError> {
    let ctx = rqctx.context();
    let tenant_id = path.into_inner().tenant_id;
    let principal = authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::ProjectCreate,
        tenant_id,
    )
    .await?;
    let request_id = parse_request_id(&rqctx);
    let req = body.into_inner();
    match ctx.store.create_project(tenant_id, req).await {
        Ok(project) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ProjectCreate,
                    request_id,
                    Some(format!("Project::\"{}\"", project.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("Project::\"{}\"", project.id)),
                    },
                    serde_json::json!({
                        "tenant_id": tenant_id,
                        "name": project.name,
                    }),
                )
                .await;
            Ok(HttpResponseCreated(project))
        }
        Err(e) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    Action::ProjectCreate,
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
