// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Cluster state management

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Cluster metadata and state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    /// Cluster UUID
    pub uuid: Uuid,

    /// Cluster name
    pub name: String,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Creation timestamp
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// Fabric network ID (if using fabric networking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fabric_network_id: Option<Uuid>,

    /// Control plane configuration (set during bootstrap)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_plane: Option<ControlPlaneConfig>,

    /// Worker configuration (set during bootstrap)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<WorkerConfig>,

    /// Node information
    #[serde(default)]
    pub nodes: HashMap<String, NodeInfo>,

    /// Last allocated fabric IP offset (for continuing IP allocation when adding workers)
    ///
    /// This is the offset from the subnet base address. For example, if the subnet is
    /// 10.0.0.0/24 and the last allocated IP was 10.0.0.14, this value would be 14.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fabric_ip_offset: Option<u32>,
}

/// Control plane configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneConfig {
    /// Control plane endpoint (IP address)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// CNS suffix for constructing load-balanced hostname (e.g. "cns.us-west-1.triton.zone")
    ///
    /// When set, the control plane can be accessed via `ctrl.<cns_suffix>` which
    /// load-balances across all control plane nodes tagged with `triton.cns.services=ctrl`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cns_suffix: Option<String>,

    /// Package UUID or name (as specified by user)
    pub package: String,

    /// Image UUID or name (as specified by user)
    pub image: String,

    /// Talos version
    pub talos_version: String,

    /// Resolved package UUID (for use when adding control plane nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_id: Option<Uuid>,

    /// Resolved image UUID (for use when adding control plane nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<Uuid>,
}

/// Worker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Package UUID or name (as specified by user)
    pub package: String,

    /// Image UUID or name (as specified by user)
    pub image: String,

    /// Resolved package UUID (for use when adding workers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_id: Option<Uuid>,

    /// Resolved image UUID (for use when adding workers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<Uuid>,
}

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Instance UUID
    pub instance_id: Uuid,

    /// Primary IP address (external)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_ip: Option<String>,

    /// Fabric IP address
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fabric_ip: Option<String>,

    /// Node role (control or worker)
    pub role: NodeRole,
}

/// Node role
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    Control,
    Worker,
}

impl ClusterState {
    /// Create a new cluster state
    pub fn new(name: String, description: Option<String>, fabric_network_id: Option<Uuid>) -> Self {
        Self {
            uuid: Uuid::new_v4(),
            name,
            description,
            created_at: chrono::Utc::now(),
            fabric_network_id,
            control_plane: None,
            workers: None,
            nodes: HashMap::new(),
            last_fabric_ip_offset: None,
        }
    }

    /// Get the cluster directory path
    pub fn cluster_dir(&self) -> Result<PathBuf> {
        Ok(clusters_base_dir()?.join(self.uuid.to_string()))
    }

    /// Get the cluster.json file path
    pub fn state_file(&self) -> Result<PathBuf> {
        Ok(self.cluster_dir()?.join("cluster.json"))
    }

    /// Save the cluster state to disk
    pub async fn save(&self) -> Result<()> {
        let dir = self.cluster_dir()?;
        tokio::fs::create_dir_all(&dir).await?;

        let state_file = self.state_file()?;
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&state_file, json).await?;

        Ok(())
    }

    /// Load cluster state from disk by UUID
    pub async fn load(uuid: Uuid) -> Result<Self> {
        let state_file = clusters_base_dir()?
            .join(uuid.to_string())
            .join("cluster.json");
        let json = tokio::fs::read_to_string(&state_file).await?;
        let state: Self = serde_json::from_str(&json)?;
        Ok(state)
    }

    /// Load cluster state by name (searches all clusters)
    pub async fn load_by_name(name: &str) -> Result<Self> {
        let clusters = list_clusters().await?;
        for cluster in clusters {
            if cluster.name == name {
                return Ok(cluster);
            }
        }
        anyhow::bail!("Cluster not found: {}", name)
    }

    /// Load cluster state by name, full UUID, or short UUID
    pub async fn load_by_name_or_uuid(name_or_uuid: &str) -> Result<Self> {
        // Try parsing as full UUID first
        if let Ok(uuid) = Uuid::parse_str(name_or_uuid) {
            return Self::load(uuid).await;
        }

        // Check if it looks like a short UUID (8 hex characters)
        if name_or_uuid.len() == 8 && name_or_uuid.chars().all(|c| c.is_ascii_hexdigit()) {
            return Self::load_by_short_uuid(name_or_uuid).await;
        }

        // Otherwise search by name
        Self::load_by_name(name_or_uuid).await
    }

    /// Load cluster state by short UUID prefix (e.g. first 8 characters)
    async fn load_by_short_uuid(short_uuid: &str) -> Result<Self> {
        let clusters = list_clusters().await?;
        let matches: Vec<ClusterState> = clusters
            .into_iter()
            .filter(|c| c.uuid.to_string().starts_with(short_uuid))
            .collect();

        match matches.len() {
            0 => anyhow::bail!("Cluster not found: {}", short_uuid),
            1 => {
                // We know there's exactly one element, so use Vec indexing
                let cluster = &matches[0];
                Ok(cluster.clone())
            }
            _ => {
                let matching_ids: Vec<String> = matches
                    .iter()
                    .map(|c| format!("{} ({})", c.name, &c.uuid.to_string()[..8]))
                    .collect();
                anyhow::bail!(
                    "Ambiguous short UUID '{}' matches multiple clusters:\n  {}",
                    short_uuid,
                    matching_ids.join("\n  ")
                )
            }
        }
    }

    /// Delete cluster state from disk
    pub async fn delete(&self) -> Result<()> {
        let dir = self.cluster_dir()?;
        tokio::fs::remove_dir_all(&dir).await?;
        Ok(())
    }
}

/// Get the base clusters directory (~/.triton/clusters)
pub fn clusters_base_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".triton").join("clusters"))
}

/// List all clusters sorted by creation time (newest first)
pub async fn list_clusters() -> Result<Vec<ClusterState>> {
    let base_dir = clusters_base_dir()?;

    // Create directory if it doesn't exist
    if !base_dir.exists() {
        tokio::fs::create_dir_all(&base_dir).await?;
        return Ok(vec![]);
    }

    let mut clusters = Vec::new();
    let mut entries = tokio::fs::read_dir(&base_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let state_file = path.join("cluster.json");
        if !state_file.exists() {
            continue;
        }

        match tokio::fs::read_to_string(&state_file).await {
            Ok(json) => {
                if let Ok(cluster) = serde_json::from_str::<ClusterState>(&json) {
                    clusters.push(cluster);
                }
            }
            Err(_) => continue,
        }
    }

    // Sort by creation time (newest first)
    clusters.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(clusters)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_short_uuid_detection() {
        // Valid short UUIDs
        assert!(is_short_uuid("2e26aefb"));
        assert!(is_short_uuid("0b92f505"));
        assert!(is_short_uuid("12345678"));
        assert!(is_short_uuid("abcdef01"));
        assert!(is_short_uuid("ABCDEF01"));

        // Invalid short UUIDs
        assert!(!is_short_uuid("2e26aef")); // Too short
        assert!(!is_short_uuid("2e26aefb9")); // Too long
        assert!(!is_short_uuid("my-cluster")); // Not hex
        assert!(!is_short_uuid("2e26aefg")); // Contains non-hex char
        assert!(!is_short_uuid("2e26-aefb")); // Contains hyphen
    }

    fn is_short_uuid(s: &str) -> bool {
        s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
    }
}
