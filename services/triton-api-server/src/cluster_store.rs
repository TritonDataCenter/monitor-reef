// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Persistence layer for Kelp cluster records.
//!
//! The store splits into two layers:
//! - [`ClusterRecord`]: the full internal orchestration state (credentials,
//!   node inventory, YAML blobs). This is what the store persists.
//! - `Cluster` (from `triton_api`): the lean public API view derived from
//!   `ClusterRecord` via `From<&ClusterRecord>`. Handlers always convert
//!   before returning to callers.
//!
//! Phase 1 uses a file-backed JSON store: one file per cluster at
//! `<state_dir>/<cluster_uuid>.json`. The store is hidden behind the
//! [`ClusterStore`] trait so the eventual moray-backed implementation
//! drops in without touching the endpoint handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::warn;
use triton_api::{Cluster, ClusterState};
use uuid::Uuid;

/// Role of a node within a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Control,
    Worker,
}

/// Inventory record for a single cluster node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub instance_id: Uuid,
    pub primary_ip: String,
    pub fabric_ip: String,
    pub role: NodeRole,
}

/// Configuration baked into the control-plane nodes at bootstrap time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneConfig {
    /// Kubernetes API server endpoint URL (e.g. `https://10.0.0.1:6443`).
    pub endpoint: String,
    pub cns_suffix: String,
    pub package_id: Uuid,
    pub image_id: Uuid,
    pub talos_version: String,
    pub kubernetes_version: String,
}

/// Configuration for worker nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    pub package_id: Uuid,
    pub image_id: Uuid,
}

/// Full internal cluster record — the complete orchestration state.
///
/// The public `Cluster` API type is derived from this via
/// `From<&ClusterRecord>`. Credential fields (`talosconfig_yaml`,
/// `kubeconfig_yaml`, `secrets_yaml`) are not exposed to callers directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterRecord {
    pub id: Uuid,
    pub name: String,
    pub account_id: Uuid,
    pub state: ClusterState,
    pub description: Option<String>,
    pub fabric_network_id: Option<Uuid>,
    pub control_plane_config: Option<ControlPlaneConfig>,
    pub worker_config: Option<WorkerConfig>,
    /// Instance UUID (as string) → node info. The key mirrors
    /// `NodeInfo::instance_id` and is stored redundantly for fast keyed
    /// access.
    pub nodes: HashMap<String, NodeInfo>,
    /// Offset of the last assigned fabric IP within the subnet. Incremented
    /// by the bootstrap endpoint as nodes are provisioned.
    pub last_fabric_ip_offset: Option<u32>,
    pub talosconfig_yaml: Option<String>,
    pub kubeconfig_yaml: Option<String>,
    pub secrets_yaml: Option<String>,
    /// Talos CA certificate (PEM). Stored after bootstrap to authenticate
    /// subsequent operator connections to the cluster.
    pub talos_ca_pem: Option<String>,
    /// Talos operator client certificate (PEM).
    pub talos_crt_pem: Option<String>,
    /// Talos operator client private key (PEM). Sensitive; do not log.
    pub talos_key_pem: Option<String>,
    /// Talos version currently running on the cluster (e.g. `1.7.6`).
    /// Set by the upgrade endpoint; `None` until the first upgrade completes.
    pub talos_version: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Whether the Triton LB controller has been installed into the cluster.
    #[serde(default)]
    pub lb_installed: bool,
}

impl From<&ClusterRecord> for Cluster {
    fn from(r: &ClusterRecord) -> Self {
        let cp = r.control_plane_config.as_ref();
        let control_plane_count = r
            .nodes
            .values()
            .filter(|n| n.role == NodeRole::Control)
            .count() as u32;
        let worker_count = r
            .nodes
            .values()
            .filter(|n| n.role == NodeRole::Worker)
            .count() as u32;
        Cluster {
            id: r.id,
            name: r.name.clone(),
            account_id: r.account_id,
            state: r.state,
            description: r.description.clone(),
            fabric_network_id: r.fabric_network_id,
            kubernetes_version: cp.map(|c| c.kubernetes_version.clone()),
            talos_version: cp
                .map(|c| c.talos_version.clone())
                .or_else(|| r.talos_version.clone()),
            endpoint: cp.map(|c| c.endpoint.clone()),
            control_plane_count,
            worker_count,
            created_at: r.created_at,
            lb_installed: Some(r.lb_installed),
        }
    }
}

/// Errors returned by [`ClusterStore`] implementations.
///
/// Mapped to HTTP responses by the endpoint handlers — `AlreadyExists`
/// becomes 409, `NotFound` becomes 500 (a server-side race condition),
/// I/O and serialization failures become 500.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cluster {0} already exists")]
    AlreadyExists(Uuid),

    #[error("cluster {0} not found (cannot update)")]
    NotFound(Uuid),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Storage abstraction for [`ClusterRecord`]s.
///
/// Implementations must be safe for concurrent use behind an `Arc`.
#[async_trait]
pub trait ClusterStore: Send + Sync {
    /// Persist a new cluster record. Fails with [`StoreError::AlreadyExists`]
    /// if the id is already in use.
    async fn create(&self, record: &ClusterRecord) -> Result<(), StoreError>;

    /// Fetch a cluster by id, or `None` if no such record exists.
    async fn get(&self, id: Uuid) -> Result<Option<ClusterRecord>, StoreError>;

    /// Return every cluster owned by the given account. Order is
    /// unspecified; callers that care should sort client-side.
    async fn list_for_account(&self, account_id: Uuid) -> Result<Vec<ClusterRecord>, StoreError>;

    /// Overwrite an existing cluster record. Fails with [`StoreError::NotFound`]
    /// if the record does not exist (a race condition the bootstrap endpoint
    /// must handle).
    async fn update(&self, record: &ClusterRecord) -> Result<(), StoreError>;

    /// Delete a cluster by id. Returns `true` if a record was removed,
    /// `false` if it did not exist.
    async fn delete(&self, id: Uuid) -> Result<bool, StoreError>;

    /// Find a cluster by name (case-sensitive). Returns the first match across
    /// all accounts, or `None` if not found. Used by unauthed relay endpoints.
    async fn find_by_name(&self, name: &str) -> Result<Option<ClusterRecord>, StoreError>;
}

/// File-backed [`ClusterStore`].
///
/// Each cluster is stored as a single JSON document at
/// `<state_dir>/<uuid>.json`. Writes are atomic — a temp file is
/// written and fsynced, then renamed over the destination — so a
/// crash mid-write never leaves a half-written record on disk.
///
/// `list_for_account` reads the directory and deserialises each entry,
/// which is fine for the cluster counts we expect during the prototype
/// phase. The eventual moray-backed store will use a secondary index on
/// `account_id` instead.
pub struct FileClusterStore {
    state_dir: PathBuf,
}

impl FileClusterStore {
    /// Construct a store rooted at `state_dir`, creating the directory
    /// (and any missing parents) if it does not already exist.
    pub async fn new(state_dir: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(&state_dir).await?;
        Ok(Self { state_dir })
    }

    fn cluster_path(&self, id: Uuid) -> PathBuf {
        self.state_dir.join(format!("{id}.json"))
    }
}

#[async_trait]
impl ClusterStore for FileClusterStore {
    async fn create(&self, record: &ClusterRecord) -> Result<(), StoreError> {
        let final_path = self.cluster_path(record.id);
        if fs::try_exists(&final_path).await? {
            return Err(StoreError::AlreadyExists(record.id));
        }
        let bytes = serde_json::to_vec_pretty(record)?;
        write_atomic(&final_path, &bytes).await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<ClusterRecord>, StoreError> {
        let path = self.cluster_path(id);
        match fs::read(&path).await {
            Ok(bytes) => {
                let record = serde_json::from_slice(&bytes)?;
                Ok(Some(record))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_for_account(&self, account_id: Uuid) -> Result<Vec<ClusterRecord>, StoreError> {
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&self.state_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            // Only read files whose stem is a valid UUID — skip config.json etc.
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if uuid::Uuid::parse_str(stem).is_err() {
                continue;
            }
            let bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            // A corrupt record shouldn't make list_for_account fail entirely
            // — log and skip so a single bad file doesn't break the API.
            let record: ClusterRecord = match serde_json::from_slice(&bytes) {
                Ok(r) => r,
                Err(e) => {
                    warn!(?path, error = %e, "skipping unparseable cluster record");
                    continue;
                }
            };
            if record.account_id == account_id {
                out.push(record);
            }
        }
        Ok(out)
    }

    async fn update(&self, record: &ClusterRecord) -> Result<(), StoreError> {
        let final_path = self.cluster_path(record.id);
        if !fs::try_exists(&final_path).await? {
            return Err(StoreError::NotFound(record.id));
        }
        let bytes = serde_json::to_vec_pretty(record)?;
        write_atomic(&final_path, &bytes).await?;
        Ok(())
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<ClusterRecord>, StoreError> {
        let mut entries = fs::read_dir(&self.state_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if uuid::Uuid::parse_str(stem).is_err() {
                continue;
            }
            let bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let record: ClusterRecord = match serde_json::from_slice(&bytes) {
                Ok(r) => r,
                Err(e) => {
                    warn!(?path, error = %e, "skipping unparseable cluster record");
                    continue;
                }
            };
            if record.name == name {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StoreError> {
        let path = self.cluster_path(id);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// Atomic file write: stage the bytes in a sibling temp file, fsync,
/// then rename over the destination. A crash before the rename leaves
/// the original file (or no file) intact; a crash after the rename
/// leaves the new file. Mirrors the pattern used by
/// `cli/triton-cli/src/commands/login.rs::write_tokens` for the token
/// cache.
async fn write_atomic(final_path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = final_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination path has no parent directory",
        )
    })?;
    let file_name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination path has no file name",
            )
        })?;
    let tmp_path = parent.join(format!(".{file_name}.tmp"));

    {
        let mut tmp = fs::File::create(&tmp_path).await?;
        tmp.write_all(bytes).await?;
        tmp.sync_all().await?;
    }
    // rename is atomic on the same filesystem on both Unix and Windows
    fs::rename(&tmp_path, final_path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use triton_api::ClusterState;

    fn sample_record(id: Uuid, account_id: Uuid, name: &str) -> ClusterRecord {
        ClusterRecord {
            id,
            name: name.to_string(),
            account_id,
            state: ClusterState::Created,
            description: None,
            fabric_network_id: None,
            control_plane_config: None,
            worker_config: None,
            nodes: HashMap::new(),
            last_fabric_ip_offset: None,
            talosconfig_yaml: None,
            kubeconfig_yaml: None,
            secrets_yaml: None,
            talos_ca_pem: None,
            talos_crt_pem: None,
            talos_key_pem: None,
            talos_version: None,
            created_at: Utc::now(),
            lb_installed: false,
        }
    }

    async fn fresh_store() -> (TempDir, FileClusterStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = FileClusterStore::new(dir.path().to_path_buf())
            .await
            .expect("store");
        (dir, store)
    }

    #[tokio::test]
    async fn create_and_get_roundtrip() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let record = sample_record(id, account, "prod");
        store.create(&record).await.expect("create");

        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.id, id);
        assert_eq!(got.account_id, account);
        assert_eq!(got.name, "prod");
        assert_eq!(got.state, ClusterState::Created);
        assert!(got.description.is_none());
        assert!(got.nodes.is_empty());
    }

    #[tokio::test]
    async fn create_rejects_duplicate_id() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let record = sample_record(id, account, "first");
        store.create(&record).await.expect("create");

        let dup = sample_record(id, account, "second");
        let err = store.create(&dup).await.expect_err("should reject");
        match err {
            StoreError::AlreadyExists(eid) => assert_eq!(eid, id),
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let (_dir, store) = fresh_store().await;
        let missing = Uuid::new_v4();
        assert!(store.get(missing).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn list_filters_by_account() {
        let (_dir, store) = fresh_store().await;
        let account_a = Uuid::new_v4();
        let account_b = Uuid::new_v4();
        let a1 = sample_record(Uuid::new_v4(), account_a, "a1");
        let a2 = sample_record(Uuid::new_v4(), account_a, "a2");
        let b1 = sample_record(Uuid::new_v4(), account_b, "b1");
        store.create(&a1).await.expect("create a1");
        store.create(&a2).await.expect("create a2");
        store.create(&b1).await.expect("create b1");

        let mut a_list = store.list_for_account(account_a).await.expect("list a");
        a_list.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(a_list.len(), 2);
        assert_eq!(a_list[0].name, "a1");
        assert_eq!(a_list[1].name, "a2");

        let b_list = store.list_for_account(account_b).await.expect("list b");
        assert_eq!(b_list.len(), 1);
        assert_eq!(b_list[0].name, "b1");
    }

    #[tokio::test]
    async fn update_existing_record() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let record = sample_record(id, account, "prod");
        store.create(&record).await.expect("create");

        let mut updated = record.clone();
        updated.state = ClusterState::Provisioning;
        store.update(&updated).await.expect("update");

        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.state, ClusterState::Provisioning);
    }

    #[tokio::test]
    async fn update_missing_returns_not_found() {
        let (_dir, store) = fresh_store().await;
        let record = sample_record(Uuid::new_v4(), Uuid::new_v4(), "ghost");
        let err = store.update(&record).await.expect_err("should fail");
        match err {
            StoreError::NotFound(id) => assert_eq!(id, record.id),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_existing_returns_true() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let record = sample_record(id, Uuid::new_v4(), "doomed");
        store.create(&record).await.expect("create");
        assert!(store.delete(id).await.expect("delete"));
        assert!(store.get(id).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn delete_missing_returns_false() {
        let (_dir, store) = fresh_store().await;
        assert!(!store.delete(Uuid::new_v4()).await.expect("delete"));
    }

    #[tokio::test]
    async fn corrupt_file_is_skipped_by_list() {
        let (dir, store) = fresh_store().await;
        let good = sample_record(Uuid::new_v4(), Uuid::new_v4(), "good");
        store.create(&good).await.expect("create good");

        // Drop a non-JSON file in the state dir.
        let bad_path = dir.path().join(format!("{}.json", Uuid::new_v4()));
        tokio::fs::write(&bad_path, b"not json")
            .await
            .expect("write bad");

        let list = store.list_for_account(good.account_id).await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, good.id);
    }

    #[tokio::test]
    async fn create_dir_if_missing() {
        let dir = TempDir::new().expect("tempdir");
        let nested = dir.path().join("does/not/exist/yet");
        let store = FileClusterStore::new(nested.clone())
            .await
            .expect("create nested");
        assert!(nested.is_dir());
        let record = sample_record(Uuid::new_v4(), Uuid::new_v4(), "x");
        store.create(&record).await.expect("create");
    }

    #[tokio::test]
    async fn from_cluster_record_counts_nodes_by_role() {
        let id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let mut record = sample_record(id, account, "multi-node");
        record.nodes.insert(
            "cp-1".to_string(),
            NodeInfo {
                instance_id: Uuid::new_v4(),
                primary_ip: "10.0.0.1".to_string(),
                fabric_ip: "192.168.1.1".to_string(),
                role: NodeRole::Control,
            },
        );
        record.nodes.insert(
            "worker-1".to_string(),
            NodeInfo {
                instance_id: Uuid::new_v4(),
                primary_ip: "10.0.0.2".to_string(),
                fabric_ip: "192.168.1.2".to_string(),
                role: NodeRole::Worker,
            },
        );
        record.nodes.insert(
            "worker-2".to_string(),
            NodeInfo {
                instance_id: Uuid::new_v4(),
                primary_ip: "10.0.0.3".to_string(),
                fabric_ip: "192.168.1.3".to_string(),
                role: NodeRole::Worker,
            },
        );

        let cluster = Cluster::from(&record);
        assert_eq!(cluster.control_plane_count, 1);
        assert_eq!(cluster.worker_count, 2);
        assert!(cluster.kubernetes_version.is_none());
        assert!(cluster.endpoint.is_none());
    }
}
