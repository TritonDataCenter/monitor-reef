// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Cross-scope resource access for ssh keys and images: the
//! single-source-of-truth visibility / deletability predicates a
//! wrong answer in which is a cross-tenant information leak. The
//! per-URL ssh-key/image read paths (`get_*`/`delete_*`) and the
//! instance-create reference check route through here.

use tritond_api::types::{Image, ImageScope, SshKey, SshKeyScope};
use tritond_store::{Store, StoreError};

use crate::auth::Principal;
use crate::principal::{principal_silo_id, principal_tenant_id, principal_user_id};

/// Single source of truth for cross-scope image visibility.
///
/// Returns `true` if `principal` can see `image`. Used by every
/// image read path (`get_image`, the per-scope list handlers) and
/// by the instance-create reference check; a wrong answer here
/// is a cross-tenant information leak.
///
/// Behaviour:
/// * Root operators (`is_root == true`) can see everything.
/// * `Public` is visible to every principal — authenticated *and*
///   anonymous (Cedar lets the latter through on the global
///   public-actions rule for `image_get`).
/// * `Silo { silo_id }` is visible iff the principal's cached
///   silo_id matches.
/// * `Tenant { tenant_id }` is visible iff the principal's
///   tenant_id matches.
/// * `Project { project_id }` resolves the project to its
///   tenant; visible iff `project.tenant_id == principal.tenant_id`.
///   (Phase 0 = "any tenant member sees any project image"; a
///   future slice can tighten to per-project membership.)
/// * `User { user_id }` is visible iff the principal's user_id
///   matches.
pub(crate) async fn image_visible_to(
    image: &Image,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    // Root sees everything regardless of scope.
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &image.scope {
        ImageScope::Public => Ok(true),
        ImageScope::Silo { silo_id } => Ok(principal_silo_id(principal) == Some(*silo_id)),
        ImageScope::Tenant { tenant_id } => Ok(principal_tenant_id(principal) == Some(*tenant_id)),
        ImageScope::Project { project_id } => {
            // Phase 0: any member of the project's tenant.
            let Some(my_tenant) = principal_tenant_id(principal) else {
                return Ok(false);
            };
            match store.get_project(*project_id).await {
                Ok(project) => Ok(project.tenant_id == my_tenant),
                Err(StoreError::NotFound) => Ok(false),
                Err(e) => Err(e),
            }
        }
        ImageScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // ImageScope is `#[non_exhaustive]`. New variants must
        // be classified explicitly in this gate; until then they
        // deny by default to avoid silent visibility bugs.
        _ => Ok(false),
    }
}

/// Stricter than [`image_visible_to`]: returns `true` if the
/// principal is allowed to delete `image`. The ownership rules
/// match the URL-vs-scope structure:
/// * `Public` — root only.
/// * `Silo` / `Tenant` / `Project` — any tenant member of the
///   resolved tenant (Phase 0); cross-tenant returns false.
/// * `User` — the owning user only.
pub(crate) async fn image_deletable_by(
    image: &Image,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &image.scope {
        // Public is operator turf.
        ImageScope::Public => Ok(false),
        // Silo / Tenant / Project follow the same visibility
        // gate as reads (Phase 0 = same-tenant access). A future
        // slice can split delete from read for these scopes.
        ImageScope::Silo { .. } | ImageScope::Tenant { .. } | ImageScope::Project { .. } => {
            image_visible_to(image, principal, store).await
        }
        ImageScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // Defensive default for future variants.
        _ => Ok(false),
    }
}

/// Single source of truth for cross-scope SSH-key visibility.
/// Mirrors [`image_visible_to`]. Used by every ssh-key read path
/// (`get_ssh_key`, the per-scope list handlers) and by the
/// instance-create reference check; a wrong answer here is a
/// cross-tenant information leak.
///
/// Behaviour:
/// * Root operators (`is_root == true`) can see everything.
/// * `Public` is visible to every principal — authenticated *and*
///   anonymous (Cedar lets the latter through on the global
///   public-actions rule for `ssh_key_get`).
/// * `Silo { silo_id }` is visible iff the principal's cached
///   silo_id matches.
/// * `Tenant { tenant_id }` is visible iff the principal's
///   tenant_id matches.
/// * `Project { project_id }` resolves the project to its
///   tenant; visible iff `project.tenant_id == principal.tenant_id`.
///   (Phase 0 = "any tenant member sees any project key"; a
///   future slice can tighten to per-project membership.)
/// * `User { user_id }` is visible iff the principal's user_id
///   matches.
pub(crate) async fn ssh_key_visible_to(
    key: &SshKey,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    // Root sees everything regardless of scope.
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &key.scope {
        SshKeyScope::Public => Ok(true),
        SshKeyScope::Silo { silo_id } => Ok(principal_silo_id(principal) == Some(*silo_id)),
        SshKeyScope::Tenant { tenant_id } => Ok(principal_tenant_id(principal) == Some(*tenant_id)),
        SshKeyScope::Project { project_id } => {
            // Phase 0: any member of the project's tenant.
            let Some(my_tenant) = principal_tenant_id(principal) else {
                return Ok(false);
            };
            match store.get_project(*project_id).await {
                Ok(project) => Ok(project.tenant_id == my_tenant),
                Err(StoreError::NotFound) => Ok(false),
                Err(e) => Err(e),
            }
        }
        SshKeyScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        // SshKeyScope is `#[non_exhaustive]`. New variants must
        // be classified explicitly in this gate; until then they
        // deny by default to avoid silent visibility bugs.
        _ => Ok(false),
    }
}

/// Stricter than [`ssh_key_visible_to`]: returns `true` if the
/// principal is allowed to delete `key`. The ownership rules
/// match the URL-vs-scope structure (same shape as
/// [`image_deletable_by`]):
/// * `Public` — root only.
/// * `Silo` / `Tenant` / `Project` — any tenant member of the
///   resolved tenant (Phase 0); cross-tenant returns false.
/// * `User` — the owning user only.
pub(crate) async fn ssh_key_deletable_by(
    key: &SshKey,
    principal: &Principal,
    store: &dyn Store,
) -> Result<bool, StoreError> {
    if let Principal::Operator { is_root: true, .. } = principal {
        return Ok(true);
    }
    match &key.scope {
        // Public is operator turf.
        SshKeyScope::Public => Ok(false),
        // Silo / Tenant / Project follow the same visibility
        // gate as reads (Phase 0 = same-tenant access).
        SshKeyScope::Silo { .. } | SshKeyScope::Tenant { .. } | SshKeyScope::Project { .. } => {
            ssh_key_visible_to(key, principal, store).await
        }
        SshKeyScope::User { user_id } => Ok(principal_user_id(principal) == Some(*user_id)),
        _ => Ok(false),
    }
}
