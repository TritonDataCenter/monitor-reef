// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! In-memory cache of per-workspace presigner credentials.
//!
//! Phase 2 of the S3 data-plane workspace gate. Tritond signs
//! presigned S3 URLs with a non-root IAM access key whose `workspace`
//! field matches the calling tenant's binding, so mantad's data plane
//! resolves the request as `CallerContext::Iam { workspace, .. }` and
//! the Phase 1 gate fires. The key (id + secret) is minted on demand
//! via mantad's `POST /admin/v1/workspaces/{name}/presigner` admin
//! route, which is idempotent — every call returns the active key for
//! the same `presigner-{workspace}` system user.
//!
//! The secret is **not** persisted in tritond's FDB. The threat model:
//! tritond's FDB is the tenant-data plane and a high-value compromise
//! target; copying the per-workspace SigV4 secret into it would
//! expand the blast radius of an FDB compromise from "tenant metadata"
//! to "tenant metadata + forge presigned URLs for every workspace."
//! Mantad already holds the canonical IAM secret on its own meta
//! store; tritond just keeps a process-local cache and re-fetches on
//! cold misses or after the TTL expires.
//!
//! Cache invalidation:
//! - 5-minute TTL on each entry, refreshed transparently on the next
//!   sign request after expiry.
//! - Explicit eviction by `drop_silo_tenant_storage` when a tenant's
//!   workspace is archived. In-flight pre-eviction signed URLs cannot
//!   survive past mantad's next `head_access_key` because the
//!   workspace delete cascades the system user's row.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mantad_client::{MantadClient, MantadClientError};
use tokio::sync::RwLock;
use uuid::Uuid;

const TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct PresignerCreds {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    creds: PresignerCreds,
    fetched_at: Instant,
}

#[derive(Default)]
pub struct PresignerCache {
    entries: RwLock<HashMap<(Uuid, String), CachedEntry>>,
}

impl PresignerCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch the presigner credentials for `(cluster_id, workspace)`.
    /// Cache hit if the entry is fresher than [`TTL`]; otherwise call
    /// the mantad admin API and update the cache.
    ///
    /// `client` is the mantad client already configured for
    /// `cluster_id`'s admin endpoint and bearer token — callers
    /// typically construct it via `crate::storage::client_for`.
    pub async fn get_or_fetch(
        &self,
        cluster_id: Uuid,
        workspace: &str,
        client: &MantadClient,
    ) -> Result<PresignerCreds, MantadClientError> {
        let key = (cluster_id, workspace.to_string());

        if let Some(entry) = self.entries.read().await.get(&key) {
            if entry.fetched_at.elapsed() < TTL {
                return Ok(entry.creds.clone());
            }
        }

        // Cache miss or stale. Fetch from mantad.
        let resp = client.provision_workspace_presigner(workspace).await?;
        let secret = resp.secret_access_key.ok_or_else(|| {
            MantadClientError::Misconfigured(format!(
                "mantad presigner provisioning for workspace {workspace} returned no \
                 secret_access_key"
            ))
        })?;
        let creds = PresignerCreds {
            access_key_id: resp.access_key_id,
            secret_access_key: secret,
        };
        self.entries.write().await.insert(
            key,
            CachedEntry {
                creds: creds.clone(),
                fetched_at: Instant::now(),
            },
        );
        Ok(creds)
    }

    /// Drop the cached credential for `(cluster_id, workspace)`.
    /// Idempotent. Called from `drop_silo_tenant_storage` after the
    /// workspace archive succeeds — mantad's cascade has already
    /// invalidated the secret on its side; tritond's eviction is
    /// belt-and-suspenders.
    pub async fn evict(&self, cluster_id: Uuid, workspace: &str) {
        self.entries
            .write()
            .await
            .remove(&(cluster_id, workspace.to_string()));
    }

    /// Test-only: count the cached entries.
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }
}

pub type SharedPresignerCache = Arc<PresignerCache>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn evict_is_idempotent() {
        let c = PresignerCache::new();
        let id = Uuid::nil();
        c.evict(id, "t-foo").await;
        assert_eq!(c.len().await, 0);
        c.evict(id, "t-foo").await;
        assert_eq!(c.len().await, 0);
    }
}
