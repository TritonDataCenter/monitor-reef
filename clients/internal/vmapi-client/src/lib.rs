// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

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

// Include the Progenitor-generated client code
// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/client.rs"));
}
pub use generated::*;

// Re-export types from the API crate for convenience.
// Note: VmAction is NOT re-exported because Progenitor generates its own type.
// Use types::VmAction for the generated enum compatible with the client builder API.
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
    MigrationAction,
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
    VmActionQuery,
    VmJobsPath,
    VmMetadataKeyPath,
    VmPath,
    VmState,
    VmStatus,
};

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
        let body = StartVmRequest {
            idempotent: if idempotent { Some(true) } else { None },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Start)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = StopVmRequest {
            idempotent: if idempotent { Some(true) } else { None },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Stop)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = KillVmRequest {
            signal,
            idempotent: if idempotent { Some(true) } else { None },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Kill)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = RebootVmRequest {
            idempotent: if idempotent { Some(true) } else { None },
        };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Reboot)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = ReprovisionVmRequest { image_uuid };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Reprovision)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Update)
            .body(serde_json::to_value(request).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::AddNics)
            .body(serde_json::to_value(request).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::UpdateNics)
            .body(serde_json::to_value(request).unwrap_or_default())
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
        let body = RemoveNicsRequest { macs };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::RemoveNics)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = CreateSnapshotRequest { snapshot_name };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::CreateSnapshot)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = RollbackSnapshotRequest { snapshot_name };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::RollbackSnapshot)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        let body = DeleteSnapshotRequest { snapshot_name };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::DeleteSnapshot)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::CreateDisk)
            .body(serde_json::to_value(request).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::ResizeDisk)
            .body(serde_json::to_value(request).unwrap_or_default())
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
        let body = DeleteDiskRequest { pci_slot };
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::DeleteDisk)
            .body(serde_json::to_value(&body).unwrap_or_default())
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
        self.inner
            .vm_action()
            .uuid(*uuid)
            .action(types::VmAction::Migrate)
            .body(serde_json::to_value(request).unwrap_or_default())
            .send()
            .await
            .map(|r| r.into_inner())
    }
}
