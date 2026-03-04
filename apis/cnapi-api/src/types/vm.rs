// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::Uuid;

/// Path parameter for VM endpoints under a server
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmPath {
    pub server_uuid: Uuid,
    pub uuid: Uuid,
}

/// Body for POST /servers/:server_uuid/vms (create)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmCreateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/update
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmUpdateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/reprovision
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmReprovisionParams {
    pub image_uuid: Uuid,
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Body for POST /servers/:server_uuid/vms/nics/update
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmNicsUpdateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/snapshots
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmSnapshotParams {
    pub snapshot_name: String,
}

/// Body for PUT /servers/:server_uuid/vms/:uuid/snapshots (rollback)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmRollbackParams {
    pub snapshot_name: String,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/images (create-image)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmImageCreateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/migrate
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmMigrateParams {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/docker-exec
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DockerExecParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/docker-copy
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DockerCopyParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for POST /servers/:server_uuid/vms/:uuid/docker-build
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DockerBuildParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Body for GET /servers/:server_uuid/vms/:uuid/docker-stats
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DockerStatsParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}
