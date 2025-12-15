// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Triton CloudAPI trait definition
//!
//! This crate defines the API trait for Triton's CloudAPI service (version 9.20.0).
//! CloudAPI provides the public-facing REST API for managing virtual machines,
//! images, networks, volumes, and other resources in Triton.

use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query,
    RequestContext, TypedBody, WebsocketChannelResult, WebsocketConnection,
};

pub mod types;
pub use types::*;

/// URL for CloudAPI documentation
///
/// The Node.js CloudAPI has documentation redirect endpoints at `/`, `/docs`, and
/// `/favicon.ico`. These cannot be represented in the Dropshot API trait because
/// they conflict with `/{account}` variable path routing. Dropshot does not allow
/// both literal segments (e.g., `/docs`) and variable segments (e.g., `/{account}`)
/// at the same path depth.
///
/// Service implementations should handle these redirects at the reverse proxy or
/// HTTP server level before routing to the Dropshot API.
pub const DOCS_URL: &str = "http://apidocs.tritondatacenter.com/cloudapi/";

/// URL for favicon
///
/// See `DOCS_URL` documentation for why this is not an API endpoint.
pub const FAVICON_URL: &str = "http://apidocs.tritondatacenter.com/favicon.ico";

/// CloudAPI trait definition
///
/// This trait defines all endpoints of the Triton CloudAPI service (version 9.20.0).
/// The API is organized into the following categories:
/// - Account management
/// - Machines (VMs) and their resources (metadata, tags, snapshots, audit)
/// - Images/datasets
/// - Networks (including fabric VLANs and networks)
/// - Volumes
/// - Firewall rules
/// - Users, roles, and policies (RBAC)
/// - SSH keys and access keys
/// - Packages
/// - Datacenters
/// - Services
/// - Migrations
///
/// Note: Documentation redirect endpoints (`/`, `/docs`, `/favicon.ico`) from the
/// Node.js CloudAPI cannot be included due to Dropshot routing limitations.
/// See `DOCS_URL` constant for details. These should be handled at the reverse
/// proxy or HTTP server level.
#[dropshot::api_description]
pub trait CloudApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Account Endpoints
    // ========================================================================

    /// Get account details
    #[endpoint {
        method = GET,
        path = "/{account}",
        tags = ["account"],
    }]
    async fn get_account(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Account>, HttpError>;

    /// Head account details
    #[endpoint {
        method = HEAD,
        path = "/{account}",
        tags = ["account"],
    }]
    async fn head_account(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Account>, HttpError>;

    /// Update account
    #[endpoint {
        method = POST,
        path = "/{account}",
        tags = ["account"],
    }]
    async fn update_account(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<UpdateAccountRequest>,
    ) -> Result<HttpResponseOk<Account>, HttpError>;

    /// Get provisioning limits
    #[endpoint {
        method = GET,
        path = "/{account}/limits",
        tags = ["account"],
    }]
    async fn get_provisioning_limits(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<ProvisioningLimits>, HttpError>;

    // ========================================================================
    // Machine Endpoints
    // ========================================================================

    /// Create a machine
    #[endpoint {
        method = POST,
        path = "/{account}/machines",
        tags = ["machines"],
    }]
    async fn create_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateMachineRequest>,
    ) -> Result<HttpResponseCreated<Machine>, HttpError>;

    /// List machines
    #[endpoint {
        method = GET,
        path = "/{account}/machines",
        tags = ["machines"],
    }]
    async fn list_machines(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        query: Query<ListMachinesQuery>,
    ) -> Result<HttpResponseOk<Vec<Machine>>, HttpError>;

    /// Head machines
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines",
        tags = ["machines"],
    }]
    async fn head_machines(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        query: Query<ListMachinesQuery>,
    ) -> Result<HttpResponseOk<Vec<Machine>>, HttpError>;

    /// Get a machine
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}",
        tags = ["machines"],
    }]
    async fn get_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Machine>, HttpError>;

    /// Head a machine
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}",
        tags = ["machines"],
    }]
    async fn head_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Machine>, HttpError>;

    /// Update a machine (action dispatch)
    ///
    /// This endpoint handles multiple actions via the action query parameter:
    /// - start: Start a stopped machine
    /// - stop: Stop a running machine
    /// - reboot: Reboot a running machine
    /// - resize: Resize to a different package
    /// - rename: Change machine name/alias
    /// - enable_firewall: Enable firewall
    /// - disable_firewall: Disable firewall
    /// - enable_deletion_protection: Enable deletion protection
    /// - disable_deletion_protection: Disable deletion protection
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}",
        tags = ["machines"],
    }]
    async fn update_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        query: Query<MachineActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<Machine>, HttpError>;

    /// Delete a machine
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}",
        tags = ["machines"],
    }]
    async fn delete_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Machine Audit
    // ========================================================================

    /// Get machine audit log
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/audit",
        tags = ["machines"],
    }]
    async fn machine_audit(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<AuditEntry>>, HttpError>;

    /// Head machine audit log
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/audit",
        tags = ["machines"],
    }]
    async fn head_audit(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<AuditEntry>>, HttpError>;

    // ========================================================================
    // Machine Metadata
    // ========================================================================

    /// Add machine metadata
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/metadata",
        tags = ["machines", "metadata"],
    }]
    async fn add_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<AddMetadataRequest>,
    ) -> Result<HttpResponseOk<Metadata>, HttpError>;

    /// List machine metadata
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/metadata",
        tags = ["machines", "metadata"],
    }]
    async fn list_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Metadata>, HttpError>;

    /// Head machine metadata
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/metadata",
        tags = ["machines", "metadata"],
    }]
    async fn head_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Metadata>, HttpError>;

    /// Get machine metadata key
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/metadata/{key}",
        tags = ["machines", "metadata"],
    }]
    async fn get_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetadataKeyPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// Head machine metadata key
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/metadata/{key}",
        tags = ["machines", "metadata"],
    }]
    async fn head_machine_metadata_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetadataKeyPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// Delete all machine metadata
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/metadata",
        tags = ["machines", "metadata"],
    }]
    async fn delete_all_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Delete machine metadata key
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/metadata/{key}",
        tags = ["machines", "metadata"],
    }]
    async fn delete_machine_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<MetadataKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Machine Tags
    // ========================================================================

    /// Add machine tags
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/tags",
        tags = ["machines", "tags"],
    }]
    async fn add_machine_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<TagsRequest>,
    ) -> Result<HttpResponseOk<Tags>, HttpError>;

    /// Replace machine tags
    #[endpoint {
        method = PUT,
        path = "/{account}/machines/{machine}/tags",
        tags = ["machines", "tags"],
    }]
    async fn replace_machine_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<TagsRequest>,
    ) -> Result<HttpResponseOk<Tags>, HttpError>;

    /// List machine tags
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/tags",
        tags = ["machines", "tags"],
    }]
    async fn list_machine_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Tags>, HttpError>;

    /// Head machine tags
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/tags",
        tags = ["machines", "tags"],
    }]
    async fn head_machine_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Tags>, HttpError>;

    /// Get machine tag
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/tags/{tag}",
        tags = ["machines", "tags"],
    }]
    async fn get_machine_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<TagPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// Head machine tag
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/tags/{tag}",
        tags = ["machines", "tags"],
    }]
    async fn head_machine_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<TagPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// Delete all machine tags
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/tags",
        tags = ["machines", "tags"],
    }]
    async fn delete_machine_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Delete machine tag
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/tags/{tag}",
        tags = ["machines", "tags"],
    }]
    async fn delete_machine_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<TagPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Machine Snapshots
    // ========================================================================

    /// Create machine snapshot
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/snapshots",
        tags = ["machines", "snapshots"],
    }]
    async fn create_machine_snapshot(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<CreateSnapshotRequest>,
    ) -> Result<HttpResponseCreated<Snapshot>, HttpError>;

    /// Start machine from snapshot
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/snapshots/{name}",
        tags = ["machines", "snapshots"],
    }]
    async fn start_machine_from_snapshot(
        rqctx: RequestContext<Self::Context>,
        path: Path<SnapshotPath>,
    ) -> Result<HttpResponseOk<Machine>, HttpError>;

    /// List machine snapshots
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/snapshots",
        tags = ["machines", "snapshots"],
    }]
    async fn list_machine_snapshots(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Snapshot>>, HttpError>;

    /// Head machine snapshots
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/snapshots",
        tags = ["machines", "snapshots"],
    }]
    async fn head_machine_snapshots(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Snapshot>>, HttpError>;

    /// Get machine snapshot
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/snapshots/{name}",
        tags = ["machines", "snapshots"],
    }]
    async fn get_machine_snapshot(
        rqctx: RequestContext<Self::Context>,
        path: Path<SnapshotPath>,
    ) -> Result<HttpResponseOk<Snapshot>, HttpError>;

    /// Head machine snapshot
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/snapshots/{name}",
        tags = ["machines", "snapshots"],
    }]
    async fn head_machine_snapshot(
        rqctx: RequestContext<Self::Context>,
        path: Path<SnapshotPath>,
    ) -> Result<HttpResponseOk<Snapshot>, HttpError>;

    /// Delete machine snapshot
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/snapshots/{name}",
        tags = ["machines", "snapshots"],
    }]
    async fn delete_machine_snapshot(
        rqctx: RequestContext<Self::Context>,
        path: Path<SnapshotPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Machine NICs
    // ========================================================================

    /// Add NIC to machine
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/nics",
        tags = ["machines", "nics"],
    }]
    async fn add_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<AddNicRequest>,
    ) -> Result<HttpResponseCreated<Nic>, HttpError>;

    /// List machine NICs
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/nics",
        tags = ["machines", "nics"],
    }]
    async fn list_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError>;

    /// Head machine NICs
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/nics",
        tags = ["machines", "nics"],
    }]
    async fn head_nics(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Nic>>, HttpError>;

    /// Get machine NIC
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/nics/{mac}",
        tags = ["machines", "nics"],
    }]
    async fn get_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// Head machine NIC
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/nics/{mac}",
        tags = ["machines", "nics"],
    }]
    async fn head_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
    ) -> Result<HttpResponseOk<Nic>, HttpError>;

    /// Remove NIC from machine
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/nics/{mac}",
        tags = ["machines", "nics"],
    }]
    async fn remove_nic(
        rqctx: RequestContext<Self::Context>,
        path: Path<NicPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Machine Disks
    // ========================================================================

    /// Create machine disk
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/disks",
        tags = ["machines", "disks"],
    }]
    async fn create_machine_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<CreateDiskRequest>,
    ) -> Result<HttpResponseCreated<Disk>, HttpError>;

    /// List machine disks
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/disks",
        tags = ["machines", "disks"],
    }]
    async fn list_machine_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError>;

    /// Head machine disks
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/disks",
        tags = ["machines", "disks"],
    }]
    async fn head_machine_disks(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<Disk>>, HttpError>;

    /// Get machine disk
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/disks/{disk}",
        tags = ["machines", "disks"],
    }]
    async fn get_machine_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<DiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// Head machine disk
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/disks/{disk}",
        tags = ["machines", "disks"],
    }]
    async fn head_machine_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<DiskPath>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// Resize machine disk (action dispatch)
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/disks/{disk}",
        tags = ["machines", "disks"],
    }]
    async fn resize_machine_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<DiskPath>,
        query: Query<DiskActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<Disk>, HttpError>;

    /// Delete machine disk
    #[endpoint {
        method = DELETE,
        path = "/{account}/machines/{machine}/disks/{disk}",
        tags = ["machines", "disks"],
    }]
    async fn delete_machine_disk(
        rqctx: RequestContext<Self::Context>,
        path: Path<DiskPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Images/Datasets
    // ========================================================================

    /// List images
    #[endpoint {
        method = GET,
        path = "/{account}/images",
        tags = ["images"],
    }]
    async fn list_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        query: Query<ListImagesQuery>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Head images
    #[endpoint {
        method = HEAD,
        path = "/{account}/images",
        tags = ["images"],
    }]
    async fn head_images(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        query: Query<ListImagesQuery>,
    ) -> Result<HttpResponseOk<Vec<Image>>, HttpError>;

    /// Get image
    #[endpoint {
        method = GET,
        path = "/{account}/images/{dataset}",
        tags = ["images"],
    }]
    async fn get_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Head image
    #[endpoint {
        method = HEAD,
        path = "/{account}/images/{dataset}",
        tags = ["images"],
    }]
    async fn head_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Create image from machine
    #[endpoint {
        method = POST,
        path = "/{account}/images",
        tags = ["images"],
    }]
    async fn create_image_from_machine(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateImageRequest>,
    ) -> Result<HttpResponseCreated<Image>, HttpError>;

    /// Update image (action dispatch)
    ///
    /// This endpoint handles multiple actions via the action query parameter:
    /// - update: Update image metadata
    /// - export: Export image to Manta
    /// - clone: Clone image to account
    /// - import-from-datacenter: Import image from another datacenter
    #[endpoint {
        method = POST,
        path = "/{account}/images/{dataset}",
        tags = ["images"],
    }]
    async fn update_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        query: Query<ImageActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<Image>, HttpError>;

    /// Delete image
    #[endpoint {
        method = DELETE,
        path = "/{account}/images/{dataset}",
        tags = ["images"],
    }]
    async fn delete_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Packages
    // ========================================================================

    /// List packages
    #[endpoint {
        method = GET,
        path = "/{account}/packages",
        tags = ["packages"],
    }]
    async fn list_packages(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Package>>, HttpError>;

    /// Head packages
    #[endpoint {
        method = HEAD,
        path = "/{account}/packages",
        tags = ["packages"],
    }]
    async fn head_packages(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Package>>, HttpError>;

    /// Get package
    #[endpoint {
        method = GET,
        path = "/{account}/packages/{package}",
        tags = ["packages"],
    }]
    async fn get_package(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
    ) -> Result<HttpResponseOk<Package>, HttpError>;

    /// Head package
    #[endpoint {
        method = HEAD,
        path = "/{account}/packages/{package}",
        tags = ["packages"],
    }]
    async fn head_package(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
    ) -> Result<HttpResponseOk<Package>, HttpError>;

    // ========================================================================
    // Networks
    // ========================================================================

    /// List networks
    #[endpoint {
        method = GET,
        path = "/{account}/networks",
        tags = ["networks"],
    }]
    async fn list_networks(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Head networks
    #[endpoint {
        method = HEAD,
        path = "/{account}/networks",
        tags = ["networks"],
    }]
    async fn head_networks(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Get network
    #[endpoint {
        method = GET,
        path = "/{account}/networks/{network}",
        tags = ["networks"],
    }]
    async fn get_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Head network
    #[endpoint {
        method = HEAD,
        path = "/{account}/networks/{network}",
        tags = ["networks"],
    }]
    async fn head_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    // ========================================================================
    // Network IPs
    // ========================================================================

    /// List network IPs
    #[endpoint {
        method = GET,
        path = "/{account}/networks/{network}/ips",
        tags = ["networks"],
    }]
    async fn list_network_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseOk<Vec<NetworkIp>>, HttpError>;

    /// Head network IPs
    #[endpoint {
        method = HEAD,
        path = "/{account}/networks/{network}/ips",
        tags = ["networks"],
    }]
    async fn head_network_ips(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
    ) -> Result<HttpResponseOk<Vec<NetworkIp>>, HttpError>;

    /// Get network IP
    #[endpoint {
        method = GET,
        path = "/{account}/networks/{network}/ips/{ip_address}",
        tags = ["networks"],
    }]
    async fn get_network_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkIpPath>,
    ) -> Result<HttpResponseOk<NetworkIp>, HttpError>;

    /// Head network IP
    #[endpoint {
        method = HEAD,
        path = "/{account}/networks/{network}/ips/{ip_address}",
        tags = ["networks"],
    }]
    async fn head_network_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkIpPath>,
    ) -> Result<HttpResponseOk<NetworkIp>, HttpError>;

    /// Update network IP
    #[endpoint {
        method = PUT,
        path = "/{account}/networks/{network}/ips/{ip_address}",
        tags = ["networks"],
    }]
    async fn update_network_ip(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkIpPath>,
        body: TypedBody<UpdateNetworkIpRequest>,
    ) -> Result<HttpResponseOk<NetworkIp>, HttpError>;

    // ========================================================================
    // Fabric VLANs
    // ========================================================================

    /// List fabric VLANs
    #[endpoint {
        method = GET,
        path = "/{account}/fabrics/default/vlans",
        tags = ["fabrics"],
    }]
    async fn list_fabric_vlans(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<FabricVlan>>, HttpError>;

    /// Head fabric VLANs
    #[endpoint {
        method = HEAD,
        path = "/{account}/fabrics/default/vlans",
        tags = ["fabrics"],
    }]
    async fn head_fabric_vlans(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<FabricVlan>>, HttpError>;

    /// Create fabric VLAN
    #[endpoint {
        method = POST,
        path = "/{account}/fabrics/default/vlans",
        tags = ["fabrics"],
    }]
    async fn create_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateFabricVlanRequest>,
    ) -> Result<HttpResponseCreated<FabricVlan>, HttpError>;

    /// Get fabric VLAN
    #[endpoint {
        method = GET,
        path = "/{account}/fabrics/default/vlans/{vlan_id}",
        tags = ["fabrics"],
    }]
    async fn get_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Head fabric VLAN
    #[endpoint {
        method = HEAD,
        path = "/{account}/fabrics/default/vlans/{vlan_id}",
        tags = ["fabrics"],
    }]
    async fn head_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Update fabric VLAN
    #[endpoint {
        method = PUT,
        path = "/{account}/fabrics/default/vlans/{vlan_id}",
        tags = ["fabrics"],
    }]
    async fn update_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
        body: TypedBody<UpdateFabricVlanRequest>,
    ) -> Result<HttpResponseOk<FabricVlan>, HttpError>;

    /// Delete fabric VLAN
    #[endpoint {
        method = DELETE,
        path = "/{account}/fabrics/default/vlans/{vlan_id}",
        tags = ["fabrics"],
    }]
    async fn delete_fabric_vlan(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Fabric Networks
    // ========================================================================

    /// List fabric networks
    #[endpoint {
        method = GET,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks",
        tags = ["fabrics"],
    }]
    async fn list_fabric_networks(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Head fabric networks
    #[endpoint {
        method = HEAD,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks",
        tags = ["fabrics"],
    }]
    async fn head_fabric_networks(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
    ) -> Result<HttpResponseOk<Vec<Network>>, HttpError>;

    /// Create fabric network
    #[endpoint {
        method = POST,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks",
        tags = ["fabrics"],
    }]
    async fn create_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricVlanPath>,
        body: TypedBody<CreateFabricNetworkRequest>,
    ) -> Result<HttpResponseCreated<Network>, HttpError>;

    /// Get fabric network
    #[endpoint {
        method = GET,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks/{id}",
        tags = ["fabrics"],
    }]
    async fn get_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Head fabric network
    #[endpoint {
        method = HEAD,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks/{id}",
        tags = ["fabrics"],
    }]
    async fn head_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Update fabric network
    #[endpoint {
        method = PUT,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks/{id}",
        tags = ["fabrics"],
    }]
    async fn update_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
        body: TypedBody<UpdateFabricNetworkRequest>,
    ) -> Result<HttpResponseOk<Network>, HttpError>;

    /// Delete fabric network
    #[endpoint {
        method = DELETE,
        path = "/{account}/fabrics/default/vlans/{vlan_id}/networks/{id}",
        tags = ["fabrics"],
    }]
    async fn delete_fabric_network(
        rqctx: RequestContext<Self::Context>,
        path: Path<FabricNetworkPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Firewall Rules
    // ========================================================================

    /// Create firewall rule
    #[endpoint {
        method = POST,
        path = "/{account}/fwrules",
        tags = ["firewall"],
    }]
    async fn create_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateFirewallRuleRequest>,
    ) -> Result<HttpResponseCreated<FirewallRule>, HttpError>;

    /// List firewall rules
    #[endpoint {
        method = GET,
        path = "/{account}/fwrules",
        tags = ["firewall"],
    }]
    async fn list_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError>;

    /// Head firewall rules
    #[endpoint {
        method = HEAD,
        path = "/{account}/fwrules",
        tags = ["firewall"],
    }]
    async fn head_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError>;

    /// Get firewall rule
    #[endpoint {
        method = GET,
        path = "/{account}/fwrules/{id}",
        tags = ["firewall"],
    }]
    async fn get_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// Head firewall rule
    #[endpoint {
        method = HEAD,
        path = "/{account}/fwrules/{id}",
        tags = ["firewall"],
    }]
    async fn head_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// Update firewall rule
    #[endpoint {
        method = POST,
        path = "/{account}/fwrules/{id}",
        tags = ["firewall"],
    }]
    async fn update_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
        body: TypedBody<UpdateFirewallRuleRequest>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// Enable firewall rule
    #[endpoint {
        method = POST,
        path = "/{account}/fwrules/{id}/enable",
        tags = ["firewall"],
    }]
    async fn enable_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// Disable firewall rule
    #[endpoint {
        method = POST,
        path = "/{account}/fwrules/{id}/disable",
        tags = ["firewall"],
    }]
    async fn disable_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<FirewallRule>, HttpError>;

    /// Delete firewall rule
    #[endpoint {
        method = DELETE,
        path = "/{account}/fwrules/{id}",
        tags = ["firewall"],
    }]
    async fn delete_firewall_rule(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List firewall rule machines
    #[endpoint {
        method = GET,
        path = "/{account}/fwrules/{id}/machines",
        tags = ["firewall"],
    }]
    async fn list_firewall_rule_machines(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<Vec<Machine>>, HttpError>;

    /// Head firewall rule machines
    #[endpoint {
        method = HEAD,
        path = "/{account}/fwrules/{id}/machines",
        tags = ["firewall"],
    }]
    async fn head_firewall_rule_machines(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
    ) -> Result<HttpResponseOk<Vec<Machine>>, HttpError>;

    /// List machine firewall rules
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/fwrules",
        tags = ["machines", "firewall"],
    }]
    async fn list_machine_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError>;

    /// Head machine firewall rules
    #[endpoint {
        method = HEAD,
        path = "/{account}/machines/{machine}/fwrules",
        tags = ["machines", "firewall"],
    }]
    async fn head_machine_firewall_rules(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Vec<FirewallRule>>, HttpError>;

    // ========================================================================
    // Users
    // ========================================================================

    /// Create user
    #[endpoint {
        method = POST,
        path = "/{account}/users",
        tags = ["users"],
    }]
    async fn create_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateUserRequest>,
    ) -> Result<HttpResponseCreated<User>, HttpError>;

    /// List users
    #[endpoint {
        method = GET,
        path = "/{account}/users",
        tags = ["users"],
    }]
    async fn list_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<User>>, HttpError>;

    /// Head users
    #[endpoint {
        method = HEAD,
        path = "/{account}/users",
        tags = ["users"],
    }]
    async fn head_users(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<User>>, HttpError>;

    /// Get user
    #[endpoint {
        method = GET,
        path = "/{account}/users/{uuid}",
        tags = ["users"],
    }]
    async fn get_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<User>, HttpError>;

    /// Head user
    #[endpoint {
        method = HEAD,
        path = "/{account}/users/{uuid}",
        tags = ["users"],
    }]
    async fn head_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<User>, HttpError>;

    /// Update user
    #[endpoint {
        method = POST,
        path = "/{account}/users/{uuid}",
        tags = ["users"],
    }]
    async fn update_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<UpdateUserRequest>,
    ) -> Result<HttpResponseOk<User>, HttpError>;

    /// Change user password
    #[endpoint {
        method = POST,
        path = "/{account}/users/{uuid}/change_password",
        tags = ["users"],
    }]
    async fn change_user_password(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<ChangePasswordRequest>,
    ) -> Result<HttpResponseOk<User>, HttpError>;

    /// Delete user
    #[endpoint {
        method = DELETE,
        path = "/{account}/users/{uuid}",
        tags = ["users"],
    }]
    async fn delete_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Roles
    // ========================================================================

    /// Create role
    #[endpoint {
        method = POST,
        path = "/{account}/roles",
        tags = ["roles"],
    }]
    async fn create_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateRoleRequest>,
    ) -> Result<HttpResponseCreated<Role>, HttpError>;

    /// List roles
    #[endpoint {
        method = GET,
        path = "/{account}/roles",
        tags = ["roles"],
    }]
    async fn list_roles(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Role>>, HttpError>;

    /// Head roles
    #[endpoint {
        method = HEAD,
        path = "/{account}/roles",
        tags = ["roles"],
    }]
    async fn head_roles(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Role>>, HttpError>;

    /// Get role
    #[endpoint {
        method = GET,
        path = "/{account}/roles/{role}",
        tags = ["roles"],
    }]
    async fn get_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePath>,
    ) -> Result<HttpResponseOk<Role>, HttpError>;

    /// Head role
    #[endpoint {
        method = HEAD,
        path = "/{account}/roles/{role}",
        tags = ["roles"],
    }]
    async fn head_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePath>,
    ) -> Result<HttpResponseOk<Role>, HttpError>;

    /// Update role
    #[endpoint {
        method = POST,
        path = "/{account}/roles/{role}",
        tags = ["roles"],
    }]
    async fn update_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePath>,
        body: TypedBody<UpdateRoleRequest>,
    ) -> Result<HttpResponseOk<Role>, HttpError>;

    /// Delete role
    #[endpoint {
        method = DELETE,
        path = "/{account}/roles/{role}",
        tags = ["roles"],
    }]
    async fn delete_role(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Policies
    // ========================================================================

    /// Create policy
    #[endpoint {
        method = POST,
        path = "/{account}/policies",
        tags = ["policies"],
    }]
    async fn create_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreatePolicyRequest>,
    ) -> Result<HttpResponseCreated<Policy>, HttpError>;

    /// List policies
    #[endpoint {
        method = GET,
        path = "/{account}/policies",
        tags = ["policies"],
    }]
    async fn list_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Policy>>, HttpError>;

    /// Head policies
    #[endpoint {
        method = HEAD,
        path = "/{account}/policies",
        tags = ["policies"],
    }]
    async fn head_policies(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Policy>>, HttpError>;

    /// Get policy
    #[endpoint {
        method = GET,
        path = "/{account}/policies/{policy}",
        tags = ["policies"],
    }]
    async fn get_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<PolicyPath>,
    ) -> Result<HttpResponseOk<Policy>, HttpError>;

    /// Head policy
    #[endpoint {
        method = HEAD,
        path = "/{account}/policies/{policy}",
        tags = ["policies"],
    }]
    async fn head_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<PolicyPath>,
    ) -> Result<HttpResponseOk<Policy>, HttpError>;

    /// Update policy
    #[endpoint {
        method = POST,
        path = "/{account}/policies/{policy}",
        tags = ["policies"],
    }]
    async fn update_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<PolicyPath>,
        body: TypedBody<UpdatePolicyRequest>,
    ) -> Result<HttpResponseOk<Policy>, HttpError>;

    /// Delete policy
    #[endpoint {
        method = DELETE,
        path = "/{account}/policies/{policy}",
        tags = ["policies"],
    }]
    async fn delete_policy(
        rqctx: RequestContext<Self::Context>,
        path: Path<PolicyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // SSH Keys
    // ========================================================================

    /// Create SSH key
    #[endpoint {
        method = POST,
        path = "/{account}/keys",
        tags = ["keys"],
    }]
    async fn create_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateSshKeyRequest>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List SSH keys
    #[endpoint {
        method = GET,
        path = "/{account}/keys",
        tags = ["keys"],
    }]
    async fn list_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Head SSH keys
    #[endpoint {
        method = HEAD,
        path = "/{account}/keys",
        tags = ["keys"],
    }]
    async fn head_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Get SSH key
    #[endpoint {
        method = GET,
        path = "/{account}/keys/{name}",
        tags = ["keys"],
    }]
    async fn get_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<KeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Head SSH key
    #[endpoint {
        method = HEAD,
        path = "/{account}/keys/{name}",
        tags = ["keys"],
    }]
    async fn head_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<KeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Delete SSH key
    #[endpoint {
        method = DELETE,
        path = "/{account}/keys/{name}",
        tags = ["keys"],
    }]
    async fn delete_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<KeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // User SSH Keys
    // ========================================================================

    /// Create user SSH key
    #[endpoint {
        method = POST,
        path = "/{account}/users/{uuid}/keys",
        tags = ["users", "keys"],
    }]
    async fn create_user_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<CreateSshKeyRequest>,
    ) -> Result<HttpResponseCreated<SshKey>, HttpError>;

    /// List user SSH keys
    #[endpoint {
        method = GET,
        path = "/{account}/users/{uuid}/keys",
        tags = ["users", "keys"],
    }]
    async fn list_user_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Head user SSH keys
    #[endpoint {
        method = HEAD,
        path = "/{account}/users/{uuid}/keys",
        tags = ["users", "keys"],
    }]
    async fn head_user_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError>;

    /// Get user SSH key
    #[endpoint {
        method = GET,
        path = "/{account}/users/{uuid}/keys/{name}",
        tags = ["users", "keys"],
    }]
    async fn get_user_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Head user SSH key
    #[endpoint {
        method = HEAD,
        path = "/{account}/users/{uuid}/keys/{name}",
        tags = ["users", "keys"],
    }]
    async fn head_user_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserKeyPath>,
    ) -> Result<HttpResponseOk<SshKey>, HttpError>;

    /// Delete user SSH key
    #[endpoint {
        method = DELETE,
        path = "/{account}/users/{uuid}/keys/{name}",
        tags = ["users", "keys"],
    }]
    async fn delete_user_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Access Keys
    // ========================================================================

    /// Create access key
    #[endpoint {
        method = POST,
        path = "/{account}/accesskeys",
        tags = ["accesskeys"],
    }]
    async fn create_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateAccessKeyRequest>,
    ) -> Result<HttpResponseCreated<CreateAccessKeyResponse>, HttpError>;

    /// List access keys
    #[endpoint {
        method = GET,
        path = "/{account}/accesskeys",
        tags = ["accesskeys"],
    }]
    async fn list_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<AccessKey>>, HttpError>;

    /// Head access keys
    #[endpoint {
        method = HEAD,
        path = "/{account}/accesskeys",
        tags = ["accesskeys"],
    }]
    async fn head_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<AccessKey>>, HttpError>;

    /// Get access key
    #[endpoint {
        method = GET,
        path = "/{account}/accesskeys/{accesskeyid}",
        tags = ["accesskeys"],
    }]
    async fn get_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccessKeyPath>,
    ) -> Result<HttpResponseOk<AccessKey>, HttpError>;

    /// Head access key
    #[endpoint {
        method = HEAD,
        path = "/{account}/accesskeys/{accesskeyid}",
        tags = ["accesskeys"],
    }]
    async fn head_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccessKeyPath>,
    ) -> Result<HttpResponseOk<AccessKey>, HttpError>;

    /// Delete access key
    #[endpoint {
        method = DELETE,
        path = "/{account}/accesskeys/{accesskeyid}",
        tags = ["accesskeys"],
    }]
    async fn delete_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccessKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // User Access Keys
    // ========================================================================

    /// Create user access key
    #[endpoint {
        method = POST,
        path = "/{account}/users/{uuid}/accesskeys",
        tags = ["users", "accesskeys"],
    }]
    async fn create_user_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<CreateAccessKeyRequest>,
    ) -> Result<HttpResponseCreated<CreateAccessKeyResponse>, HttpError>;

    /// List user access keys
    #[endpoint {
        method = GET,
        path = "/{account}/users/{uuid}/accesskeys",
        tags = ["users", "accesskeys"],
    }]
    async fn list_user_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<Vec<AccessKey>>, HttpError>;

    /// Head user access keys
    #[endpoint {
        method = HEAD,
        path = "/{account}/users/{uuid}/accesskeys",
        tags = ["users", "accesskeys"],
    }]
    async fn head_user_access_keys(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<Vec<AccessKey>>, HttpError>;

    /// Get user access key
    #[endpoint {
        method = GET,
        path = "/{account}/users/{uuid}/accesskeys/{accesskeyid}",
        tags = ["users", "accesskeys"],
    }]
    async fn get_user_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserAccessKeyPath>,
    ) -> Result<HttpResponseOk<AccessKey>, HttpError>;

    /// Head user access key
    #[endpoint {
        method = HEAD,
        path = "/{account}/users/{uuid}/accesskeys/{accesskeyid}",
        tags = ["users", "accesskeys"],
    }]
    async fn head_user_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserAccessKeyPath>,
    ) -> Result<HttpResponseOk<AccessKey>, HttpError>;

    /// Delete user access key
    #[endpoint {
        method = DELETE,
        path = "/{account}/users/{uuid}/accesskeys/{accesskeyid}",
        tags = ["users", "accesskeys"],
    }]
    async fn delete_user_access_key(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserAccessKeyPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Config
    // ========================================================================

    /// Get configuration
    #[endpoint {
        method = GET,
        path = "/{account}/config",
        tags = ["config"],
    }]
    async fn get_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Config>, HttpError>;

    /// Head configuration
    #[endpoint {
        method = HEAD,
        path = "/{account}/config",
        tags = ["config"],
    }]
    async fn head_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Config>, HttpError>;

    /// Update configuration
    #[endpoint {
        method = PUT,
        path = "/{account}/config",
        tags = ["config"],
    }]
    async fn update_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<UpdateConfigRequest>,
    ) -> Result<HttpResponseOk<Config>, HttpError>;

    // ========================================================================
    // Datacenters
    // ========================================================================

    /// List datacenters
    #[endpoint {
        method = GET,
        path = "/{account}/datacenters",
        tags = ["datacenters"],
    }]
    async fn list_datacenters(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Datacenter>>, HttpError>;

    /// Get datacenter
    #[endpoint {
        method = GET,
        path = "/{account}/datacenters/{dc}",
        tags = ["datacenters"],
    }]
    async fn get_datacenter(
        rqctx: RequestContext<Self::Context>,
        path: Path<DatacenterPath>,
    ) -> Result<HttpResponseOk<String>, HttpError>;

    /// List foreign datacenters
    #[endpoint {
        method = GET,
        path = "/{account}/foreigndatacenters",
        tags = ["datacenters"],
    }]
    async fn list_foreign_datacenters(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Datacenter>>, HttpError>;

    /// Add foreign datacenter
    #[endpoint {
        method = POST,
        path = "/{account}/foreigndatacenters",
        tags = ["datacenters"],
    }]
    async fn add_foreign_datacenter(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<AddForeignDatacenterRequest>,
    ) -> Result<HttpResponseCreated<Datacenter>, HttpError>;

    // ========================================================================
    // Services
    // ========================================================================

    /// List services
    #[endpoint {
        method = GET,
        path = "/{account}/services",
        tags = ["services"],
    }]
    async fn list_services(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Service>>, HttpError>;

    // ========================================================================
    // Volumes
    // ========================================================================

    /// List volume sizes
    #[endpoint {
        method = GET,
        path = "/{account}/volumesizes",
        tags = ["volumes"],
    }]
    async fn list_volume_sizes(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<VolumeSize>>, HttpError>;

    /// Get volume
    #[endpoint {
        method = GET,
        path = "/{account}/volumes/{id}",
        tags = ["volumes"],
    }]
    async fn get_volume(
        rqctx: RequestContext<Self::Context>,
        path: Path<VolumePath>,
    ) -> Result<HttpResponseOk<Volume>, HttpError>;

    /// List volumes
    #[endpoint {
        method = GET,
        path = "/{account}/volumes",
        tags = ["volumes"],
    }]
    async fn list_volumes(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Volume>>, HttpError>;

    /// Create volume
    #[endpoint {
        method = POST,
        path = "/{account}/volumes",
        tags = ["volumes"],
    }]
    async fn create_volume(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<CreateVolumeRequest>,
    ) -> Result<HttpResponseCreated<Volume>, HttpError>;

    /// Delete volume
    #[endpoint {
        method = DELETE,
        path = "/{account}/volumes/{id}",
        tags = ["volumes"],
    }]
    async fn delete_volume(
        rqctx: RequestContext<Self::Context>,
        path: Path<VolumePath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Update volume (action dispatch)
    #[endpoint {
        method = POST,
        path = "/{account}/volumes/{id}",
        tags = ["volumes"],
    }]
    async fn update_volume(
        rqctx: RequestContext<Self::Context>,
        path: Path<VolumePath>,
        query: Query<VolumeActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<Volume>, HttpError>;

    // ========================================================================
    // Migrations
    // ========================================================================

    /// List migrations
    #[endpoint {
        method = GET,
        path = "/{account}/migrations",
        tags = ["migrations"],
    }]
    async fn list_migrations(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
    ) -> Result<HttpResponseOk<Vec<Migration>>, HttpError>;

    /// Get migration
    #[endpoint {
        method = GET,
        path = "/{account}/migrations/{machine}",
        tags = ["migrations"],
    }]
    async fn get_migration(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<Migration>, HttpError>;

    /// Get migration estimate
    #[endpoint {
        method = GET,
        path = "/{account}/machines/{machine}/migrate",
        tags = ["machines", "migrations"],
    }]
    async fn migrate_machine_estimate(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
    ) -> Result<HttpResponseOk<MigrationEstimate>, HttpError>;

    /// Migrate machine
    #[endpoint {
        method = POST,
        path = "/{account}/machines/{machine}/migrate",
        tags = ["machines", "migrations"],
    }]
    async fn migrate(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<MigrateRequest>,
    ) -> Result<HttpResponseOk<Migration>, HttpError>;

    // ========================================================================
    // WebSocket Endpoints
    // ========================================================================

    /// Stream VM state changes via WebSocket
    ///
    /// Provides real-time notifications of VM state transitions for monitoring
    /// tools and dashboards. Requires WebSocket protocol upgrade.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/{account}/changefeed",
        tags = ["changefeed"],
    }]
    async fn get_changefeed(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    /// Connect to machine VNC console via WebSocket
    ///
    /// Provides browser-based VNC console access to KVM/bhyve virtual machines.
    /// Only available for running machines. Requires WebSocket protocol upgrade.
    /// Available since API version 8.4.0.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/{account}/machines/{machine}/vnc",
        tags = ["machines"],
    }]
    async fn get_machine_vnc(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    // ========================================================================
    // Role Tags (RBAC)
    // ========================================================================

    /// Replace account role tags
    ///
    /// This endpoint replaces all role tags on the account itself (the machines
    /// collection). Returns the resource name and the updated list of role tags.
    #[endpoint {
        method = PUT,
        path = "/{account}",
        tags = ["role-tags"],
    }]
    async fn replace_account_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace machine role tags
    ///
    /// Replaces all role tags on a specific machine resource.
    #[endpoint {
        method = PUT,
        path = "/{account}/machines/{machine}",
        tags = ["machines", "role-tags"],
    }]
    async fn replace_machine_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<MachinePath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    // ========================================================================
    // Collection-Level Role Tag Endpoints
    // ========================================================================
    //
    // These endpoints allow tagging the ability to list/create resources of a
    // given type. The Node.js CloudAPI uses generic paths like /{account}/{resource_name}
    // but those conflict with Dropshot's routing, so we provide explicit endpoints
    // for each valid resource type.

    /// Replace role tags on the users collection
    #[endpoint {
        method = PUT,
        path = "/{account}/users",
        tags = ["role-tags"],
    }]
    async fn replace_users_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the roles collection
    #[endpoint {
        method = PUT,
        path = "/{account}/roles",
        tags = ["role-tags"],
    }]
    async fn replace_roles_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the packages collection
    #[endpoint {
        method = PUT,
        path = "/{account}/packages",
        tags = ["role-tags"],
    }]
    async fn replace_packages_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the images collection
    #[endpoint {
        method = PUT,
        path = "/{account}/images",
        tags = ["role-tags"],
    }]
    async fn replace_images_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the policies collection
    #[endpoint {
        method = PUT,
        path = "/{account}/policies",
        tags = ["role-tags"],
    }]
    async fn replace_policies_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the keys collection
    #[endpoint {
        method = PUT,
        path = "/{account}/keys",
        tags = ["role-tags"],
    }]
    async fn replace_keys_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the datacenters collection
    #[endpoint {
        method = PUT,
        path = "/{account}/datacenters",
        tags = ["role-tags"],
    }]
    async fn replace_datacenters_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the firewall rules collection
    #[endpoint {
        method = PUT,
        path = "/{account}/fwrules",
        tags = ["role-tags"],
    }]
    async fn replace_fwrules_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the networks collection
    #[endpoint {
        method = PUT,
        path = "/{account}/networks",
        tags = ["role-tags"],
    }]
    async fn replace_networks_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on the services collection
    #[endpoint {
        method = PUT,
        path = "/{account}/services",
        tags = ["role-tags"],
    }]
    async fn replace_services_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<AccountPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    // ========================================================================
    // Individual Resource Role Tag Endpoints
    // ========================================================================
    //
    // These endpoints allow tagging individual resources. Note: machines already
    // has replace_machine_role_tags above. Datacenters and services are read-only
    // and don't support individual tagging.

    /// Replace role tags on a specific user
    #[endpoint {
        method = PUT,
        path = "/{account}/users/{uuid}",
        tags = ["role-tags"],
    }]
    async fn replace_user_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific role
    #[endpoint {
        method = PUT,
        path = "/{account}/roles/{role}",
        tags = ["role-tags"],
    }]
    async fn replace_role_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<RolePath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific package
    #[endpoint {
        method = PUT,
        path = "/{account}/packages/{package}",
        tags = ["role-tags"],
    }]
    async fn replace_package_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<PackagePath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific image
    #[endpoint {
        method = PUT,
        path = "/{account}/images/{dataset}",
        tags = ["role-tags"],
    }]
    async fn replace_image_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific policy
    #[endpoint {
        method = PUT,
        path = "/{account}/policies/{policy}",
        tags = ["role-tags"],
    }]
    async fn replace_policy_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<PolicyPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific SSH key
    #[endpoint {
        method = PUT,
        path = "/{account}/keys/{name}",
        tags = ["role-tags"],
    }]
    async fn replace_key_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<KeyPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific firewall rule
    #[endpoint {
        method = PUT,
        path = "/{account}/fwrules/{id}",
        tags = ["role-tags"],
    }]
    async fn replace_fwrule_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<FirewallRulePath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific network
    #[endpoint {
        method = PUT,
        path = "/{account}/networks/{network}",
        tags = ["role-tags"],
    }]
    async fn replace_network_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<NetworkPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    // ========================================================================
    // User Sub-Resource Role Tag Endpoints
    // ========================================================================

    /// Replace role tags on a user's keys collection
    #[endpoint {
        method = PUT,
        path = "/{account}/users/{uuid}/keys",
        tags = ["role-tags"],
    }]
    async fn replace_user_keys_collection_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Replace role tags on a specific user SSH key
    #[endpoint {
        method = PUT,
        path = "/{account}/users/{uuid}/keys/{name}",
        tags = ["users", "role-tags"],
    }]
    async fn replace_user_key_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserKeyPath>,
        body: TypedBody<ReplaceRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;
}
