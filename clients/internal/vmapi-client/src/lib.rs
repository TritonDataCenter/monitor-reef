// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

// When implementing changefeed it became brutally apparent that the
// json payloads emitted by CloudAPI on the changefeed are pretty much
// raw VMAPI changefeed payloads. At that point, rather than encode
// all of the VMAPI types directly inside the CloudAPI trait, it felt
// prudent to have Claude build out all the VMAPI types in a vmapi-api crate.
// So we pointed the Claude at all of the restify->dropshot tooling
// and out popped everything: the API, the client, and the CLI.

// If this message is still here, then this crate is probably still
// relatively untested.

//! VMAPI Client Library
//!
//! This client provides typed access to the Triton VMAPI service.
//! VMAPI is an internal HTTP API for managing virtual machines in a Triton datacenter.
//!
//! ## Usage
//!
//! ### Basic Client
//!
//! For direct API access (internal Triton services):
//!
//! ```ignore
//! use vmapi_client::Client;
//!
//! let client = Client::new("http://vmapi.my-dc.my-cloud.local");
//!
//! // List VMs
//! let vms = client.list_vms().send().await?;
//!
//! // Get a specific VM
//! let vm = client.get_vm().uuid(vm_uuid).send().await?;
//! ```
//!
//! ### TypedClient for Action-based Endpoints
//!
//! For VM action endpoints, use the typed wrapper methods for better ergonomics:
//!
//! ```ignore
//! use vmapi_client::TypedClient;
//!
//! let client = TypedClient::new("http://vmapi.my-dc.my-cloud.local");
//!
//! // Typed VM actions
//! client.start_vm(&vm_uuid, false).await?;
//! client.stop_vm(&vm_uuid, false).await?;
//! client.reboot_vm(&vm_uuid, false).await?;
//!
//! // Update VM with typed request
//! let update = vmapi_api::UpdateVmRequest {
//!     ram: Some(2048),
//!     ..Default::default()
//! };
//! client.update_vm(&vm_uuid, &update).await?;
//!
//! // Access underlying client for other operations
//! let jobs = client.inner().list_jobs().send().await?;
//! ```

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use vmapi_api::{
    // Metadata types
    AddMetadataRequest,
    // Action request types
    AddNicsRequest,
    // Role tag types
    AddRoleTagsRequest,
    // Common types
    Brand,
    CreateDiskRequest,
    CreateSnapshotRequest,
    // VM types
    CreateVmRequest,
    DeleteDiskRequest,
    DeleteSnapshotRequest,
    Disk,
    // Status types
    GetStatusesQuery,
    // Job types
    Job,
    JobExecution,
    JobPath,
    JobResponse,
    KillVmRequest,
    ListJobsQuery,
    // Migration types
    ListMigrationsQuery,
    ListVmsQuery,
    MetadataObject,
    MetadataResponse,
    MetadataType,
    MetadataValueResponse,
    MigrateVmRequest,
    Migration,
    MigrationPath,
    MigrationPhase,
    MigrationProgress,
    MigrationProgressRequest,
    MigrationState,
    Nic,
    NicSpec,
    PingResponse,
    PostJobResultsRequest,
    PutVmRequest,
    PutVmsRequest,
    RebootVmRequest,
    RemoveNicsRequest,
    ReprovisionVmRequest,
    ResizeDiskRequest,
    RoleTagPath,
    RoleTagsPath,
    RoleTagsResponse,
    RollbackSnapshotRequest,
    SetMetadataRequest,
    SetRoleTagsRequest,
    Snapshot,
    SnapshotState,
    StartVmRequest,
    StatusesResponse,
    StopVmRequest,
    StoreMigrationRecordRequest,
    Tags,
    TaskChainEntry,
    TaskResult,
    Timestamp,
    UpdateNicsRequest,
    UpdateVmRequest,
    UpdateVmServerUuidRequest,
    Uuid,
    Vm,
    VmAction,
    VmActionQuery,
    VmJobsPath,
    VmMetadataKeyPath,
    VmPath,
    VmState,
    VmStatus,
};

/// Serialize a request type to JSON Value, panicking on failure.
///
/// All request types in this crate are simple structs (String, Option, Uuid
/// fields) whose serialization cannot fail. This replaces the previous
/// `unwrap_or_default()` pattern which silently produced `Value::Null` on
/// error, masking bugs as confusing server-side failures.
#[allow(clippy::expect_used)]
fn to_json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("request serialization should not fail")
}

/// Wraps an action enum + per-action request body into a single JSON object.
/// Produces `{"action": "<variant>", ...body_fields}` via `#[serde(flatten)]`.
///
/// Node.js Restify's `mapParams: true` merges query and body params, so
/// clients may send `action` in either place. We send it in the body to
/// match the established wire format.
#[derive(serde::Serialize)]
struct ActionBody<A: serde::Serialize, B: serde::Serialize> {
    action: A,
    #[serde(flatten)]
    body: B,
}

/// Typed client wrapper for action-based endpoints
///
/// This wrapper provides ergonomic methods for VMAPI's action-based endpoints
/// (VM actions) while still allowing access to the underlying Progenitor-generated
/// client for all other operations.
pub struct TypedClient {
    inner: Client,
}

impl TypedClient {
    /// Create a new typed client wrapper
    ///
    /// # Arguments
    /// * `base_url` - VMAPI base URL (e.g., "http://vmapi.my-dc.my-cloud.local")
    pub fn new(base_url: &str) -> Self {
        Self {
            inner: Client::new(base_url),
        }
    }

    /// Create a typed client with a custom reqwest client
    ///
    /// # Arguments
    /// * `base_url` - VMAPI base URL
    /// * `http_client` - Custom reqwest client
    pub fn new_with_client(base_url: &str, http_client: reqwest::Client) -> Self {
        Self {
            inner: Client::new_with_client(base_url, http_client),
        }
    }

    /// Access the underlying Progenitor client for non-wrapped methods
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    // ========================================================================
    // VM Actions
    // ========================================================================

    /// Start a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `idempotent` - If true, don't error if VM is already running
    pub async fn start_vm(
        &self,
        uuid: &Uuid,
        idempotent: bool,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Start,
            body: StartVmRequest {
                idempotent: if idempotent { Some(true) } else { None },
            },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Stop a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `idempotent` - If true, don't error if VM is already stopped
    pub async fn stop_vm(
        &self,
        uuid: &Uuid,
        idempotent: bool,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Stop,
            body: StopVmRequest {
                idempotent: if idempotent { Some(true) } else { None },
            },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Kill a VM (send signal)
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `signal` - Optional signal to send (default: SIGKILL)
    /// * `idempotent` - If true, don't error if VM is already stopped
    pub async fn kill_vm(
        &self,
        uuid: &Uuid,
        signal: Option<String>,
        idempotent: bool,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Kill,
            body: KillVmRequest {
                signal,
                idempotent: if idempotent { Some(true) } else { None },
            },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Reboot a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `idempotent` - If true, don't error if VM is not running
    pub async fn reboot_vm(
        &self,
        uuid: &Uuid,
        idempotent: bool,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Reboot,
            body: RebootVmRequest {
                idempotent: if idempotent { Some(true) } else { None },
            },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Reprovision a VM with a new image
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `image_uuid` - Image UUID to reprovision with
    pub async fn reprovision_vm(
        &self,
        uuid: &Uuid,
        image_uuid: Uuid,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Reprovision,
            body: ReprovisionVmRequest { image_uuid },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Update VM properties
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Update request with fields to change
    pub async fn update_vm(
        &self,
        uuid: &Uuid,
        request: &UpdateVmRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Update,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Add NICs to a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Add NICs request (networks or macs)
    pub async fn add_nics(
        &self,
        uuid: &Uuid,
        request: &AddNicsRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::AddNics,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Update NICs on a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Update NICs request
    pub async fn update_nics(
        &self,
        uuid: &Uuid,
        request: &UpdateNicsRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::UpdateNics,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Remove NICs from a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `macs` - MAC addresses of NICs to remove
    pub async fn remove_nics(
        &self,
        uuid: &Uuid,
        macs: Vec<String>,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::RemoveNics,
            body: RemoveNicsRequest { macs },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Create a snapshot of a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `snapshot_name` - Optional snapshot name (auto-generated if not provided)
    pub async fn create_snapshot(
        &self,
        uuid: &Uuid,
        snapshot_name: Option<String>,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::CreateSnapshot,
            body: CreateSnapshotRequest { snapshot_name },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Rollback a VM to a snapshot
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `snapshot_name` - Name of the snapshot to rollback to
    pub async fn rollback_snapshot(
        &self,
        uuid: &Uuid,
        snapshot_name: String,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::RollbackSnapshot,
            body: RollbackSnapshotRequest { snapshot_name },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Delete a snapshot from a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `snapshot_name` - Name of the snapshot to delete
    pub async fn delete_snapshot(
        &self,
        uuid: &Uuid,
        snapshot_name: String,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::DeleteSnapshot,
            body: DeleteSnapshotRequest { snapshot_name },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Create a disk for a bhyve VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Create disk request
    pub async fn create_disk(
        &self,
        uuid: &Uuid,
        request: &CreateDiskRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::CreateDisk,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Resize a disk on a bhyve VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Resize disk request
    pub async fn resize_disk(
        &self,
        uuid: &Uuid,
        request: &ResizeDiskRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::ResizeDisk,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Delete a disk from a bhyve VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `pci_slot` - PCI slot of the disk to delete
    pub async fn delete_disk(
        &self,
        uuid: &Uuid,
        pci_slot: String,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::DeleteDisk,
            body: DeleteDiskRequest { pci_slot },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Migrate a VM
    ///
    /// # Arguments
    /// * `uuid` - VM UUID
    /// * `request` - Migration request with action and optional target
    pub async fn migrate_vm(
        &self,
        uuid: &Uuid,
        request: &MigrateVmRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        let body = ActionBody {
            action: VmAction::Migrate,
            body: request,
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .body(to_json_value(&body))
            .send()
            .await
            .map(|r| r.into_inner())
    }
}
