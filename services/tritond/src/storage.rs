// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Helpers for the storage-cluster forwarder endpoints.
//!
//! The `/v1/storage/clusters/{id}/...` surface forwards typed admin
//! calls to a registered `mantad`. This module supplies:
//!
//! 1. `client_for` — look up the cluster record, refuse if its
//!    surface is anything other than S3 (mantafs / manta-block ship
//!    later), build a [`MantadClient`] with the stored bearer token,
//!    return it alongside the [`StorageCluster`] so the caller can
//!    make secondary store updates (e.g. `update_storage_cluster_status`
//!    after a probe).
//! 2. `mantad_error_to_http` — translate
//!    [`MantadClientError`] to a Dropshot [`HttpError`] preserving
//!    the upstream status code so admin-backend can render mantad's
//!    own validation failures (409 on duplicate-name, 404 on missing
//!    bucket) instead of flattening everything to 500.
//! 3. Field-by-field copy functions (`*_from`, `*_to`) between each
//!    `mantad_client::types::*` and its mirror in
//!    `tritond_api::types::*`. The two type families are
//!    byte-identical on the wire — the mirrors exist only so
//!    Dropshot's `JsonSchema` requirement (schemars 0.8) is
//!    satisfied without forcing the manta-storage workspace to
//!    pin schemars 0.8 to match. They're written as plain
//!    functions, not `From` impls, because the orphan rule
//!    blocks `impl From<mantad_client::X> for tritond_api::Y`
//!    inside this crate (both types are foreign to `tritond`).

use std::sync::Arc;

use dropshot::{ClientErrorStatusCode, HttpError};
use mantad_client::{MantadClient, MantadClientError};
use tritond_api::{
    PresignResponse, StorageAccessKey, StorageBucket, StorageClusterSummary, StorageMembership,
    StorageNode, StorageObjectSummary, StorageObjectsPage, StoragePeerEntry, StorageUser,
};
use tritond_audit::Outcome as AuditOutcome;
use tritond_store::{StorageCluster, StorageClusterSurface, Store, StoreError};
use uuid::Uuid;

use crate::auth::Principal;
use crate::sigv4;

/// Resolved storage workspace context for a forwarder request.
///
/// Returned by [`resolve_workspace_scope`] so handlers can fail
/// loudly on tenants without a workspace binding (412) before
/// they reach the mantad call.
///
/// Today's storage forwarder doesn't yet pass the workspace
/// name to mantad-client — that requires extending the mantad
/// admin routes to accept a workspace parameter, which is its
/// own slice. The gate is still load-bearing: it prevents a
/// tenant whose `storage_workspace_id` is `None` from
/// silently sharing the cluster-wide bucket / IAM namespace
/// with bound tenants. Once the data-plane plumbing lands,
/// every forwarder will key its mantad call off [`Self::workspace_name`].
#[derive(Debug, Clone)]
pub(crate) enum WorkspaceScope {
    /// Caller is bound to one workspace; everything they see
    /// or mutate is scoped to it.
    Bound {
        /// Wire-name on mantad: `t-{tenant_uuid_simple}`.
        workspace_name: String,
    },
    /// Caller is permitted cross-tenant visibility (fleet audit,
    /// inventory, support tooling). Granted via the
    /// [`crate::auth::Action::WorkspaceListAcrossTenants`] Cedar
    /// action — root principals match by the catch-all root rule,
    /// and a non-root operator can be granted the capability
    /// without becoming root.
    Unscoped,
}

impl WorkspaceScope {
    /// The wire-name to pass on mantad calls, when scoped.
    /// `None` means "unscoped" (operator cross-tenant read).
    pub(crate) fn workspace_name(&self) -> Option<&str> {
        match self {
            Self::Bound { workspace_name } => Some(workspace_name.as_str()),
            Self::Unscoped => None,
        }
    }
}

/// Resolve the workspace scope a forwarder call should run
/// against, given the authenticated principal.
///
/// Scope resolution order:
///
/// 1. **Anonymous principal** → 412. No workspace can be
///    resolved without an identity.
/// 2. **Principal permitted [`crate::auth::Action::WorkspaceListAcrossTenants`]**
///    → [`WorkspaceScope::Unscoped`]. Root operators match by
///    the catch-all root permit rule; non-root principals can
///    be granted the capability explicitly. This branch fires
///    before the tenant-binding check so a fleet operator who
///    *also* happens to be tenant-bound still sees cross-tenant.
/// 3. **Tenant-bound principal with `storage_workspace_id = Some`**
///    → [`WorkspaceScope::Bound`].
/// 4. **Tenant-bound principal with `storage_workspace_id = None`**
///    → 412 `TenantStorageUnbound` with the init-storage hint.
/// 5. **Operator without a tenant binding and without the
///    cross-tenant capability** → 403.
pub async fn resolve_workspace_scope(
    auth: &crate::auth::AuthService,
    store: &Arc<dyn Store>,
    principal: &Principal,
) -> Result<WorkspaceScope, HttpError> {
    if matches!(principal, Principal::Anonymous) {
        return Err(HttpError::for_client_error(
            Some("PreconditionFailed".to_string()),
            ClientErrorStatusCode::PRECONDITION_FAILED,
            "anonymous principal cannot resolve a storage workspace".to_string(),
        ));
    }
    // Cross-tenant view: gate on the explicit action so the
    // capability can be granted to non-root fleet operators
    // without elevating them to root. Cedar's deny-by-default
    // means the existing root permit-all rule still satisfies
    // this for `is_root: true` principals.
    if auth
        .authorize(principal, crate::auth::Action::WorkspaceListAcrossTenants)
        .is_ok()
    {
        return Ok(WorkspaceScope::Unscoped);
    }
    match principal {
        Principal::Operator {
            tenant_id: Some(tenant_id),
            ..
        } => {
            let tenant = store
                .get_tenant(*tenant_id)
                .await
                .map_err(store_error_to_http)?;
            match tenant.storage_workspace_id {
                Some(workspace_uuid) => Ok(WorkspaceScope::Bound {
                    workspace_name: format!("t-{}", workspace_uuid.simple()),
                }),
                None => Err(HttpError::for_client_error(
                    Some("TenantStorageUnbound".to_string()),
                    ClientErrorStatusCode::PRECONDITION_FAILED,
                    format!(
                        "tenant {tenant_id} has no storage binding. an operator must register a \
                         default S3 cluster (`tcadm config set storage.default_s3_cluster_id`) \
                         before this tenant's members can use storage endpoints"
                    ),
                )),
            }
        }
        Principal::Operator {
            tenant_id: None, ..
        } => Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "operator has no tenant binding and lacks the workspace_list_across_tenants \
             capability; storage forwarders require either fleet permission or tenant membership"
                .to_string(),
        )),
        Principal::Anonymous => unreachable!("anonymous handled above"),
    }
}

/// Resolve a registered cluster id to (record, ready-to-call client).
///
/// Returns:
///
/// * `404` when the cluster id is unknown.
/// * `409 Conflict` when the cluster's surface is `Fs` or `Block`.
///   The forwarder endpoints implement only the S3 surface today;
///   refusing here keeps `mantafs` / `manta-block` registrations
///   visible in the registry without lighting up endpoints we
///   haven't implemented.
/// * `500` when the stored token is rejected by `MantadClient::new`.
pub async fn client_for(
    store: &Arc<dyn Store>,
    cluster_id: Uuid,
) -> Result<(StorageCluster, MantadClient), HttpError> {
    let cluster = store
        .get_storage_cluster(cluster_id)
        .await
        .map_err(store_error_to_http)?;
    if cluster.surface != StorageClusterSurface::S3 {
        return Err(HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            format!(
                "storage cluster {} has surface {:?}; forwarder endpoints implement only the S3 \
                 surface today",
                cluster.id, cluster.surface
            ),
        ));
    }
    let client = MantadClient::new(&cluster.endpoint, &cluster.admin_token).map_err(|e| {
        HttpError::for_internal_error(format!(
            "build mantad client for cluster {}: {e}",
            cluster.id
        ))
    })?;
    Ok((cluster, client))
}

/// Translate a [`MantadClientError`] into a paired tritond
/// [`AuditOutcome`] + HTTP [`HttpError`] for handlers that need to
/// emit a failure audit event AND return a useful HTTP response.
///
/// Returns (http_response, audit_outcome). The audit outcome
/// preserves the upstream status code as a `ClientError` /
/// `ServerError` distinction; the HTTP error keeps the same code
/// so admin-backend can render mantad's own validation failures
/// (409 on duplicate-name, 404 on missing bucket) instead of
/// flattening everything to 500.
pub fn mantad_error_to_http_audit(err: MantadClientError) -> (HttpError, AuditOutcome) {
    let outcome = match &err {
        MantadClientError::Status { status, body } => {
            // 4xx → ClientError; everything else (network failures
            // and 5xx) is a server-side problem from tritond's
            // point of view since we proxied a valid request and
            // the upstream blew up.
            if (400..500).contains(status) {
                AuditOutcome::ClientError {
                    code: *status,
                    message: format!("mantad upstream {status}: {body}"),
                }
            } else {
                AuditOutcome::ServerError {
                    message: format!("mantad upstream {status}: {body}"),
                }
            }
        }
        other => AuditOutcome::ServerError {
            message: format!("mantad client failure: {other}"),
        },
    };
    (mantad_error_to_http(err), outcome)
}

/// Translate a [`MantadClientError`] into an HTTP error that
/// preserves the upstream cause where possible.
///
/// Status codes that mantad returns get mapped 1:1 (4xx → 4xx, 5xx
/// → 5xx, default 502 for "we got a thing we don't recognise"); the
/// wrapped body is surfaced as the message so admin-backend gets a
/// useful failure string.
pub fn mantad_error_to_http(err: MantadClientError) -> HttpError {
    match err {
        MantadClientError::Status { status, body } => {
            let msg = format!("mantad upstream error: {status}: {body}");
            // Map well-known client codes; everything else flows
            // through as a 502 Bad Gateway since the failure is
            // upstream, not in tritond.
            match status {
                400 => HttpError::for_client_error(
                    Some("BadRequest".to_string()),
                    ClientErrorStatusCode::BAD_REQUEST,
                    msg,
                ),
                401 | 403 => HttpError::for_client_error(
                    Some("UpstreamForbidden".to_string()),
                    ClientErrorStatusCode::FORBIDDEN,
                    msg,
                ),
                404 => HttpError::for_client_error(
                    Some("NotFound".to_string()),
                    ClientErrorStatusCode::NOT_FOUND,
                    msg,
                ),
                409 => HttpError::for_client_error(
                    Some("Conflict".to_string()),
                    ClientErrorStatusCode::CONFLICT,
                    msg,
                ),
                _ => HttpError::for_internal_error(msg),
            }
        }
        // Reqwest / serde / config errors all collapse to 502:
        // tritond reached out and got something it can't parse or
        // couldn't reach the daemon at all.
        other => HttpError::for_internal_error(format!("mantad client failure: {other}")),
    }
}

fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            "storage cluster not found".to_string(),
        ),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        StoreError::Backend(msg) => HttpError::for_internal_error(msg),
        StoreError::FencedOut { saga_id } => HttpError::for_unavail(
            Some("FencedOut".to_string()),
            format!("saga {saga_id} adopted by another tritond instance; retry"),
        ),
        // placement-keyspace errors. The
        // storage-cluster handlers never write to the placement
        // keyspaces, so reaching any of these here would be a
        // programming error; surface as 500 with the underlying
        // reason so it lands in the operator-visible logs.
        StoreError::PinConflict { reason } => {
            HttpError::for_internal_error(format!("unexpected pin conflict: {reason}"))
        }
        StoreError::CapacityExhausted {
            server_uuid,
            reason,
        } => HttpError::for_internal_error(format!(
            "unexpected cn-capacity exhausted on {server_uuid}: {reason}"
        )),
        StoreError::AlreadyExists(msg) => HttpError::for_internal_error(msg),
        // ScanLimitExceeded is operator-visible at 400. The storage
        // handlers do not yet trip bounded-scan paths (storage-cluster
        // inventory is small), but the variant is reachable via the
        // shared Store trait so we map it explicitly rather than fall
        // through to a 500.
        StoreError::ScanLimitExceeded { cap, hint } => HttpError::for_client_error(
            Some("ScanLimitExceeded".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            format!("scan exceeded {cap} rows without completing; {hint}"),
        ),
    }
}

// ─────────────── mantad → tritond mirror conversions ────────────────
//
// These are plain functions, not `From` impls, because the orphan rule
// blocks `impl From<mantad_client::X> for tritond_api::Y` from
// living in this crate (both types are foreign to `tritond`). They
// could move into either upstream crate, but the cost would be a
// new cross-repo dependency edge for what is genuinely a tritond-
// internal adapter.

pub(crate) fn cluster_summary_from(s: mantad_client::ClusterSummary) -> StorageClusterSummary {
    StorageClusterSummary {
        version: s.version,
        primary: s.primary,
        this_node: s.this_node,
        replication_factor: s.replication_factor,
        nodes_total: s.nodes_total,
        nodes_alive: s.nodes_alive,
        buckets: s.buckets,
        total_blobs: s.total_blobs,
        total_bytes: s.total_bytes,
        racks: s.racks,
        query_ms: s.query_ms,
    }
}

pub(crate) fn node_from(n: mantad_client::Node) -> StorageNode {
    StorageNode {
        id: n.id,
        rack: n.rack,
        internal_url: n.internal_url,
        alive: n.alive,
        is_primary: n.is_primary,
        blobs: n.blobs,
        bytes: n.bytes,
        buckets: n.buckets,
        error: n.error,
    }
}

pub(crate) fn peer_from(p: mantad_client::PeerEntry) -> StoragePeerEntry {
    StoragePeerEntry {
        id: p.id,
        rack: p.rack,
        internal_url: p.internal_url,
    }
}

pub(crate) fn membership_from(m: mantad_client::Membership) -> StorageMembership {
    StorageMembership {
        version: m.version,
        peers: m.peers.into_iter().map(peer_from).collect(),
        auto_membership: m.auto_membership,
    }
}

pub(crate) fn bucket_from(b: mantad_client::Bucket) -> StorageBucket {
    StorageBucket {
        name: b.name,
        owner: b.owner,
        created_at: b.created_at,
        workspace: b.workspace,
        object_count: b.object_count,
        total_bytes: b.total_bytes,
    }
}

pub(crate) fn object_summary_from(o: mantad_client::types::ObjectSummary) -> StorageObjectSummary {
    StorageObjectSummary {
        key: o.key,
        size: o.size,
        etag: o.etag,
        content_type: o.content_type,
        last_modified: o.last_modified,
    }
}

pub(crate) fn objects_page_from(p: mantad_client::ObjectsPage) -> StorageObjectsPage {
    StorageObjectsPage {
        objects: p.objects.into_iter().map(object_summary_from).collect(),
        common_prefixes: p.common_prefixes,
        is_truncated: p.is_truncated,
        next_continuation_token: p.next_continuation_token,
    }
}

pub(crate) fn user_from(u: mantad_client::User) -> StorageUser {
    StorageUser {
        name: u.name,
        created_at: u.created_at,
        workspace: u.workspace,
    }
}

pub(crate) fn access_key_from(k: mantad_client::AccessKey) -> StorageAccessKey {
    StorageAccessKey {
        access_key_id: k.access_key_id,
        user: k.user,
        created_at: k.created_at,
        status: k.status,
        secret_access_key: k.secret_access_key,
        workspace: k.workspace,
    }
}

// And the reverse direction for request bodies the operator submits.

pub(crate) fn add_node_request_to(
    r: tritond_api::StorageAddNodeRequest,
) -> mantad_client::AddNodeRequest {
    mantad_client::AddNodeRequest {
        id: r.id,
        rack: r.rack,
        internal_url: r.internal_url,
    }
}

pub(crate) fn reweight_request_to(
    r: tritond_api::StorageReweightRequest,
) -> mantad_client::ReweightRequest {
    mantad_client::ReweightRequest { factor: r.factor }
}

pub(crate) fn create_bucket_request_to(
    r: tritond_api::StorageCreateBucketRequest,
) -> mantad_client::CreateBucketRequest {
    mantad_client::CreateBucketRequest {
        name: r.name,
        owner: r.owner,
        durability: r.durability,
    }
}

pub(crate) fn create_user_request_to(
    r: tritond_api::StorageCreateUserRequest,
) -> mantad_client::CreateUserRequest {
    mantad_client::CreateUserRequest { name: r.name }
}

pub(crate) fn scoped_access_key_request_to(
    r: tritond_api::StorageScopedAccessKeyRequest,
) -> mantad_client::types::ScopedAccessKeyRequest {
    mantad_client::types::ScopedAccessKeyRequest {
        scope: r
            .scope
            .into_iter()
            .map(|e| mantad_client::types::ScopeEntry {
                bucket: e.bucket,
                level: match e.level {
                    tritond_api::StorageScopeLevel::Read => mantad_client::types::ScopeLevel::Read,
                    tritond_api::StorageScopeLevel::ReadWrite => {
                        mantad_client::types::ScopeLevel::ReadWrite
                    }
                    tritond_api::StorageScopeLevel::Full => mantad_client::types::ScopeLevel::Full,
                },
                key_prefix: e.key_prefix,
            })
            .collect(),
    }
}

pub(crate) fn objects_query_to(q: tritond_api::StorageObjectsQuery) -> mantad_client::ObjectsQuery {
    mantad_client::ObjectsQuery {
        prefix: q.prefix,
        delimiter: q.delimiter,
        continuation_token: q.continuation_token,
        max_keys: q.max_keys,
    }
}

/// Mint a SigV4 query-string-authenticated URL the browser can hand
/// directly to mantad's S3 data plane.
///
/// `workspace` chooses which credential signs the URL:
///   * `Some(name)` — fetches the per-workspace presigner credential
///     from mantad via the in-process [`PresignerCache`] (Phase 2 of
///     the data-plane workspace gate). The resulting URL
///     authenticates on mantad as `CallerContext::Iam { workspace,
///     .. }`, so the gate fires and a malicious caller cannot
///     rewrite the URL to point at another workspace's bucket
///     without breaking the signature *and* failing the gate.
///   * `None` — falls back to the cluster-level root presigner
///     (`cluster.presigner_access_key_id` / `..._secret`). The
///     resulting URL authenticates as root on mantad and bypasses
///     the workspace gate. Used for operator / fleet-admin tooling
///     (e.g. unscoped presigns by a fleet operator).
///
/// Returns:
///
/// * `404` when the cluster id is unknown.
/// * `409 Conflict` when the cluster's `s3_endpoint` or presigner
///   credentials are not configured (the operator must call
///   `POST /v1/storage/clusters/{id}/presigner` first).
/// * `400 Bad Request` when sigv4 input validation rejects the
///   bucket/key/expires_secs (empty strings, expires_secs out of
///   range).
/// * `500` for any other sigv4 misconfiguration (bad endpoint URL).
///
/// Used by `presign_storage_cluster_object_{put,get}` and (in a
/// follow-up) the multipart per-part URL minter.
pub async fn mint_presigned_url(
    store: &Arc<dyn Store>,
    presigner_cache: &crate::presigner_cache::SharedPresignerCache,
    cluster_id: Uuid,
    workspace: Option<&str>,
    method: &str,
    bucket: &str,
    key: &str,
    expires_secs: u32,
) -> Result<PresignResponse, HttpError> {
    let cluster = store
        .get_storage_cluster(cluster_id)
        .await
        .map_err(store_error_to_http)?;
    if cluster.surface != StorageClusterSurface::S3 {
        return Err(HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            format!(
                "storage cluster {} has surface {:?}; presign endpoints implement only the S3 \
                 surface today",
                cluster.id, cluster.surface
            ),
        ));
    }
    let s3_endpoint = cluster.s3_endpoint.as_deref().ok_or_else(|| {
        HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            format!(
                "storage cluster {} has no s3_endpoint configured — set one via \
                 POST /v1/storage/clusters/{{id}}/presigner",
                cluster.id
            ),
        )
    })?;

    // Per-workspace path (preferred): the caller is bound to a
    // workspace, so sign with a workspace-scoped key. Cluster fallback
    // (Unscoped / root) only when `workspace` is None.
    let (access_key_id, secret_access_key) = if let Some(ws) = workspace {
        let (_cluster, client) = client_for(store, cluster_id).await?;
        let creds = presigner_cache
            .get_or_fetch(cluster_id, ws, &client)
            .await
            .map_err(|e| {
                let (http_err, _audit) = mantad_error_to_http_audit(e);
                http_err
            })?;
        (creds.access_key_id, creds.secret_access_key)
    } else {
        let ak = cluster
            .presigner_access_key_id
            .clone()
            .ok_or_else(|| presigner_unconfigured(cluster.id))?;
        let sk = cluster
            .presigner_secret_access_key
            .clone()
            .ok_or_else(|| presigner_unconfigured(cluster.id))?;
        (ak, sk)
    };

    let url = sigv4::presign_url(sigv4::PresignRequest {
        access_key_id: &access_key_id,
        secret_access_key: &secret_access_key,
        region: &cluster.default_region,
        endpoint: s3_endpoint,
        method,
        bucket,
        key,
        extra_query: &[],
        expires_secs,
        now: chrono::Utc::now(),
    })
    .map_err(|e| match e {
        sigv4::PresignError::Misconfigured(msg) => HttpError::for_client_error(
            Some("BadRequest".to_string()),
            ClientErrorStatusCode::BAD_REQUEST,
            format!("presign input invalid: {msg}"),
        ),
        sigv4::PresignError::BadEndpoint(msg) => HttpError::for_internal_error(format!(
            "cluster {} s3_endpoint is malformed: {msg}",
            cluster.id
        )),
    })?;
    Ok(PresignResponse {
        url,
        method: method.to_string(),
        headers: std::collections::HashMap::new(),
    })
}

fn presigner_unconfigured(id: Uuid) -> HttpError {
    HttpError::for_client_error(
        Some("Conflict".to_string()),
        ClientErrorStatusCode::CONFLICT,
        format!(
            "storage cluster {id} has no presigner configured — set one via \
             POST /v1/storage/clusters/{{id}}/presigner"
        ),
    )
}
