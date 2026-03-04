// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::Uuid;

/// Path parameter for server-specific endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ServerPath {
    pub server_uuid: Uuid,
}

/// Query parameters for GET /servers
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ServerListParams {
    /// Comma-separated list of UUIDs to look up
    #[serde(default)]
    pub uuids: Option<String>,
    /// Return only setup servers
    #[serde(default)]
    pub setup: Option<bool>,
    /// Return only headnodes
    #[serde(default)]
    pub headnode: Option<bool>,
    /// Return only reserved servers
    #[serde(default)]
    pub reserved: Option<bool>,
    /// Return only reservoir servers
    #[serde(default)]
    pub reservoir: Option<bool>,
    /// Return machine with given hostname
    #[serde(default)]
    pub hostname: Option<String>,
    /// Comma-separated extras: agents, vms, memory, disk, sysinfo, capacity, all
    #[serde(default)]
    pub extras: Option<String>,
    /// Comma-separated field names to return
    #[serde(default)]
    pub fields: Option<String>,
    /// Maximum number of results (1-1000, default 1000)
    #[serde(default)]
    pub limit: Option<u32>,
    /// Offset for pagination
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Server object as returned by CNAPI
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Server {
    pub uuid: Uuid,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub setup: Option<bool>,
    #[serde(default)]
    pub reserved: Option<bool>,
    #[serde(default)]
    pub headnode: Option<bool>,
    #[serde(default)]
    pub current_platform: Option<String>,
    #[serde(default)]
    pub boot_platform: Option<String>,
    #[serde(default)]
    pub datacenter: Option<String>,
    #[serde(default)]
    pub rack_identifier: Option<String>,
    #[serde(default)]
    pub memory_total_bytes: Option<u64>,
    #[serde(default)]
    pub memory_available_bytes: Option<u64>,
    #[serde(default)]
    pub disk_pool_size_bytes: Option<u64>,
    #[serde(default)]
    pub overprovision_ratios: Option<serde_json::Value>,
    #[serde(default)]
    pub sysinfo: Option<serde_json::Value>,
    #[serde(default)]
    pub agents: Option<Vec<AgentInfo>>,
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    #[serde(default)]
    pub vms: Option<serde_json::Value>,
    #[serde(default)]
    pub last_heartbeat: Option<String>,
    #[serde(default)]
    pub last_boot: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub transitional_status: Option<String>,
    #[serde(default)]
    pub setting_up: Option<bool>,
}

/// Agent information (cn-agent, vm-agent, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub uuid: Option<Uuid>,
    #[serde(default)]
    pub image_uuid: Option<Uuid>,
}

/// Body for POST /servers/:server_uuid (update)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ServerUpdateParams {
    #[serde(default)]
    pub reserved: Option<bool>,
    #[serde(default)]
    pub reservoir: Option<bool>,
    #[serde(default)]
    pub rack_identifier: Option<String>,
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    #[serde(default)]
    pub overprovision_ratios: Option<serde_json::Value>,
    #[serde(default)]
    pub setting_up: Option<bool>,
    #[serde(default)]
    pub transitional_status: Option<String>,
}

/// Body for POST /servers/:server_uuid/events/heartbeat
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HeartbeatParams {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/events/status
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusUpdateParams {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/execute
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommandExecuteParams {
    pub script: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<serde_json::Value>,
}

/// Body for POST /servers/:server_uuid/ensure-image
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnsureImageParams {
    pub image_uuid: Uuid,
    #[serde(default)]
    pub zfs_storage_pool_name: Option<String>,
}

/// Body for POST /servers/:server_uuid/install-agent
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstallAgentParams {
    #[serde(default)]
    pub package_url: Option<String>,
    #[serde(default)]
    pub package_name: Option<String>,
    #[serde(default)]
    pub image_uuid: Option<Uuid>,
}

/// Body for POST /servers/:server_uuid/uninstall-agents
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UninstallAgentsParams {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/recovery-config
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecoveryConfigParams {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/sysinfo (register)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SysinfoRegisterParams {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}
