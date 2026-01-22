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

    /// Get all nodes with full capacity info (available_mb, percent_used)
    ///
    /// This is useful for destination selection where we need to track
    /// and compare available capacity across sharks.
    pub async fn get_all_nodes_with_info(&self) -> Result<Vec<StorageNodeInfo>, StorinfoError> {
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
        Ok(cache.nodes.values().cloned().collect())
    }

    /// Get nodes with capacity info, excluding specified datacenters
    ///
    /// This is useful for destination selection when certain datacenters
    /// should not be used as evacuation targets (e.g., due to maintenance,
    /// capacity constraints, or regional restrictions).
    ///
    /// # Arguments
    /// * `blacklist` - Datacenter names to exclude from results
    ///
    /// # Example
    /// ```ignore
    /// let nodes = storinfo.get_nodes_excluding_datacenters(&["dc1", "dc2"]).await?;
    /// // nodes will not contain any sharks in dc1 or dc2
    /// ```
    pub async fn get_nodes_excluding_datacenters(
        &self,
        blacklist: &[String],
    ) -> Result<Vec<StorageNodeInfo>, StorinfoError> {
        let all_nodes = self.get_all_nodes_with_info().await?;

        if blacklist.is_empty() {
            return Ok(all_nodes);
        }

        Ok(all_nodes
            .into_iter()
            .filter(|info| !blacklist.contains(&info.node.datacenter))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a StorageNodeInfo for testing
    fn make_node_info(id: &str, datacenter: &str, available_mb: u64) -> StorageNodeInfo {
        StorageNodeInfo {
            node: StorageNode {
                manta_storage_id: id.to_string(),
                datacenter: datacenter.to_string(),
            },
            available_mb,
            percent_used: 50.0,
        }
    }

    // Helper to create StorageNodeInfo with custom percent_used
    fn make_node_info_full(
        id: &str,
        datacenter: &str,
        available_mb: u64,
        percent_used: f64,
    ) -> StorageNodeInfo {
        StorageNodeInfo {
            node: StorageNode {
                manta_storage_id: id.to_string(),
                datacenter: datacenter.to_string(),
            },
            available_mb,
            percent_used,
        }
    }

    // -------------------------------------------------------------------------
    // Test: SharkInfo to StorageNodeInfo conversion
    // -------------------------------------------------------------------------
    #[test]
    fn test_shark_info_conversion() {
        let shark = SharkInfo {
            manta_storage_id: "1.stor.domain.com".to_string(),
            datacenter: "dc1".to_string(),
            available_mb: 1000,
            percent_used: 25.5,
            timestamp: "2025-01-01T00:00:00Z".to_string(),
        };

        let info: StorageNodeInfo = shark.into();

        assert_eq!(info.node.manta_storage_id, "1.stor.domain.com");
        assert_eq!(info.node.datacenter, "dc1");
        assert_eq!(info.available_mb, 1000);
        assert!((info.percent_used - 25.5).abs() < 0.01);
    }

    // -------------------------------------------------------------------------
    // Test: Capacity filtering logic
    // -------------------------------------------------------------------------
    #[test]
    fn test_capacity_filtering() {
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 500),   // Below threshold
            make_node_info("shark2.dc1", "dc1", 1000),  // At threshold
            make_node_info("shark3.dc2", "dc2", 2000),  // Above threshold
            make_node_info("shark4.dc2", "dc2", 100),   // Well below threshold
        ];

        let min_available_mb = 1000u64;

        let filtered: Vec<_> = nodes
            .iter()
            .filter(|info| info.available_mb >= min_available_mb)
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].node.manta_storage_id, "shark2.dc1");
        assert_eq!(filtered[1].node.manta_storage_id, "shark3.dc2");
    }

    // -------------------------------------------------------------------------
    // Test: Datacenter filtering logic
    // -------------------------------------------------------------------------
    #[test]
    fn test_datacenter_filtering() {
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 1000),
            make_node_info("shark2.dc1", "dc1", 2000),
            make_node_info("shark3.dc2", "dc2", 3000),
            make_node_info("shark4.dc3", "dc3", 4000),
        ];

        let datacenter = "dc2";

        let filtered: Vec<_> = nodes
            .iter()
            .filter(|info| info.node.datacenter == datacenter)
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].node.manta_storage_id, "shark3.dc2");
    }

    // -------------------------------------------------------------------------
    // Test: Multiple datacenter blacklist
    // -------------------------------------------------------------------------
    #[test]
    fn test_multiple_blacklist() {
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 1000),
            make_node_info("shark2.dc2", "dc2", 2000),
            make_node_info("shark3.dc3", "dc3", 3000),
            make_node_info("shark4.dc4", "dc4", 4000),
            make_node_info("shark5.dc5", "dc5", 5000),
        ];

        let blacklist = ["dc1".to_string(), "dc3".to_string(), "dc5".to_string()];

        let filtered: Vec<_> = nodes
            .into_iter()
            .filter(|info| !blacklist.contains(&info.node.datacenter))
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].node.datacenter, "dc2");
        assert_eq!(filtered[1].node.datacenter, "dc4");
    }

    // -------------------------------------------------------------------------
    // Test: Node info with edge case percent_used values
    // -------------------------------------------------------------------------
    #[test]
    fn test_percent_used_edge_cases() {
        // 0% used
        let empty_node = make_node_info_full("shark1", "dc1", 10000, 0.0);
        assert_eq!(empty_node.percent_used, 0.0);

        // 100% used
        let full_node = make_node_info_full("shark2", "dc1", 0, 100.0);
        assert_eq!(full_node.percent_used, 100.0);
        assert_eq!(full_node.available_mb, 0);

        // High precision
        let precise_node = make_node_info_full("shark3", "dc1", 500, 87.654321);
        assert!((precise_node.percent_used - 87.654321).abs() < 0.000001);
    }

    #[test]
    fn test_blacklist_filtering() {
        // Test the filtering logic directly without async
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 1000),
            make_node_info("shark2.dc1", "dc1", 2000),
            make_node_info("shark3.dc2", "dc2", 3000),
            make_node_info("shark4.dc3", "dc3", 4000),
        ];

        let blacklist = ["dc1".to_string(), "dc2".to_string()];

        // Apply the same filtering logic used in get_nodes_excluding_datacenters
        let filtered: Vec<_> = nodes
            .into_iter()
            .filter(|info| !blacklist.contains(&info.node.datacenter))
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].node.manta_storage_id, "shark4.dc3");
        assert_eq!(filtered[0].node.datacenter, "dc3");
    }

    #[test]
    fn test_empty_blacklist_returns_all() {
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 1000),
            make_node_info("shark2.dc2", "dc2", 2000),
        ];

        let blacklist: Vec<String> = vec![];

        let filtered: Vec<_> = nodes
            .into_iter()
            .filter(|info| blacklist.is_empty() || !blacklist.contains(&info.node.datacenter))
            .collect();

        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_blacklist_all_returns_empty() {
        let nodes = vec![
            make_node_info("shark1.dc1", "dc1", 1000),
            make_node_info("shark2.dc1", "dc1", 2000),
        ];

        let blacklist = ["dc1".to_string()];

        let filtered: Vec<_> = nodes
            .into_iter()
            .filter(|info| !blacklist.contains(&info.node.datacenter))
            .collect();

        assert_eq!(filtered.len(), 0);
    }
}
