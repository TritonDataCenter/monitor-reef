// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Action dispatch types for IMGAPI
//!
//! IMGAPI uses action dispatch on several POST endpoints:
//! - POST /images/:uuid — 12 actions (import, activate, export, etc.)
//! - POST /images — 4 actions (create, create-from-vm, import-docker-image, import-lxd-image)
//! - POST /images/:uuid/acl — 2 actions (add, remove)
//! - POST /state — 1 action (dropcaches)

use super::common::Uuid;
use super::file::StorageType;
use super::image::{ImageOs, ImageType};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// POST /images/:uuid action dispatch
// ============================================================================

/// Actions available via POST /images/:uuid
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ImageAction {
    /// Import an image manifest (admin-only)
    Import,
    /// Import an image from a remote IMGAPI (admin-only, creates workflow job)
    ImportRemote,
    /// Import an image from another datacenter (creates workflow job)
    ImportFromDatacenter,
    /// Import a Docker image (admin-only, streaming response)
    ImportDockerImage,
    /// Import an LXD image (admin-only, streaming response)
    ImportLxdImage,
    /// Change storage backend (admin-only)
    ChangeStor,
    /// Export image to Manta
    Export,
    /// Activate an unactivated image
    Activate,
    /// Enable a disabled image
    Enable,
    /// Disable an active image
    Disable,
    /// Add image to a channel
    ChannelAdd,
    /// Update mutable image fields
    Update,
}

/// Query parameters for POST /images/:uuid action dispatch
///
/// The `action` field is optional in the query string because clients may
/// send it in the request body instead.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImageActionQuery {
    /// Action to perform
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// Per-action request body types for POST /images/:uuid
// ============================================================================

/// Request body for `import` action (admin-only)
///
/// The body is the full image manifest with uuid matching the URL.
/// Uses the same shape as CreateImageRequest but the uuid must be present.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImportImageRequest {
    /// Manifest version
    #[serde(default)]
    pub v: Option<u32>,
    /// Image UUID (must match URL path)
    pub uuid: Uuid,
    /// Owner account UUID
    pub owner: Uuid,
    /// Image name
    pub name: String,
    /// Image version string
    pub version: String,
    /// Image type
    #[serde(default, rename = "type")]
    pub image_type: Option<ImageType>,
    /// Operating system
    #[serde(default)]
    pub os: Option<ImageOs>,
    /// Whether the image is public
    #[serde(default)]
    pub public: Option<bool>,
    /// Whether the image is disabled
    #[serde(default)]
    pub disabled: Option<bool>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// ACL
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Tags
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
    /// Requirements
    #[serde(default)]
    pub requirements: Option<serde_json::Value>,
    /// Users
    #[serde(default)]
    pub users: Option<Vec<super::image::ImageUser>>,
    /// Generate passwords
    #[serde(default)]
    pub generate_passwords: Option<bool>,
    /// Inherited directories
    #[serde(default)]
    pub inherited_directories: Option<Vec<String>>,
    /// Origin image UUID
    #[serde(default)]
    pub origin: Option<Uuid>,
    /// NIC driver (zvol only)
    #[serde(default)]
    pub nic_driver: Option<String>,
    /// Disk driver (zvol only)
    #[serde(default)]
    pub disk_driver: Option<String>,
    /// CPU type (zvol only)
    #[serde(default)]
    pub cpu_type: Option<String>,
    /// Image size in MB (zvol only)
    #[serde(default)]
    pub image_size: Option<u64>,
    /// Channels
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    /// Billing tags
    #[serde(default)]
    pub billing_tags: Option<Vec<String>>,
    /// Traits
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
}

/// Query parameters for `import` action with source (admin-only, fetches from remote)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportImageFromSourceQuery {
    /// Action (must be "import")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Source IMGAPI URL to fetch the manifest from
    pub source: String,
    /// Skip owner check
    #[serde(default)]
    pub skip_owner_check: Option<bool>,
    /// Storage backend to use
    #[serde(default)]
    pub storage: Option<StorageType>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

/// Query parameters for `import-remote` action (admin-only)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportRemoteImageQuery {
    /// Action (must be "import-remote")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Source IMGAPI URL
    pub source: String,
    /// Skip owner check
    #[serde(default)]
    pub skip_owner_check: Option<bool>,
}

/// Query parameters for `import-from-datacenter` action
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportFromDatacenterQuery {
    /// Action (must be "import-from-datacenter")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Datacenter name to import from
    pub datacenter: String,
    /// Account UUID
    pub account: Uuid,
}

/// Query parameters for `import-docker-image` action (admin-only)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportDockerImageQuery {
    /// Action (must be "import-docker-image")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Docker repository name
    pub repo: Option<String>,
    /// Docker image tag (use tag or digest, not both)
    #[serde(default)]
    pub tag: Option<String>,
    /// Docker image digest (use tag or digest, not both)
    #[serde(default)]
    pub digest: Option<String>,
    /// Whether this is a public image
    #[serde(default)]
    pub public: Option<bool>,
}

/// Query parameters for `import-lxd-image` action (admin-only)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportLxdImageQuery {
    /// Action (must be "import-lxd-image")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// LXD image alias or fingerprint
    #[serde(default)]
    pub alias: Option<String>,
}

/// Request body for `change-stor` action (admin-only)
///
/// Empty body; storage type is specified in query params.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChangeStorQuery {
    /// Action (must be "change-stor")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Target storage backend
    pub stor: StorageType,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

/// Query parameters for `export` action
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportImageQuery {
    /// Action (must be "export")
    #[serde(default)]
    pub action: Option<ImageAction>,
    /// Manta path to export to
    pub manta_path: String,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

/// Request body for `activate` action
///
/// No additional fields required.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ActivateImageRequest {}

/// Request body for `enable` action
///
/// No additional fields required.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnableImageRequest {}

/// Request body for `disable` action
///
/// No additional fields required.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DisableImageRequest {}

/// Request body for `channel-add` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChannelAddImageRequest {
    /// Channel name to add the image to
    pub channel: String,
}

/// Request body for `update` action
///
/// Contains all mutable fields on an image manifest.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateImageRequest {
    /// Updated image name
    #[serde(default)]
    pub name: Option<String>,
    /// Updated version
    #[serde(default)]
    pub version: Option<String>,
    /// Updated description
    #[serde(default)]
    pub description: Option<String>,
    /// Updated homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// Updated EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// Updated ACL
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Updated tags
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
    /// Updated requirements
    #[serde(default)]
    pub requirements: Option<serde_json::Value>,
    /// Updated users
    #[serde(default)]
    pub users: Option<Vec<super::image::ImageUser>>,
    /// Updated generate_passwords
    #[serde(default)]
    pub generate_passwords: Option<bool>,
    /// Updated inherited_directories
    #[serde(default)]
    pub inherited_directories: Option<Vec<String>>,
    /// Updated billing tags
    #[serde(default)]
    pub billing_tags: Option<Vec<String>>,
    /// Updated traits
    #[serde(default)]
    pub traits: Option<serde_json::Value>,
    /// Updated public flag
    #[serde(default)]
    pub public: Option<bool>,
    /// Updated state
    #[serde(default)]
    pub state: Option<String>,
    /// Updated error
    #[serde(default)]
    pub error: Option<super::image::ImageError>,
}

// ============================================================================
// POST /images action dispatch (create actions)
// ============================================================================

/// Actions available via POST /images (in addition to the default create)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CreateImageAction {
    /// Create image from an existing VM
    CreateFromVm,
    /// Import a Docker image (admin-only, streaming)
    ImportDockerImage,
    /// Import an LXD image (admin-only, streaming)
    ImportLxdImage,
    /// Import from another datacenter
    ImportFromDatacenter,
}

/// Query parameters for POST /images action dispatch
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateImageActionQuery {
    /// Action to perform (if omitted, creates from manifest body)
    #[serde(default)]
    pub action: Option<CreateImageAction>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

/// Request body for `create-from-vm` action
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateImageFromVmRequest {
    /// UUID of the VM to create the image from
    pub vm_uuid: Uuid,
    /// Image name
    pub name: String,
    /// Image version
    pub version: String,
    /// Image description
    #[serde(default)]
    pub description: Option<String>,
    /// Homepage URL
    #[serde(default)]
    pub homepage: Option<String>,
    /// EULA URL
    #[serde(default)]
    pub eula: Option<String>,
    /// ACL
    #[serde(default)]
    pub acl: Option<Vec<Uuid>>,
    /// Tags
    #[serde(default)]
    pub tags: Option<serde_json::Value>,
    /// Whether to make incremental
    #[serde(default)]
    pub incremental: Option<bool>,
    /// Maximum origin chain depth
    #[serde(default)]
    pub max_origin_depth: Option<u32>,
    /// Image OS
    #[serde(default)]
    pub os: Option<ImageOs>,
    /// Image type
    #[serde(default, rename = "type")]
    pub image_type: Option<ImageType>,
}

// ============================================================================
// POST /images/:uuid/acl action dispatch
// ============================================================================

/// Actions available via POST /images/:uuid/acl
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AclAction {
    /// Add UUIDs to the ACL
    Add,
    /// Remove UUIDs from the ACL
    Remove,
}

/// Query parameters for POST /images/:uuid/acl
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AclActionQuery {
    /// Action to perform (defaults to "add" if omitted)
    #[serde(default)]
    pub action: Option<AclAction>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// POST /state action dispatch
// ============================================================================

/// Actions available via POST /state
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StateAction {
    /// Drop all caches
    Dropcaches,
}

/// Query parameters for POST /state
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StateActionQuery {
    /// Action to perform
    pub action: StateAction,
}

// ============================================================================
// POST /images/:uuid/push
// ============================================================================

/// Query parameters for admin push (Docker image push)
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AdminPushQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

// ============================================================================
// POST /images/:uuid/clone
// ============================================================================

/// Query parameters for clone image
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloneImageQuery {
    /// Account UUID (required for clone)
    pub account: Uuid,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
}

// ============================================================================
// GET /images/:uuid/file query
// ============================================================================

/// Query parameters for getting an image file
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetImageFileQuery {
    /// Index of the file to retrieve (default: 0)
    #[serde(default)]
    pub index: Option<u32>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// GET /images/:uuid/icon query
// ============================================================================

/// Query parameters for getting an image icon
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetImageIconQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// DELETE /images/:uuid query
// ============================================================================

/// Query parameters for deleting an image
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteImageQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Force delete even if there are dependent images
    #[serde(default)]
    pub force_all_channels: Option<bool>,
}

// ============================================================================
// DELETE /images/:uuid/icon query
// ============================================================================

/// Query parameters for deleting an image icon
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteImageIconQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// GET /images/:uuid/jobs query
// ============================================================================

/// Query parameters for listing image jobs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListImageJobsQuery {
    /// Filter by job task name
    #[serde(default)]
    pub task: Option<String>,
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
}

// ============================================================================
// POST /images/:uuid/file/from-url query
// ============================================================================

/// Query parameters for adding an image file from URL
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddImageFileFromUrlQuery {
    /// Channel name
    #[serde(default)]
    pub channel: Option<String>,
    /// Account UUID
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Storage backend to use
    #[serde(default)]
    pub storage: Option<StorageType>,
}
