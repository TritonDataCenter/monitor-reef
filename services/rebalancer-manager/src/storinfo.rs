// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Storinfo client for discovering storage nodes
//!
//! Storinfo provides information about available storage nodes
//! including their capacity, availability, and shark (storage server) info.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use rebalancer_types::StorageNode;

/// Storinfo client errors
#[derive(Debug, Error)]
pub enum StorinfoError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Storage node not found: {0}")]
    NotFound(String),

    #[error("Storinfo service unavailable")]
    Unavailable,
}

/// Raw shark information from Storinfo API
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SharkInfo {
    #[serde(rename = "manta_storage_id")]
    pub manta_storage_id: String,
    pub datacenter: String,
    #[serde(rename = "availableMB")]
    pub available_mb: u64,
    #[serde(rename = "percentUsed")]
    pub percent_used: f64,
    pub timestamp: String,
}

/// Extended storage node info with capacity data (internal use only)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StorageNodeInfo {
    /// The storage node identity
    pub node: StorageNode,
    /// Available capacity in MB
    pub available_mb: u64,
    /// Percentage of storage used
    pub percent_used: f64,
}

impl From<SharkInfo> for StorageNodeInfo {
    fn from(shark: SharkInfo) -> Self {
        StorageNodeInfo {
            node: StorageNode {
                manta_storage_id: shark.manta_storage_id,
                datacenter: shark.datacenter,
            },
            available_mb: shark.available_mb,
            percent_used: shark.percent_used,
        }
    }
}

/// Storinfo client with caching
pub struct StorinfoClient {
    client: Client,
    base_url: String,
    cache: Arc<RwLock<StorinfoCache>>,
}

/// Cached storage node information
struct StorinfoCache {
    nodes: HashMap<String, StorageNodeInfo>,
    last_updated: Option<std::time::Instant>,
}

impl StorinfoClient {
    /// Create a new Storinfo client
    pub fn new(base_url: String, timeout_secs: u64) -> Result<Self, StorinfoError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()?;

        Ok(Self {
            client,
            base_url,
            cache: Arc::new(RwLock::new(StorinfoCache {
                nodes: HashMap::new(),
                last_updated: None,
            })),
        })
    }

    /// Refresh the cache from Storinfo
    pub async fn refresh(&self) -> Result<(), StorinfoError> {
        let url = format!("{}/poll", self.base_url);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                "Storinfo poll returned non-success status"
            );
            return Err(StorinfoError::Unavailable);
        }

        let sharks: Vec<SharkInfo> = response.json().await?;

        let mut cache = self.cache.write().await;
        cache.nodes.clear();
        for shark in sharks {
            let info: StorageNodeInfo = shark.into();
            cache.nodes.insert(info.node.manta_storage_id.clone(), info);
        }
        cache.last_updated = Some(std::time::Instant::now());

        tracing::debug!(count = cache.nodes.len(), "Refreshed storinfo cache");

        Ok(())
    }

    /// Get a storage node by ID
    pub async fn get_node(&self, storage_id: &str) -> Result<StorageNode, StorinfoError> {
        // Try cache first
        {
            let cache = self.cache.read().await;
            if let Some(info) = cache.nodes.get(storage_id) {
                return Ok(info.node.clone());
            }
        }

        // Refresh cache and try again
        self.refresh().await?;

        let cache = self.cache.read().await;
        cache
            .nodes
            .get(storage_id)
            .map(|info| info.node.clone())
            .ok_or_else(|| StorinfoError::NotFound(storage_id.to_string()))
    }

    /// Get all storage nodes
    #[allow(dead_code)]
    pub async fn get_all_nodes(&self) -> Result<Vec<StorageNode>, StorinfoError> {
        // Check if cache needs refresh (older than 30 seconds)
        let needs_refresh = {
            let cache = self.cache.read().await;
            match cache.last_updated {
                None => true,
                Some(t) => t.elapsed() > Duration::from_secs(30),
            }
        };

        if needs_refresh {
            self.refresh().await?;
        }

        let cache = self.cache.read().await;
        Ok(cache.nodes.values().map(|info| info.node.clone()).collect())
    }

    /// Get nodes in a specific datacenter
    #[allow(dead_code)]
    pub async fn get_nodes_in_datacenter(
        &self,
        datacenter: &str,
    ) -> Result<Vec<StorageNode>, StorinfoError> {
        let all_nodes = self.get_all_nodes().await?;
        Ok(all_nodes
            .into_iter()
            .filter(|n| n.datacenter == datacenter)
            .collect())
    }

    /// Get nodes with available capacity above threshold (in MB)
    #[allow(dead_code)]
    pub async fn get_nodes_with_capacity(
        &self,
        min_available_mb: u64,
    ) -> Result<Vec<StorageNode>, StorinfoError> {
        // Check if cache needs refresh
        let needs_refresh = {
            let cache = self.cache.read().await;
            match cache.last_updated {
                None => true,
                Some(t) => t.elapsed() > Duration::from_secs(30),
            }
        };

        if needs_refresh {
            self.refresh().await?;
        }

        let cache = self.cache.read().await;
        Ok(cache
            .nodes
            .values()
            .filter(|info| info.available_mb >= min_available_mb)
            .map(|info| info.node.clone())
            .collect())
    }
}
