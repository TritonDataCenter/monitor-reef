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

/// RFD 00007 AP-3a `GET /v1/system/instances?image=&cn=&silo=&tenant=&project=&state=`.
///
/// Fleet-wide instance search. This is the endpoint that opened the
/// entire RFD: an operator needs to ask "which VMs are using image X?"
/// in one HTTP call. Backed by the AP-1c secondary indexes for
/// image and cn (single FDB range read); falls back to per-tenant
/// project membership when scoped narrower.
///
/// Capability gate: requires `SystemRead`. A caller without it gets
/// 404 NotFound (indistinguishable from "no such path" per Locked
/// Decision #3).
///
/// Dispatch (priority order, narrowest index first):
///   ?image=<uuid>  -> Store::list_instances_by_image
///   ?cn=<uuid>     -> Store::list_instances_by_cn
///   else           -> 400 MissingScope today (cross-fleet scan
///                     without an index would exceed SCAN_CAP).
///                     A future slice may walk all tenant/project
///                     prefixes when bounded by additional
///                     selectors.
///
/// Post-index filters re-apply silo/tenant/project/cn/image/state
/// to the result set so combinations narrow correctly.
pub(crate) async fn list_system_instances_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::InstanceQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Instance>>, HttpError> {
    use tritond_api::v1::{InstanceQuery, ResultsPage};
    let ctx = rqctx.context();
    // Capability gate. A non-fleet-admin sees 404 here just like
    // any other /v1/system/ endpoint.
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, tritond_store::Capability::SystemRead)?;

    let InstanceQuery {
        scope,
        image,
        cn,
        state,
    } = query.into_inner();

    let raw: Vec<Instance> = if let Some(image_id) = image {
        ctx.store
            .list_instances_by_image(image_id)
            .await
            .map_err(store_error_to_http)?
    } else if let Some(cn_id) = cn {
        ctx.store
            .list_instances_by_cn(cn_id)
            .await
            .map_err(store_error_to_http)?
    } else {
        return Err(HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/system/instances requires `image=` or `cn=` to scope the \
             fleet-wide scan; full-fleet enumeration is not enabled at AP-3a-2"
                .to_string(),
        ));
    };

    // Apply the remaining selectors as filters. The
    // `Instance.tenant_id`, `project_id`, `host_cn_uuid`, and
    // `image_id` fields are all checked; the silo selector resolves
    // through a per-instance Tenant.silo_id lookup which we skip at
    // AP-3a-2 (operators usually scope via tenant directly).
    let mut filtered: Vec<Instance> = raw
        .into_iter()
        .filter(|i| {
            scope.tenant.is_none_or(|t| i.tenant_id == t)
                && scope.project.is_none_or(|p| i.project_id == p)
                && image.is_none_or(|im| i.image_id == im)
                && cn.is_none_or(|c| i.host_cn_uuid == Some(c))
        })
        .collect();

    if let Some(want) = state.as_deref() {
        filtered.retain(|i| {
            let k = i.lifecycle.kind();
            format!("{k:?}").to_ascii_lowercase() == want.to_ascii_lowercase()
        });
    }

    Ok(HttpResponseOk(ResultsPage::single(filtered)))
}

/// RFD 00007 AP-3a-4: `GET /v1/system/networking/nics?ip=&subnet=&instance=&mac=`.
/// Fleet-wide NIC search. Operator's "who owns 10.x.x.x?" query.
///
/// Indexed dispatch (priority: ip > subnet > instance):
///   ?ip=<addr>  -> Store::find_nic_by_ip (unique by invariant)
///   ?subnet=    -> Store::list_nics_by_subnet (AP-1c index)
///   ?instance=  -> Store::list_nics_for_instance (existing index)
///
/// Capability: `SystemRead`.
pub(crate) async fn list_system_nics_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::NicQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Nic>>, HttpError> {
    use tritond_api::v1::{NicQuery, ResultsPage};
    let ctx = rqctx.context();
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, tritond_store::Capability::SystemRead)?;
    let NicQuery {
        scope,
        instance,
        subnet,
        ip,
    } = query.into_inner();
    let raw: Vec<Nic> = if let Some(ip_addr) = ip {
        match ctx.store.find_nic_by_ip(ip_addr).await {
            Ok(n) => vec![n],
            Err(StoreError::NotFound) => Vec::new(),
            Err(e) => return Err(store_error_to_http(e)),
        }
    } else if let Some(subnet_id) = subnet {
        ctx.store
            .list_nics_by_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?
    } else if let Some(instance_id) = instance {
        ctx.store
            .list_nics_for_instance(instance_id)
            .await
            .map_err(store_error_to_http)?
    } else {
        return Err(HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/system/networking/nics requires `ip=`, `subnet=`, or `instance=`".to_string(),
        ));
    };
    let nics: Vec<Nic> = raw
        .into_iter()
        .filter(|n| {
            scope.tenant.is_none_or(|t| n.tenant_id == t)
                && scope.project.is_none_or(|p| n.project_id == p)
        })
        .collect();
    Ok(HttpResponseOk(ResultsPage::single(nics)))
}

/// RFD 00007 AP-3a-3: `GET /v1/system/cns/{cn_id}/instances`.
/// Fixed-axis view: every instance currently placed on a single CN.
/// The natural endpoint behind the admin UI's CN-detail "Hosted
/// instances" tab. Backed by the existing
/// `instance/in_host_cn/<cn>/` index (delegate to
/// `Store::list_instances_by_cn`).
///
/// Capability: `SystemRead`.
pub(crate) async fn list_system_cn_instances_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemCnPath>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Instance>>, HttpError> {
    use tritond_api::v1::ResultsPage;
    let ctx = rqctx.context();
    let tritond_api::v1::SystemCnPath { cn_id } = path.into_inner();
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, tritond_store::Capability::SystemRead)?;
    let instances = ctx
        .store
        .list_instances_by_cn(cn_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(instances)))
}

/// RFD 00007 AP-3a-3: `GET /v1/system/images/{image_id}/instances`.
/// Fixed-axis "what's using this image?" view. The natural endpoint
/// behind the admin UI's Image-detail "In use by" tab and the
/// answer to the question that opened RFD 00007. Backed by the
/// AP-1c `idx/image/<image>/<instance>` keyspace.
///
/// Capability: `SystemRead`.
pub(crate) async fn list_system_image_instances_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::SystemImagePath>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Instance>>, HttpError> {
    use tritond_api::v1::ResultsPage;
    let ctx = rqctx.context();
    let tritond_api::v1::SystemImagePath { image_id } = path.into_inner();
    let principal = crate::auth::authenticate_only(&rqctx, &ctx.auth, &ctx.store).await?;
    crate::auth::require_capability(&principal, tritond_store::Capability::SystemRead)?;
    let instances = ctx
        .store
        .list_instances_by_image(image_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(instances)))
}

/// RFD 00007 `GET /v1/instances?tenant=&project=&image=&cn=&state=`.
///
/// The flat customer-facing instance list. Dispatches on the
/// selector set:
///
/// * `?image=<uuid>` -> single FDB range read against the AP-1c
///   `instance/in_image/<image>/` index, then per-id point reads.
/// * `?cn=<uuid>` -> existing `instance/in_host_cn/<cn>/` index via
///   `Store::list_instances_by_cn`.
/// * `?tenant=<uuid>&project=<uuid>` -> the bounded `list_instances_in_project`
///   path, identical to the legacy `/v2/tenants/{t}/projects/{p}/instances`.
/// * No selectors set, or only `?tenant=` -> `400 BadRequest`
///   `MissingScope` until AP-3a lands the bounded-scan across-projects
///   handler. A future slice expands this; today the failure is
///   typed so a client can react.
///
/// Cross-scope checks: a non-fleet-admin principal who sets
/// `?tenant=` outside their own tenant gets `404 NotFound` (via
/// `authenticate_and_authorize_in_tenant`). The `?silo=` selector
/// is rejected with `400 ScopeNotAccepted` on this endpoint (the
/// fleet-admin variant lives at `/v1/system/instances` in a later
/// slice).
///
/// AP-2b ships UUID-only selector parsing; name-resolution
/// (`NameOrId::Name(...)`) returns `400 BadRequest` until
/// `handlers::selectors::resolve_name_or_id` lands in AP-3a.
pub(crate) async fn list_instances_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::InstanceQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Instance>>, HttpError> {
    use tritond_api::v1::{InstanceQuery, ResultsPage};
    let ctx = rqctx.context();
    let InstanceQuery {
        scope,
        image,
        cn,
        state,
    } = query.into_inner();

    // Reject `?silo=` on the customer endpoint (typed error per
    // RFD 00007 D-Ap-8). Fleet-admin reads with silo scope live at
    // `/v1/system/instances`, which lands in a later slice.
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints; \
             call /v1/system/instances if you have fleet-admin capabilities"
                .to_string(),
        ));
    }

    // AP-2b ships UUID-only selectors (Dropshot constraint on scalar
    // query params). AP-3a swaps to a `NameOrId` newtype that
    // resolves names server-side via `handlers::selectors`.
    let tenant_uuid = scope.tenant;
    let project_uuid = scope.project;
    let image_uuid = image;
    let cn_uuid = cn;

    // Authentication: the principal must be authenticated and, if
    // they specified a tenant, must be in that tenant's silo.
    if let Some(tenant_id) = tenant_uuid {
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceList,
            tenant_id,
        )
        .await?;
    } else {
        authenticate_and_authorize(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::InstanceList,
        )
        .await?;
    }

    // Dispatch on the selector set. Priority order favours the
    // narrowest index: image > cn > project.
    let raw: Vec<Instance> = if let Some(image_id) = image_uuid {
        ctx.store
            .list_instances_by_image(image_id)
            .await
            .map_err(store_error_to_http)?
    } else if let Some(cn_id) = cn_uuid {
        ctx.store
            .list_instances_by_cn(cn_id)
            .await
            .map_err(store_error_to_http)?
    } else if let Some(project_id) = project_uuid {
        // tenant_uuid is required when project_uuid is set so the
        // cross-tenant 404 check above already fired; verify the
        // project lives under the named tenant for defence in depth.
        let project = ctx
            .store
            .get_project(project_id)
            .await
            .map_err(store_error_to_http)?;
        if let Some(tenant_id) = tenant_uuid
            && project.tenant_id != tenant_id
        {
            return Err(not_found());
        }
        ctx.store
            .list_instances_in_project(project_id)
            .await
            .map_err(store_error_to_http)?
    } else {
        // AP-2b: no fleet-wide bounded-scan handler yet. The
        // operator surface at /v1/system/instances will accept this
        // shape; the customer surface always requires either an
        // indexed selector (image/cn) or a project scope.
        return Err(HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "set `image=`, `cn=`, or `tenant=&project=` to scope the list; \
             cross-project scans on the customer surface require AP-3a"
                .to_string(),
        ));
    };

    // Cross-scope filters (post-index). When the index gave us a
    // wider set than the caller asked for, narrow it client-side
    // (still inside the handler) to honour the additional selectors.
    let mut filtered: Vec<Instance> = raw
        .into_iter()
        .filter(|i| {
            tenant_uuid.is_none_or(|t| i.tenant_id == t)
                && project_uuid.is_none_or(|p| i.project_id == p)
                && cn_uuid.is_none_or(|c| i.host_cn_uuid == Some(c))
                && image_uuid.is_none_or(|im| i.image_id == im)
        })
        .collect();

    // `state=` is a string match against the LifecycleState kind
    // name. Bounded by the result-set size from the indexed
    // selector above so we cannot exceed SCAN_CAP here.
    if let Some(want) = state.as_deref() {
        filtered.retain(|i| {
            let k = i.lifecycle.kind();
            format!("{k:?}").to_ascii_lowercase() == want.to_ascii_lowercase()
        });
    }

    Ok(HttpResponseOk(ResultsPage::single(filtered)))
}

/// RFD 00007 AP-2d: `POST /v1/instances?tenant=&project=` body
/// handler. Unpacks the query selectors, requires both `tenant=` and
/// `project=` (the customer surface always requires a project scope
/// for creates), then delegates to the same inner machinery as the
/// v2 path-scoped handler. `silo=` is rejected with 400
/// `ScopeNotAccepted` here per RFD 00007 D-Ap-8.
pub(crate) async fn create_instance_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::ScopeSelectors>,
    body: TypedBody<NewInstance>,
) -> Result<HttpResponseCreated<Instance>, HttpError> {
    let scope = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is not accepted on the customer surface; \
             /v1/instances always creates inside the principal's silo"
                .to_string(),
        ));
    }
    let tenant_id = scope.tenant.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/instances requires `?tenant=<uuid>&project=<uuid>` selectors".to_string(),
        )
    })?;
    let project_id = scope.project.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/instances requires `?tenant=<uuid>&project=<uuid>` selectors".to_string(),
        )
    })?;
    create_instance_inner(rqctx, tenant_id, project_id, body.into_inner()).await
}

/// Shared inner-impl for instance creation. Used by both the v2
/// path-scoped handler and the v1 query-scoped handler. Carries the
/// full validate -> Cedar -> saga flow; the wrappers only differ in
/// how they reconstruct the (tenant_id, project_id) pair.
async fn create_instance_inner(
    rqctx: RequestContext<ApiContext>,
    tenant_id: Uuid,
    project_id: Uuid,
    req: NewInstance,
) -> Result<HttpResponseCreated<Instance>, HttpError> {
    let ctx = rqctx.context();
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

// AP-2c: flat /v1/instances/{instance_id}[/{action}] handlers.
// Each reads the instance row to recover (tenant_id, project_id),
// then delegates to the shared inner machinery. The principal's
// silo is checked inside the inner functions via
// `authenticate_and_authorize_in_tenant`. Cross-tenant probes
// surface as 404 because the inner check rejects with NotFound when
// the principal's silo doesn't match the instance's tenant.

pub(crate) async fn get_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::InstanceGet,
        instance.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(instance))
}

pub(crate) async fn delete_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
    query: Query<InstanceDeleteQuery>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let _force = query.into_inner().force;

    // Read the instance first so we can authorise against its tenant
    // and recover the (tenant_id, project_id, target_cn_uuid) the
    // saga's params and audit records need. /v1/ paths don't carry
    // tenant/project in the URL the way the legacy /v2/ shape did.
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = instance.tenant_id;
    let project_id = instance.project_id;
    let target_cn_uuid = instance.host_cn_uuid;

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

    // v2p invalidation push (PROTEUS_PLAN §11.7.1 item 8). Lives
    // here (not in the saga action body) because the global
    // `peer_invalidations` ring isn't reachable from a SagaContext
    // yet. Done before the saga so the release_record action can
    // drop the NIC rows safely.
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

    // RFD 00004 SG-3: instance-delete saga. Detaches FIPs, enqueues
    // a Delete job for the agent, awaits the agent's terminal
    // status, then releases the record. Unwinds via the detach
    // undo if anything fails before the record is released; lands
    // Stuck if release_record fails after the agent acked Delete.
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

pub(crate) async fn start_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    lifecycle_saga_entry_uuid(
        rqctx,
        instance.tenant_id,
        instance.project_id,
        instance_id,
        Action::InstanceStart,
        crate::sagas::instance_lifecycle::LifecycleOp::Start,
    )
    .await
}

pub(crate) async fn stop_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    lifecycle_saga_entry_uuid(
        rqctx,
        instance.tenant_id,
        instance.project_id,
        instance_id,
        Action::InstanceStop,
        crate::sagas::instance_lifecycle::LifecycleOp::Stop,
    )
    .await
}

pub(crate) async fn restart_instance_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::InstancePath>,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::InstancePath { instance_id } = path.into_inner();
    let instance = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    lifecycle_saga_entry_uuid(
        rqctx,
        instance.tenant_id,
        instance.project_id,
        instance_id,
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
    let TenantProjectInstancePath {
        tenant_id,
        project_id,
        instance_id,
    } = path.into_inner();
    lifecycle_saga_entry_uuid(rqctx, tenant_id, project_id, instance_id, action, op).await
}

/// AP-2c: shared inner entry-point for both the v2 (path-scoped)
/// and v1 (flat instance_id) lifecycle handlers. Takes the
/// already-unpacked uuids so the flat /v1 endpoint can resolve the
/// owning tenant + project from the Instance row before reaching
/// the saga machinery.
async fn lifecycle_saga_entry_uuid(
    rqctx: RequestContext<ApiContext>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    action: Action,
    op: crate::sagas::instance_lifecycle::LifecycleOp,
) -> Result<HttpResponseOk<Instance>, HttpError> {
    let ctx = rqctx.context();
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

/// RFD 00007 AP-2f: `GET /v1/nics?tenant=&project=&instance=&subnet=&ip=`.
/// Flat NIC list backed by the AP-1c secondary indexes. Dispatches
/// on the selector set (priority: ip > subnet > instance) so each
/// query is one index read.
pub(crate) async fn list_nics_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::NicQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Nic>>, HttpError> {
    use tritond_api::v1::{NicQuery, ResultsPage};
    let ctx = rqctx.context();
    let NicQuery {
        scope,
        instance,
        subnet,
        ip,
    } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    // Authentication: if the principal scoped to a tenant, check it.
    // Otherwise the per-row tenant check below 404s on cross-tenant
    // probes after the index narrows.
    if let Some(tenant_id) = scope.tenant {
        authenticate_and_authorize_in_tenant(
            &rqctx,
            &ctx.auth,
            &ctx.audit,
            &ctx.store,
            Action::NicList,
            tenant_id,
        )
        .await?;
    } else {
        authenticate_and_authorize(&rqctx, &ctx.auth, &ctx.audit, &ctx.store, Action::NicList)
            .await?;
    }
    let raw: Vec<Nic> = if let Some(ip_addr) = ip {
        // ip is unique by invariant -> at most one NIC.
        match ctx.store.find_nic_by_ip(ip_addr).await {
            Ok(n) => vec![n],
            Err(StoreError::NotFound) => Vec::new(),
            Err(e) => return Err(store_error_to_http(e)),
        }
    } else if let Some(subnet_id) = subnet {
        ctx.store
            .list_nics_by_subnet(subnet_id)
            .await
            .map_err(store_error_to_http)?
    } else if let Some(instance_id) = instance {
        ctx.store
            .list_nics_for_instance(instance_id)
            .await
            .map_err(store_error_to_http)?
    } else {
        return Err(HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/nics requires one of `ip=`, `subnet=`, or `instance=`".to_string(),
        ));
    };
    // Filter the index result set against the remaining selectors.
    let nics: Vec<Nic> = raw
        .into_iter()
        .filter(|n| {
            scope.tenant.is_none_or(|t| n.tenant_id == t)
                && scope.project.is_none_or(|p| n.project_id == p)
                && instance.is_none_or(|i| n.instance_id == i)
                && subnet.is_none_or(|s| n.subnet_id == s)
        })
        .collect();
    Ok(HttpResponseOk(ResultsPage::single(nics)))
}

/// RFD 00007 AP-2f: `GET /v1/nics/{nic_id}`. Flat single-NIC read.
pub(crate) async fn get_nic_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::NicPath>,
) -> Result<HttpResponseOk<Nic>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::NicPath { nic_id } = path.into_inner();
    let nic = ctx
        .store
        .get_nic(nic_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::NicGet,
        nic.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(nic))
}

/// RFD 00007 AP-2e: `GET /v1/disks?tenant=&project=&instance=`.
/// Flat disk list. Requires `?instance=<uuid>` for now; a future
/// slice expands to cross-project disk searches once the customer
/// surface needs them.
pub(crate) async fn list_disks_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::DiskQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<Disk>>, HttpError> {
    use tritond_api::v1::{DiskQuery, ResultsPage};
    let ctx = rqctx.context();
    let DiskQuery { scope, instance } = query.into_inner();

    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    let instance_id = instance.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/disks requires `?instance=<uuid>` until the cross-project \
             disk search lands in a later AP-2 slice"
                .to_string(),
        )
    })?;
    // Read the instance to recover the owning tenant, then auth in
    // that tenant. Cross-tenant probes 404 because the auth check
    // rejects when the principal's silo doesn't match.
    let inst = ctx
        .store
        .get_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    if let Some(t) = scope.tenant
        && inst.tenant_id != t
    {
        return Err(not_found());
    }
    if let Some(p) = scope.project
        && inst.project_id != p
    {
        return Err(not_found());
    }
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DiskList,
        inst.tenant_id,
    )
    .await?;
    let disks = ctx
        .store
        .list_disks_for_instance(instance_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(ResultsPage::single(disks)))
}

/// RFD 00007 AP-2e: `GET /v1/disks/{disk_id}`. Flat single-disk
/// read by UUID; reads the disk row, derives the owning tenant via
/// the parent instance, then auths against the principal's silo.
pub(crate) async fn get_disk_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::DiskPath>,
) -> Result<HttpResponseOk<Disk>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::DiskPath { disk_id } = path.into_inner();
    let disk = ctx
        .store
        .get_disk(disk_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::DiskGet,
        disk.tenant_id,
    )
    .await?;
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

/// RFD 00007 AP-2i: `GET /v1/floating-ips?tenant=&project=`. Flat
/// floating-IP list scoped to a project. Both selectors required.
pub(crate) async fn list_floating_ips_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::FloatingIpQuery>,
) -> Result<HttpResponseOk<tritond_api::v1::ResultsPage<FloatingIp>>, HttpError> {
    use tritond_api::v1::{FloatingIpQuery, ResultsPage};
    let ctx = rqctx.context();
    let FloatingIpQuery { scope } = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is only accepted on /v1/system/ endpoints".to_string(),
        ));
    }
    let tenant_id = scope.tenant.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/floating-ips requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;
    let project_id = scope.project.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "GET /v1/floating-ips requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpList,
        tenant_id,
    )
    .await?;
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
    Ok(HttpResponseOk(ResultsPage::single(fips)))
}

/// RFD 00007 AP-2i: `GET /v1/floating-ips/{floating_ip_id}`. Flat
/// single-FIP read.
pub(crate) async fn get_floating_ip_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::FloatingIpPath>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::FloatingIpPath { floating_ip_id } = path.into_inner();
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpGet,
        fip.tenant_id,
    )
    .await?;
    Ok(HttpResponseOk(fip))
}

/// RFD 00007 AP-3a-12: `POST /v1/floating-ips?tenant=&project=`.
/// Drives the same allocate-saga the legacy v2 handler used.
pub(crate) async fn create_floating_ip_v1(
    rqctx: RequestContext<ApiContext>,
    query: Query<tritond_api::v1::ScopeSelectors>,
    body: TypedBody<NewFloatingIp>,
) -> Result<HttpResponseCreated<FloatingIp>, HttpError> {
    let scope = query.into_inner();
    if scope.silo.is_some() {
        return Err(HttpError::for_client_error(
            Some("ScopeNotAccepted".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "the `silo` selector is not accepted on the customer surface; \
             /v1/floating-ips always allocates inside the principal's silo"
                .to_string(),
        ));
    }
    let tenant_id = scope.tenant.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/floating-ips requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;
    let project_id = scope.project.ok_or_else(|| {
        HttpError::for_client_error(
            Some("MissingScope".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            "POST /v1/floating-ips requires `?tenant=<uuid>&project=<uuid>`".to_string(),
        )
    })?;

    let ctx = rqctx.context();
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

/// RFD 00007 AP-3a-12: `DELETE /v1/floating-ips/{floating_ip_id}`.
/// Tenant resolved from the row; store enforces the detach-first gate.
pub(crate) async fn delete_floating_ip_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::FloatingIpPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::FloatingIpPath { floating_ip_id } = path.into_inner();
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    let tenant_id = fip.tenant_id;
    let project_id = fip.project_id;

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

/// RFD 00007 AP-2i: `POST /v1/floating-ips/{floating_ip_id}/attach`.
/// Body is the same `AttachFloatingIpRequest` as v2.
pub(crate) async fn attach_floating_ip_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::FloatingIpPath>,
    body: TypedBody<AttachFloatingIpRequest>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::FloatingIpPath { floating_ip_id } = path.into_inner();
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpAttach,
        fip.tenant_id,
    )
    .await?;
    let req = body.into_inner();
    let attached = ctx
        .store
        .attach_floating_ip(floating_ip_id, req.nic_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(attached))
}

/// RFD 00007 AP-2i: `POST /v1/floating-ips/{floating_ip_id}/detach`.
pub(crate) async fn detach_floating_ip_v1(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::v1::FloatingIpPath>,
) -> Result<HttpResponseOk<FloatingIp>, HttpError> {
    let ctx = rqctx.context();
    let tritond_api::v1::FloatingIpPath { floating_ip_id } = path.into_inner();
    let fip = ctx
        .store
        .get_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    authenticate_and_authorize_in_tenant(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::FloatingIpDetach,
        fip.tenant_id,
    )
    .await?;
    let detached = ctx
        .store
        .detach_floating_ip(floating_ip_id)
        .await
        .map_err(store_error_to_http)?;
    Ok(HttpResponseOk(detached))
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
