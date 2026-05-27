// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `meta` HTTP handlers — the four-scope layered metadata CRUD
//! (`IMDS_DESIGN.md` §4.1). See [`tritond_store`] for the validation
//! rules + storage layer; this module just translates the wire surface
//! to those calls and does the per-scope RBAC dance.

use crate::auth::{
    Action, authenticate_and_authorize, authenticate_and_authorize_in_silo,
    authenticate_and_authorize_in_tenant,
};
use crate::context::ApiContext;
use crate::error::store_error_to_http;

use chrono::Utc;
use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseDeleted, HttpResponseOk, Path, Query,
    RequestContext, TypedBody,
};
use tritond_api::{
    MetaEntry, MetaKeyQuery, MetaScopePath, SetGuestMetaRequest, SetMetaRequest, SetMetaResponse,
};
use tritond_store::{
    MetaError, MetaScope, MetaValue, Store, StoreError, default_guest_visible, validate_meta_entry,
};
use uuid::Uuid;

/// Map a [`MetaError`] (key/value/scope validation failure) to an
/// HTTP 400 with the error's `Display` message in the body.
fn meta_error_to_http(err: MetaError) -> HttpError {
    HttpError::for_client_error(None, ClientErrorStatusCode::BAD_REQUEST, err.to_string())
}

/// Resolve the `tenant_id` the Cedar `tenant-member-*` rule should
/// match for a tenant-scoped/project-scoped/instance-scoped metadata
/// request. The `Silo` scope is handled separately by the caller (it
/// uses the silo-member Cedar rule instead).
async fn tenant_id_for_scope(
    store: &dyn Store,
    scope: MetaScope,
    scope_id: Uuid,
) -> Result<Uuid, HttpError> {
    match scope {
        MetaScope::Tenant => Ok(scope_id),
        MetaScope::Project => match store.get_project(scope_id).await {
            Ok(p) => Ok(p.tenant_id),
            Err(e) => Err(store_error_to_http(e)),
        },
        MetaScope::Instance => match store.get_instance(scope_id).await {
            Ok(i) => Ok(i.tenant_id),
            Err(e) => Err(store_error_to_http(e)),
        },
        MetaScope::Silo => Err(HttpError::for_internal_error(
            "tenant_id_for_scope called with Silo".to_string(),
        )),
    }
}

/// Authenticate + authorize the request for one metadata operation at
/// the given `(scope, scope_id)`. Returns once Cedar has allowed; the
/// caller proceeds with the store call.
async fn authorize_meta(
    rqctx: &RequestContext<ApiContext>,
    scope: MetaScope,
    scope_id: Uuid,
    action: Action,
) -> Result<(), HttpError> {
    let ctx = rqctx.context();
    if matches!(scope, MetaScope::Silo) {
        authenticate_and_authorize_in_silo(
            rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, scope_id,
        )
        .await
        .map(|_| ())
    } else {
        let tenant_id = tenant_id_for_scope(ctx.store.as_ref(), scope, scope_id).await?;
        authenticate_and_authorize_in_tenant(
            rqctx, &ctx.auth, &ctx.audit, &ctx.store, action, tenant_id,
        )
        .await
        .map(|_| ())
    }
}

pub(crate) async fn list_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<MetaScopePath>,
) -> Result<HttpResponseOk<Vec<MetaEntry>>, HttpError> {
    let MetaScopePath { scope, scope_id } = path.into_inner();
    authorize_meta(&rqctx, scope, scope_id, Action::MetaList).await?;
    let ctx = rqctx.context();
    match ctx.store.list_meta(scope, scope_id).await {
        Ok(rows) => Ok(HttpResponseOk(
            rows.into_iter()
                .map(|(key, value)| MetaEntry { key, value })
                .collect(),
        )),
        Err(e) => Err(store_error_to_http(e)),
    }
}

pub(crate) async fn get_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<MetaScopePath>,
    query: Query<MetaKeyQuery>,
) -> Result<HttpResponseOk<MetaEntry>, HttpError> {
    let MetaScopePath { scope, scope_id } = path.into_inner();
    let key = query.into_inner().key;
    authorize_meta(&rqctx, scope, scope_id, Action::MetaGet).await?;
    let ctx = rqctx.context();
    match ctx.store.get_meta(scope, scope_id, &key).await {
        Ok(value) => Ok(HttpResponseOk(MetaEntry { key, value })),
        Err(e) => Err(store_error_to_http(e)),
    }
}

pub(crate) async fn set_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<MetaScopePath>,
    query: Query<MetaKeyQuery>,
    body: TypedBody<SetMetaRequest>,
) -> Result<HttpResponseOk<SetMetaResponse>, HttpError> {
    let MetaScopePath { scope, scope_id } = path.into_inner();
    let key = query.into_inner().key;
    let req = body.into_inner();
    authorize_meta(&rqctx, scope, scope_id, Action::MetaSet).await?;

    let guest_writable = req.guest_writable.unwrap_or(false);
    // Re-validate defensively at the handler edge; the store trusts
    // what it's handed.
    validate_meta_entry(scope, &key, &req.value, guest_writable).map_err(meta_error_to_http)?;

    let principal_id = rqctx.request_id.clone();
    // Authenticated identity isn't easily threaded through here yet;
    // record the request-id as `updated_by` for now (audit tracks the
    // full principal separately). A follow-up will plumb the principal
    // uuid through the helper return value.
    let updated_by = format!("api:{principal_id}");

    let entry = MetaValue {
        value: req.value,
        guest_visible: req
            .guest_visible
            .unwrap_or_else(|| default_guest_visible(scope, &key)),
        guest_writable,
        updated_by,
        updated_at: Utc::now(),
    };

    let ctx = rqctx.context();
    match ctx
        .store
        .set_meta(scope, scope_id, &key, entry.clone())
        .await
    {
        Ok(generation) => Ok(HttpResponseOk(SetMetaResponse {
            entry: MetaEntry { key, value: entry },
            generation,
        })),
        Err(e) => Err(store_error_to_http(e)),
    }
}

pub(crate) async fn delete_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<MetaScopePath>,
    query: Query<MetaKeyQuery>,
) -> Result<HttpResponseDeleted, HttpError> {
    let MetaScopePath { scope, scope_id } = path.into_inner();
    let key = query.into_inner().key;
    authorize_meta(&rqctx, scope, scope_id, Action::MetaDelete).await?;
    let ctx = rqctx.context();
    match ctx.store.delete_meta(scope, scope_id, &key).await {
        Ok(_gen) => Ok(HttpResponseDeleted()),
        Err(e) => Err(store_error_to_http(e)),
    }
}

pub(crate) async fn get_instance_realized_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::InstanceRealizedMetaPath>,
) -> Result<HttpResponseOk<Vec<tritond_api::RealizedMetaEntry>>, HttpError> {
    let instance_id = path.into_inner().instance_id;
    // RBAC: anyone in the instance's owning tenant can read the
    // realized view (mirrors the per-key MetaList/MetaGet grants).
    authorize_meta(&rqctx, MetaScope::Instance, instance_id, Action::MetaList).await?;
    realized_meta_response(&rqctx, instance_id).await
}

/// Agent-facing variant: same body shape as
/// [`get_instance_realized_meta`], but the caller is a CN-bound
/// agent API key (matches the auth shape of `/v1/agent/peer` /
/// `/v1/agent/blueprints`). tritonagent's IMDS daemon calls this
/// to answer guest IMDSv2 requests — the tenant-member Cedar rule
/// can't authorize a CN-bound key. The dataplane already enforces
/// locality: the IMDS request arrives via the guest's vnic on
/// this CN.
pub(crate) async fn agent_get_instance_realized_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::InstanceRealizedMetaPath>,
) -> Result<HttpResponseOk<Vec<tritond_api::RealizedMetaEntry>>, HttpError> {
    let instance_id = path.into_inner().instance_id;
    let ctx = rqctx.context();
    let _principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;
    realized_meta_response(&rqctx, instance_id).await
}

/// Build the affected-instances reverse-index for one (scope, key)
/// pair. Walks every instance under the request scope, computes its
/// realized view, and partitions on whether the request scope's
/// value wins or a narrower scope shadows it.
///
/// Cost: O(N) realized-view builds, where N is the number of
/// instances under the scope. For a single-CN dev silo this is a few
/// dozen calls; at fleet scale it'll need an index (deferred per
/// `IMDS_DESIGN.md` — the design uses per-scope generation counters
/// rather than a reverse-index, but the affected-instances list is
/// the load-bearing affordance for the operator console's IMDS
/// authoring surface and an on-demand scan is correct, just not
/// yet optimised).
pub(crate) async fn get_affected_instances(
    rqctx: RequestContext<ApiContext>,
    path: Path<MetaScopePath>,
    query: Query<MetaKeyQuery>,
) -> Result<HttpResponseOk<tritond_api::AffectedInstancesResponse>, HttpError> {
    let MetaScopePath { scope, scope_id } = path.into_inner();
    let key = query.into_inner().key;
    // Validation upfront — return 400 for an obviously bad key rather
    // than scanning the whole scope just to discover the key is
    // syntactically illegal.
    if let Err(e) = tritond_store::validate_meta_key(scope, &key) {
        return Err(meta_error_to_http(e));
    }
    // Same RBAC as `list_meta` — anyone who can read the scope's
    // metadata can ask "where does this value flow."
    authorize_meta(&rqctx, scope, scope_id, Action::MetaList).await?;
    let ctx = rqctx.context();
    let store = ctx.store.as_ref();

    // Pull the value at the request scope first; absent → empty wins
    // and shadowed lists (the operator is asking about a key that
    // isn't authored here, so nothing flows from here either way).
    let value_at_scope = match store.get_meta(scope, scope_id, &key).await {
        Ok(v) => Some(v),
        Err(StoreError::NotFound) => None,
        Err(e) => return Err(store_error_to_http(e)),
    };

    // Collect every instance under the request scope. For Silo and
    // Tenant we need to fan out through the tenancy tree; for
    // Project there is one list call; for Instance the "list" is the
    // singleton {scope_id}.
    let instances: Vec<tritond_store::Instance> = match scope {
        MetaScope::Silo => {
            let tenants = store
                .list_tenants_in_silo(scope_id)
                .await
                .map_err(store_error_to_http)?;
            let mut out = Vec::new();
            for t in tenants {
                let projects = store
                    .list_projects_in_tenant(t.id)
                    .await
                    .map_err(store_error_to_http)?;
                for p in projects {
                    let insts = store
                        .list_instances_in_project(p.id)
                        .await
                        .map_err(store_error_to_http)?;
                    out.extend(insts);
                }
            }
            out
        }
        MetaScope::Tenant => {
            let projects = store
                .list_projects_in_tenant(scope_id)
                .await
                .map_err(store_error_to_http)?;
            let mut out = Vec::new();
            for p in projects {
                let insts = store
                    .list_instances_in_project(p.id)
                    .await
                    .map_err(store_error_to_http)?;
                out.extend(insts);
            }
            out
        }
        MetaScope::Project => store
            .list_instances_in_project(scope_id)
            .await
            .map_err(store_error_to_http)?,
        MetaScope::Instance => match store.get_instance(scope_id).await {
            Ok(i) => vec![i],
            Err(e) => return Err(store_error_to_http(e)),
        },
    };

    let mut wins = Vec::new();
    let mut shadowed = Vec::new();

    // The provenance kind that the request scope produces — anything
    // matching this in an instance's realized view means "this scope
    // wins for this instance." Anything else (a narrower scope or
    // `System`) means "this scope is shadowed."
    let want_provenance = match scope {
        MetaScope::Silo => tritond_store::MetaProvenance::Silo,
        MetaScope::Tenant => tritond_store::MetaProvenance::Tenant,
        MetaScope::Project => tritond_store::MetaProvenance::Project,
        MetaScope::Instance => tritond_store::MetaProvenance::Instance,
    };

    for inst in instances {
        let view = match crate::build_instance_realized_view(store, inst.id).await {
            Ok(v) => v,
            Err(StoreError::NotFound) => continue,
            Err(e) => return Err(store_error_to_http(e)),
        };
        let inst_ref = tritond_api::AffectedInstanceRef {
            id: inst.id,
            tenant_id: inst.tenant_id,
            project_id: inst.project_id,
            name: inst.name.clone(),
        };
        match view.get(&key) {
            Some((_value, prov)) if *prov == want_provenance => {
                wins.push(inst_ref);
            }
            Some((_value, prov)) => {
                shadowed.push(tritond_api::ShadowedInstance {
                    instance: inst_ref,
                    winner_scope: *prov,
                });
            }
            None => {
                // Key is not in this instance's realized view at all
                // — neither the request scope nor any narrower scope
                // has it. Don't list this instance: it's irrelevant
                // to the operator's "if I edit here, what changes?"
                // question. (We could surface a third "unaware"
                // bucket but the UI doesn't need it: an empty wins
                // + empty shadowed already tells the operator the
                // edit affects nothing under this scope.)
            }
        }
    }

    Ok(HttpResponseOk(tritond_api::AffectedInstancesResponse {
        value_at_scope,
        wins,
        shadowed,
    }))
}

async fn realized_meta_response(
    rqctx: &RequestContext<ApiContext>,
    instance_id: Uuid,
) -> Result<HttpResponseOk<Vec<tritond_api::RealizedMetaEntry>>, HttpError> {
    let ctx = rqctx.context();
    match crate::build_instance_realized_view(ctx.store.as_ref(), instance_id).await {
        Ok(view) => Ok(HttpResponseOk(
            view.entries
                .into_iter()
                .map(|(key, (value, from))| tritond_api::RealizedMetaEntry { key, value, from })
                .collect(),
        )),
        Err(e) => Err(store_error_to_http(e)),
    }
}

pub(crate) async fn agent_set_instance_guest_meta(
    rqctx: RequestContext<ApiContext>,
    path: Path<tritond_api::InstanceRealizedMetaPath>,
    query: Query<MetaKeyQuery>,
    body: TypedBody<SetGuestMetaRequest>,
) -> Result<HttpResponseOk<SetMetaResponse>, HttpError> {
    let instance_id = path.into_inner().instance_id;
    let key = query.into_inner().key;
    let req = body.into_inner();
    let ctx = rqctx.context();

    // Agent (CN-bound) authorization. Tenant-member Cedar can't
    // authorize a CN-bound API key, so this endpoint exists in
    // parallel to `set_meta` with agent-scoped auth instead.
    let _principal = authenticate_and_authorize(
        &rqctx,
        &ctx.auth,
        &ctx.audit,
        &ctx.store,
        Action::AgentBlueprint,
    )
    .await?;

    // Walk the realized view to find which scope currently owns
    // this key. The agent can update **existing** entries that the
    // operator has marked `guest_writable: true` at *any* scope
    // (silo / tenant / project / instance) — the write lands at the
    // scope where the entry already lives, so writes propagate to
    // every sibling instance sharing that scope.
    //
    // Update-only semantics: no create path. The operator must
    // pre-create the key (with `guest_writable: true`) at the scope
    // they want guests writing to.
    let view = crate::build_instance_realized_view(ctx.store.as_ref(), instance_id)
        .await
        .map_err(store_error_to_http)?;
    let Some((existing_value, provenance)) = view.entries.get(&key) else {
        return Err(HttpError::for_client_error(
            None,
            ClientErrorStatusCode::NOT_FOUND,
            format!(
                "agent writeback rejected: no realized entry at key `{key}` for instance {instance_id}"
            ),
        ));
    };
    if !existing_value.guest_writable {
        return Err(HttpError::for_client_error(
            None,
            ClientErrorStatusCode::FORBIDDEN,
            format!(
                "agent writeback rejected: key `{key}` is not guest_writable at the scope it lives in"
            ),
        ));
    }

    // Provenance tells us which scope the entry came from; resolve
    // the corresponding scope_id for the calling instance's
    // hierarchy. `System` entries are computed (not stored) so
    // there's nothing to write back to.
    //
    // Silo writeback is explicitly forbidden: an instance must never
    // be able to mutate silo-scope metadata even if a stale
    // silo-scope `guest_writable: true` exists (the
    // `meta_key_guest_writable_allowed` rule prevents new silo
    // writable keys, but old data could exist from before the rule
    // tightened). Allowed targets are tenant / project / instance,
    // all resolved from the calling instance's own hierarchy —
    // never another tenant's, never above the silo line.
    let (scope, scope_id) = match provenance {
        tritond_store::MetaProvenance::Silo => {
            return Err(HttpError::for_client_error(
                None,
                ClientErrorStatusCode::FORBIDDEN,
                format!(
                    "agent writeback rejected: key `{key}` lives at silo scope; instances cannot write silo-scope metadata"
                ),
            ));
        }
        tritond_store::MetaProvenance::Tenant => {
            let instance = ctx
                .store
                .get_instance(instance_id)
                .await
                .map_err(store_error_to_http)?;
            (MetaScope::Tenant, instance.tenant_id)
        }
        tritond_store::MetaProvenance::Project => {
            let instance = ctx
                .store
                .get_instance(instance_id)
                .await
                .map_err(store_error_to_http)?;
            (MetaScope::Project, instance.project_id)
        }
        tritond_store::MetaProvenance::Instance => (MetaScope::Instance, instance_id),
        tritond_store::MetaProvenance::System => {
            return Err(HttpError::for_client_error(
                None,
                ClientErrorStatusCode::FORBIDDEN,
                format!("key `{key}` is a computed System entry and not writable"),
            ));
        }
    };

    validate_meta_entry(scope, &key, &req.value, existing_value.guest_writable)
        .map_err(meta_error_to_http)?;

    let principal_id = rqctx.request_id.clone();
    let updated_by = format!("agent:{principal_id}");
    let entry = MetaValue {
        value: req.value,
        // Preserve the operator-set flags. The guest can never
        // change visibility / writability — those gate-keep what
        // the guest is allowed to touch in the first place.
        guest_visible: existing_value.guest_visible,
        guest_writable: existing_value.guest_writable,
        updated_by,
        updated_at: Utc::now(),
    };

    match ctx
        .store
        .set_meta(scope, scope_id, &key, entry.clone())
        .await
    {
        Ok(generation) => Ok(HttpResponseOk(SetMetaResponse {
            entry: MetaEntry { key, value: entry },
            generation,
        })),
        Err(e) => Err(store_error_to_http(e)),
    }
}

// The unused `StoreError` import keeps the implicit conversion-via-
// `store_error_to_http` path explicit at module scope; silence the
// dead-code lint until we use it directly.
#[allow(dead_code)]
fn _store_error_marker(_e: StoreError) {}
