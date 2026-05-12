// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-CN credential minting and the CN-binding enforcement checks
//! that gate agent-facing endpoints, plus the shared mantad
//! node-membership forwarding body.

use dropshot::{ClientErrorStatusCode, HttpError, HttpResponseOk, Path, RequestContext};
use uuid::Uuid;

use tritond_api::StorageClusterNodePath;
use tritond_api::types::{NetworkResourceId, ProvisioningJob, RealizerId, StorageMembership};
use tritond_audit::Outcome as AuditOutcome;
use tritond_auth::{ConsoleTicketKey, generate_api_key};
use tritond_store::{ApiKey, ApiKeyScope, Cn, Store};

use crate::auth::{Action, authenticate_and_authorize, require_authenticated};
use crate::context::ApiContext;
use crate::error::{bad_request, not_found, store_error_to_http};
use crate::validate::parse_request_id;

/// Shared body for the parameter-less, mantad-side mutation endpoints
/// that take only a node id (drain / undrain). Centralises the auth
/// + audit pattern so each handler is a 3-line wrapper.
pub(crate) async fn forward_node_membership_op<F, Fut>(
    rqctx: &RequestContext<ApiContext>,
    path: Path<StorageClusterNodePath>,
    action: Action,
    op: F,
) -> Result<HttpResponseOk<StorageMembership>, HttpError>
where
    F: FnOnce(mantad_client::MantadClient, u32) -> Fut,
    Fut: std::future::Future<
            Output = Result<mantad_client::Membership, mantad_client::MantadClientError>,
        >,
{
    let ctx = rqctx.context();
    let principal =
        authenticate_and_authorize(rqctx, &ctx.auth, &ctx.audit, &ctx.store, action).await?;
    let request_id = parse_request_id(rqctx);
    let p = path.into_inner();
    let (_, client) = crate::storage::client_for(&ctx.store, p.id).await?;
    let payload = serde_json::json!({ "node_id": p.node_id });
    match op(client, p.node_id).await {
        Ok(m) => {
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    AuditOutcome::Success {
                        resource: Some(format!("StorageCluster::\"{}\"", p.id)),
                    },
                    payload,
                )
                .await;
            Ok(HttpResponseOk(crate::storage::membership_from(m)))
        }
        Err(e) => {
            let (http_err, audit_outcome) = crate::storage::mantad_error_to_http_audit(e);
            ctx.audit
                .record_mutation(
                    &principal,
                    action,
                    request_id,
                    Some(format!("StorageCluster::\"{}\"", p.id)),
                    audit_outcome,
                    payload,
                )
                .await;
            Err(http_err)
        }
    }
}

/// Mint a fresh per-CN API key, persist it (with bound_to_cn set
/// to the CN's server_uuid + scope = Agent), and atomically wire
/// it onto the Cn record via `approve_cn`. On error, audits the
/// failure with the supplied principal + request_id and returns
/// a 500.
///
/// The CN's "owning user" is the principal who triggered the
/// approval. For the operator approval path that's the operator's
/// user_id; for the auto-approve path (anonymous) we fall back to
/// the bootstrap root operator's id so the key has a real owner
/// in the existing per-user list. (A future slice may give CNs
/// their own User-equivalent.)
pub(crate) async fn mint_and_attach_cn_credential(
    ctx: &ApiContext,
    principal: &crate::auth::Principal,
    request_id: Option<Uuid>,
    cn: &Cn,
) -> Result<Cn, HttpError> {
    let owner_user_id = match require_authenticated(principal.clone()) {
        Ok((uid, _)) => uid,
        Err(_) => {
            ctx.store
                .get_user_by_username(crate::bootstrap::ROOT_USERNAME)
                .await
                .map_err(store_error_to_http)?
                .id
        }
    };

    let material = generate_api_key()
        .await
        .map_err(|e| HttpError::for_internal_error(format!("generate api key: {e}")))?;
    let key_id = Uuid::new_v4();
    let record = ApiKey {
        id: key_id,
        user_id: owner_user_id,
        description: format!("agent: cn {}", cn.server_uuid),
        lookup_id: material.lookup_id.clone(),
        hash: material.hash,
        scope: ApiKeyScope::Agent,
        bound_to_cn: Some(cn.server_uuid),
        created_at: chrono::Utc::now(),
    };
    ctx.store
        .create_api_key(record)
        .await
        .map_err(store_error_to_http)?;

    // Per-CN console-ticket signing key. Generated here so it lands
    // on the Cn record in the same atomic `approve_cn` update as the
    // bound API key + pending plaintext; the agent retrieves it
    // (hex-encoded) alongside the API key on its first
    // long-poll-after-approval. Deliberately a distinct key from the
    // operator-login JwtKey: a compromised CN must not be able to
    // forge operator access tokens.
    let console_ticket_key = ConsoleTicketKey::generate();
    let console_ticket_key_bytes = *console_ticket_key.bytes();

    let now = chrono::Utc::now();
    let updated = match ctx
        .store
        .approve_cn(
            cn.server_uuid,
            key_id,
            material.plaintext,
            console_ticket_key_bytes,
            now,
        )
        .await
    {
        Ok(updated) => updated,
        Err(e) => {
            // Key created but approve failed. Audit so an operator
            // can clean up the orphan key.
            ctx.audit
                .record_mutation(
                    principal,
                    Action::CnApprove,
                    request_id,
                    Some(format!("Cn::\"{}\"", cn.server_uuid)),
                    AuditOutcome::ServerError {
                        message: format!("orphaned api key {key_id}: {e}"),
                    },
                    serde_json::json!({
                        "server_uuid": cn.server_uuid,
                        "orphaned_api_key_id": key_id,
                    }),
                )
                .await;
            return Err(store_error_to_http(e));
        }
    };
    Ok(updated)
}

/// 403 if the job's `claimed_by` (which the agent set when
/// it claimed) doesn't match the bound key's CN. Used by
/// `agent_complete_job` and `agent_job_blueprint` so a bound
/// key for CN-A can't operate on a job claimed by CN-B.
pub(crate) fn enforce_job_belongs_to_bound_cn(
    job: &ProvisioningJob,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    // `claimed_by` is free-text on the wire today; bound agents
    // are required to set it to their server_uuid string.
    let Some(ref claimed_by) = job.claimed_by else {
        return Err(HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "job has no claimer; bound key cannot operate on it".to_string(),
        ));
    };
    let claimed_uuid = Uuid::parse_str(claimed_by).map_err(|_| {
        HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "job claimed_by is not a uuid; bound key cannot match it".to_string(),
        )
    })?;
    crate::auth::enforce_cn_binding(Some(bound_cn), claimed_uuid)
}

/// 403 if a CN-bound key tries to write a realization row for a
/// different CN realizer. Edge-cluster realization rows are reported
/// by a tritonagent running on an edge CN, so the caller must still
/// be CN-bound but the row key is the edge-cluster id.
pub(crate) fn enforce_realizer_belongs_to_bound_cn(
    realizer: RealizerId,
    bound_cn: Uuid,
) -> Result<(), HttpError> {
    match realizer {
        RealizerId::Cn { id } => crate::auth::enforce_cn_binding(Some(bound_cn), id),
        RealizerId::EdgeCluster { .. } => Ok(()),
        _ => Err(bad_request("unsupported realizer kind")),
    }
}

pub(crate) async fn ensure_realization_resource_exists(
    store: &dyn Store,
    resource: NetworkResourceId,
) -> Result<(), HttpError> {
    match resource {
        NetworkResourceId::Vpc { id } => store.get_vpc(id).await.map(|_| ()),
        NetworkResourceId::Subnet { id } => store.get_subnet(id).await.map(|_| ()),
        NetworkResourceId::RouteTable { id } => store.get_route_table(id).await.map(|_| ()),
        NetworkResourceId::Route { id } => store.get_route(id).await.map(|_| ()),
        NetworkResourceId::NatGateway { id } => store.get_nat_gateway(id).await.map(|_| ()),
        NetworkResourceId::FloatingIp { id } => store.get_floating_ip(id).await.map(|_| ()),
        NetworkResourceId::EdgeCluster { id } => store.get_edge_cluster(id).await.map(|_| ()),
        NetworkResourceId::SecurityGroup { .. }
        | NetworkResourceId::SecurityGroupRule { .. }
        | NetworkResourceId::NicSecurityGroupAttachment { .. } => return Err(not_found()),
        _ => return Err(not_found()),
    }
    .map_err(store_error_to_http)
}
