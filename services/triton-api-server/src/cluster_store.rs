// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Persistence layer for Kelp cluster records.
//!
//! Phase 1 uses a file-backed JSON store: one file per cluster at
//! `<state_dir>/<cluster_uuid>.json`. The store is hidden behind the
//! [`ClusterStore`] trait so the eventual moray-backed implementation
//! drops in without touching the endpoint handlers. See
//! `docs/design/kelp-cluster-storage.md` for the migration plan.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::warn;
use triton_api::Cluster;
use uuid::Uuid;

/// Errors returned by [`ClusterStore`] implementations.
///
/// Mapped to HTTP responses by the endpoint handlers — `AlreadyExists`
/// becomes 409, I/O and serialization failures become 500.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cluster {0} already exists")]
    AlreadyExists(Uuid),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Storage abstraction for [`Cluster`] records.
///
/// Implementations must be safe for concurrent use behind an `Arc`.
/// The trait is deliberately narrow — Phase 1 only needs CRUD on whole
/// records. Bootstrap and other future endpoints will add operations
/// for state transitions and partial updates.
#[async_trait]
pub trait ClusterStore: Send + Sync {
    /// Persist a new cluster record. Fails with
    /// [`StoreError::AlreadyExists`] if the id is already in use.
    async fn create(&self, cluster: &Cluster) -> Result<(), StoreError>;

    /// Fetch a cluster by id, or `None` if no such record exists.
    async fn get(&self, id: Uuid) -> Result<Option<Cluster>, StoreError>;

    /// Return every cluster owned by the given account. Order is
    /// unspecified; callers that care should sort client-side.
    async fn list_for_account(&self, account_id: Uuid) -> Result<Vec<Cluster>, StoreError>;

    /// Delete a cluster by id. Returns `true` if a record was removed,
    /// `false` if it did not exist.
    async fn delete(&self, id: Uuid) -> Result<bool, StoreError>;
}

/// File-backed [`ClusterStore`].
///
/// Each cluster is stored as a single JSON document at
/// `<state_dir>/<uuid>.json`. Writes are atomic — a temp file is
/// written and fsynced, then renamed over the destination — so a
/// crash mid-write never leaves a half-written record on disk.
///
/// `list_for_account` reads the directory and deserialises each
/// entry, which is fine for the cluster counts we expect during the
/// prototype phase. The eventual moray-backed store will use a
/// secondary index on `account_id` instead.
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
    async fn create(&self, cluster: &Cluster) -> Result<(), StoreError> {
        let final_path = self.cluster_path(cluster.id);
        if fs::try_exists(&final_path).await? {
            return Err(StoreError::AlreadyExists(cluster.id));
        }
        let bytes = serde_json::to_vec_pretty(cluster)?;
        write_atomic(&final_path, &bytes).await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<Cluster>, StoreError> {
        let path = self.cluster_path(id);
        match fs::read(&path).await {
            Ok(bytes) => {
                let cluster = serde_json::from_slice(&bytes)?;
                Ok(Some(cluster))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_for_account(&self, account_id: Uuid) -> Result<Vec<Cluster>, StoreError> {
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&self.state_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            // A corrupt record shouldn't make list_for_account fail entirely
            // — log and skip so a single bad file doesn't break the API.
            let cluster: Cluster = match serde_json::from_slice(&bytes) {
                Ok(c) => c,
                Err(e) => {
                    warn!(?path, error = %e, "skipping unparseable cluster record");
                    continue;
                }
            };
            if cluster.account_id == account_id {
                out.push(cluster);
            }
        }
        Ok(out)
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
    use chrono::Utc;
    use tempfile::TempDir;
    use triton_api::ClusterState;

    fn sample_cluster(id: Uuid, account_id: Uuid, name: &str) -> Cluster {
        Cluster {
            id,
            name: name.to_string(),
            account_id,
            state: ClusterState::Created,
            kubernetes_version: "1.30.3".to_string(),
            talos_version: "1.7.6".to_string(),
            created_at: Utc::now(),
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
        let cluster = sample_cluster(id, account, "prod");
        store.create(&cluster).await.expect("create");

        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.id, id);
        assert_eq!(got.account_id, account);
        assert_eq!(got.name, "prod");
        assert_eq!(got.state, ClusterState::Created);
    }

    #[tokio::test]
    async fn create_rejects_duplicate_id() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let cluster = sample_cluster(id, account, "first");
        store.create(&cluster).await.expect("create");

        let dup = sample_cluster(id, account, "second");
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
        let a1 = sample_cluster(Uuid::new_v4(), account_a, "a1");
        let a2 = sample_cluster(Uuid::new_v4(), account_a, "a2");
        let b1 = sample_cluster(Uuid::new_v4(), account_b, "b1");
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
    async fn delete_existing_returns_true() {
        let (_dir, store) = fresh_store().await;
        let id = Uuid::new_v4();
        let cluster = sample_cluster(id, Uuid::new_v4(), "doomed");
        store.create(&cluster).await.expect("create");
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
        let good = sample_cluster(Uuid::new_v4(), Uuid::new_v4(), "good");
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
        // And the store works once constructed.
        let cluster = sample_cluster(Uuid::new_v4(), Uuid::new_v4(), "x");
        store.create(&cluster).await.expect("create");
    }
}
