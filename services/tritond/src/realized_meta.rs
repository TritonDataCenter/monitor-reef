// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Assemble the full [`RealizedView`] for one instance.
//!
//! The pure pieces (merge precedence, computed system keys, the
//! provenance-tagged view) live in `tritond-store`
//! ([`RealizedMeta::merge`], [`computed_metadata`],
//! [`RealizedView::build`]). This module just walks the scope chain
//! (instance → project → tenant → silo), pulls the four `list_meta`
//! maps, resolves the records the computed keys need (primary NIC,
//! VPC, image, SSH public keys), and hands them to
//! `RealizedView::build`. This is the load-bearing call behind the
//! `/v1/meta/instance/{id}/realized` API and the
//! `/triton/dynamic/realized` IMDS endpoint (the latter filtered to
//! `guest_visible`).
//!
//! See `IMDS_DESIGN.md` §1.5.

use tritond_store::{MetaScope, RealizedView, Store, StoreError, computed_metadata};
use uuid::Uuid;

/// Build the full realized view for `instance_id`: the precedence
/// merge of the four stored scopes plus the computed system keys, each
/// leaf provenance-tagged. Caller filters to `guest_visible()` when
/// serving the IMDS path.
///
/// The instance must exist; any missing supporting record (NIC / VPC /
/// image / SSH key) is treated as "not yet available" — the computed
/// keys degrade rather than the function failing. The four
/// `list_meta` calls always succeed (a scope with no metadata yields
/// an empty list).
pub async fn build_instance_realized_view(
    store: &dyn Store,
    instance_id: Uuid,
) -> Result<RealizedView, StoreError> {
    let instance = store.get_instance(instance_id).await?;

    // The four stored scopes. tenant_id and project_id come straight
    // off the instance; silo_id is one hop up through the tenant.
    let tenant = store.get_tenant(instance.tenant_id).await?;
    let silo_meta = store
        .list_meta(MetaScope::Silo, tenant.silo_id)
        .await
        .unwrap_or_default();
    let tenant_meta = store
        .list_meta(MetaScope::Tenant, instance.tenant_id)
        .await
        .unwrap_or_default();
    let project_meta = store
        .list_meta(MetaScope::Project, instance.project_id)
        .await
        .unwrap_or_default();
    let instance_meta = store
        .list_meta(MetaScope::Instance, instance_id)
        .await
        .unwrap_or_default();

    // Records the computed keys want. Each one is best-effort.
    let nics = store
        .list_nics_for_instance(instance_id)
        .await
        .unwrap_or_default();
    let primary_nic = nics
        .iter()
        .find(|n| n.subnet_id == instance.primary_subnet_id)
        .or_else(|| nics.first());
    let vpc = match primary_nic {
        Some(n) => store.get_vpc(n.vpc_id).await.ok(),
        None => None,
    };
    let image = store.get_image(instance.image_id).await.ok();

    // Resolve injected SSH public keys (any that the catalog has lost
    // are skipped silently — the realized view degrades, not fails).
    let mut ssh_pubkeys: Vec<String> = Vec::with_capacity(instance.ssh_key_ids.len());
    for key_id in &instance.ssh_key_ids {
        if let Ok(k) = store.get_ssh_key(*key_id).await {
            ssh_pubkeys.push(k.public_key);
        }
    }

    let computed = computed_metadata(
        &instance,
        primary_nic,
        vpc.as_ref(),
        image.as_ref(),
        &ssh_pubkeys,
    );

    Ok(RealizedView::build(
        &silo_meta,
        &tenant_meta,
        &project_meta,
        &instance_meta,
        &computed,
    ))
}
