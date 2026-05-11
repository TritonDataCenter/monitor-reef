// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Request-edge validation helpers (sha256, openssh public keys,
//! request-id parsing, VPC parentage checks).

use dropshot::{HttpError, RequestContext};
use uuid::Uuid;

use crate::error::{not_found, store_error_to_http};
use tritond_store::Store;

/// Validate an image's `sha256` field — must be exactly 64 lowercase
/// hex characters.
pub(crate) fn validate_sha256(s: &str) -> Result<(), String> {
    if s.len() != 64 {
        return Err(format!("sha256 must be 64 hex chars (got {})", s.len()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err("sha256 must be lowercase hex (0-9, a-f)".to_string());
    }
    Ok(())
}

/// Parse an inbound openssh public-key string and return its
/// canonical SHA-256 fingerprint. Returns `Err` with a user-facing
/// message on parse failure (mapped to 400 by callers).
pub(crate) fn parse_ssh_public_key(public_key: &str) -> Result<String, String> {
    let parsed = ssh_key::PublicKey::from_openssh(public_key.trim())
        .map_err(|e| format!("invalid openssh public key: {e}"))?;
    Ok(parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string())
}

pub(crate) fn parse_request_id<T>(rqctx: &RequestContext<T>) -> Option<Uuid>
where
    T: dropshot::ServerContext,
{
    Uuid::parse_str(&rqctx.request_id).ok()
}

/// Verify that `vpc_id` exists and that its `tenant_id`+`project_id`
/// match the URL path. Used by the DHCP endpoints (and any future
/// VPC-scoped resource) to surface cross-tenant probes as 404 rather
/// than leak existence via a 403/409.
pub(crate) async fn check_vpc_parentage(
    store: &dyn Store,
    vpc_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
) -> Result<(), HttpError> {
    let vpc = store.get_vpc(vpc_id).await.map_err(store_error_to_http)?;
    if vpc.tenant_id != tenant_id || vpc.project_id != project_id {
        return Err(not_found());
    }
    Ok(())
}
