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

//! VMAPI (VM API) trait definition
//!
//! This crate defines the API trait for Triton's VMAPI service (version 9.17.0).
//! VMAPI is an internal HTTP API for managing virtual machines in a Triton datacenter.
//!
//! # API Overview
//!
//! VMAPI provides the following functionality:
//! - VM lifecycle management (create, read, update, delete)
//! - VM actions (start, stop, reboot, migrate, etc.)
//! - Metadata and tag management
//! - Role tag management for RBAC
//! - Job management (workflow tracking)
//! - VM migration management
//!
//! # JSON Field Naming
//!
//! VMAPI uses snake_case for all JSON field names, which is standard for
//! Triton internal APIs (unlike CloudAPI which uses camelCase).

use dropshot::{
    HttpError, HttpResponseAccepted, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, Query, RequestContext, TypedBody, WebsocketChannelResult, WebsocketConnection,
};

pub mod types;
pub use types::*;

/// VMAPI trait definition
///
/// This trait defines all endpoints of the Triton VMAPI service (version 9.17.0).
/// The API is organized into the following categories:
/// - Ping (health check)
/// - VMs (virtual machine CRUD and actions)
/// - Jobs (workflow job tracking)
/// - Metadata (customer_metadata, internal_metadata, tags)
/// - Role Tags (RBAC)
/// - Statuses (bulk VM status queries)
/// - Migrations (VM migration management)
#[dropshot::api_description]
pub trait VmApi {
    /// Context type for request handlers
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Ping Endpoint
    // ========================================================================

    /// Health check endpoint
    #[endpoint {
        method = GET,
        path = "/ping",
        tags = ["ping"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // VM Endpoints
    // ========================================================================

    /// List VMs
    ///
    /// Returns an array of VM objects matching the query filters.
    #[endpoint {
        method = GET,
        path = "/vms",
        tags = ["vms"],
    }]
    async fn list_vms(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListVmsQuery>,
    ) -> Result<HttpResponseOk<Vec<Vm>>, HttpError>;

    /// Count VMs (HEAD)
    ///
    /// Returns count of VMs in response headers without body.
    #[endpoint {
        method = HEAD,
        path = "/vms",
        tags = ["vms"],
    }]
    async fn head_vms(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListVmsQuery>,
    ) -> Result<HttpResponseOk<Vec<Vm>>, HttpError>;

    /// Create VM
    ///
    /// Provisions a new virtual machine.
    #[endpoint {
        method = POST,
        path = "/vms",
        tags = ["vms"],
    }]
    async fn create_vm(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateVmRequest>,
    ) -> Result<HttpResponseCreated<JobResponse>, HttpError>;

    /// Bulk update VMs for a server
    ///
    /// Used by cn-agent to sync VM state for an entire compute node.
    #[endpoint {
        method = PUT,
        path = "/vms",
        tags = ["vms"],
    }]
    async fn put_vms(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<PutVmsRequest>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Get VM
    ///
    /// Returns a single VM object.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}",
        tags = ["vms"],
    }]
    async fn get_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<Vm>, HttpError>;

    /// Check VM existence (HEAD)
    ///
    /// Returns 200 if VM exists, 404 otherwise.
    #[endpoint {
        method = HEAD,
        path = "/vms/{uuid}",
        tags = ["vms"],
    }]
    async fn head_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<Vm>, HttpError>;

    /// Perform VM action
    ///
    /// Dispatches an action on the VM. The action may be specified in the
    /// request body (`{"action": "start", ...}`) or as a query parameter
    /// (`?action=start`). Body takes precedence over the query parameter.
    ///
    /// Available actions: start, stop, kill, reboot, reprovision, update,
    /// add_nics, update_nics, remove_nics, create_snapshot, rollback_snapshot,
    /// delete_snapshot, create_disk, resize_disk, delete_disk, migrate.
    ///
    /// The request body varies by action type.
    #[endpoint {
        method = POST,
        path = "/vms/{uuid}",
        tags = ["vms"],
    }]
    async fn vm_action(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        query: Query<VmActionQuery>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Replace VM object
    ///
    /// Replaces the entire VM object (used by cn-agent for sync).
    #[endpoint {
        method = PUT,
        path = "/vms/{uuid}",
        tags = ["vms"],
    }]
    async fn put_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<PutVmRequest>,
    ) -> Result<HttpResponseOk<Vm>, HttpError>;

    /// Destroy VM
    ///
    /// Destroys the virtual machine.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}",
        tags = ["vms"],
    }]
    async fn delete_vm(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Get VM process info
    ///
    /// Returns process information for the VM from CNAPI.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/proc",
        tags = ["vms"],
    }]
    async fn get_vm_proc(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    // ========================================================================
    // Job Endpoints
    // ========================================================================

    /// List jobs
    ///
    /// Returns an array of workflow jobs.
    #[endpoint {
        method = GET,
        path = "/jobs",
        tags = ["jobs"],
    }]
    async fn list_jobs(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListJobsQuery>,
    ) -> Result<HttpResponseOk<Vec<Job>>, HttpError>;

    /// Get job
    ///
    /// Returns a single job object.
    #[endpoint {
        method = GET,
        path = "/jobs/{job_uuid}",
        tags = ["jobs"],
    }]
    async fn get_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<Job>, HttpError>;

    /// Wait for job completion
    ///
    /// Blocks until the job completes and returns the final job state.
    #[endpoint {
        method = GET,
        path = "/jobs/{job_uuid}/wait",
        tags = ["jobs"],
    }]
    async fn wait_job(
        rqctx: RequestContext<Self::Context>,
        path: Path<JobPath>,
    ) -> Result<HttpResponseOk<Job>, HttpError>;

    /// Post job results (workflow callback)
    ///
    /// Internal endpoint for workflow to report job completion.
    #[endpoint {
        method = POST,
        path = "/job_results",
        tags = ["jobs"],
    }]
    async fn post_job_results(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<PostJobResultsRequest>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// List jobs for a VM
    ///
    /// Returns jobs associated with a specific VM.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/jobs",
        tags = ["vms", "jobs"],
    }]
    async fn list_vm_jobs(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        query: Query<ListJobsQuery>,
    ) -> Result<HttpResponseOk<Vec<Job>>, HttpError>;

    // ========================================================================
    // Customer Metadata Endpoints
    // ========================================================================

    /// List customer metadata
    ///
    /// Returns all customer metadata for a VM.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/customer_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn list_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<MetadataObject>, HttpError>;

    /// Get customer metadata key
    ///
    /// Returns the value of a specific customer metadata key.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/customer_metadata/{key}",
        tags = ["vms", "metadata"],
    }]
    async fn get_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Add customer metadata (merge)
    ///
    /// Adds/merges customer metadata with existing values.
    #[endpoint {
        method = POST,
        path = "/vms/{uuid}/customer_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn add_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<AddMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Set customer metadata (replace all)
    ///
    /// Replaces all customer metadata with the provided values.
    #[endpoint {
        method = PUT,
        path = "/vms/{uuid}/customer_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn set_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<SetMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete customer metadata key
    ///
    /// Removes a specific customer metadata key.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/customer_metadata/{key}",
        tags = ["vms", "metadata"],
    }]
    async fn delete_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete all customer metadata
    ///
    /// Removes all customer metadata.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/customer_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn delete_all_customer_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    // ========================================================================
    // Internal Metadata Endpoints
    // ========================================================================

    /// List internal metadata
    ///
    /// Returns all internal metadata for a VM.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/internal_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn list_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<MetadataObject>, HttpError>;

    /// Get internal metadata key
    ///
    /// Returns the value of a specific internal metadata key.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/internal_metadata/{key}",
        tags = ["vms", "metadata"],
    }]
    async fn get_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Add internal metadata (merge)
    ///
    /// Adds/merges internal metadata with existing values.
    #[endpoint {
        method = POST,
        path = "/vms/{uuid}/internal_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn add_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<AddMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Set internal metadata (replace all)
    ///
    /// Replaces all internal metadata with the provided values.
    #[endpoint {
        method = PUT,
        path = "/vms/{uuid}/internal_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn set_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<SetMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete internal metadata key
    ///
    /// Removes a specific internal metadata key.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/internal_metadata/{key}",
        tags = ["vms", "metadata"],
    }]
    async fn delete_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete all internal metadata
    ///
    /// Removes all internal metadata.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/internal_metadata",
        tags = ["vms", "metadata"],
    }]
    async fn delete_all_internal_metadata(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    // ========================================================================
    // Tags Endpoints
    // ========================================================================

    /// List tags
    ///
    /// Returns all tags for a VM.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/tags",
        tags = ["vms", "tags"],
    }]
    async fn list_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<MetadataObject>, HttpError>;

    /// Get tag
    ///
    /// Returns the value of a specific tag.
    #[endpoint {
        method = GET,
        path = "/vms/{uuid}/tags/{key}",
        tags = ["vms", "tags"],
    }]
    async fn get_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Add tags (merge)
    ///
    /// Adds/merges tags with existing values.
    #[endpoint {
        method = POST,
        path = "/vms/{uuid}/tags",
        tags = ["vms", "tags"],
    }]
    async fn add_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<AddMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Set tags (replace all)
    ///
    /// Replaces all tags with the provided values.
    #[endpoint {
        method = PUT,
        path = "/vms/{uuid}/tags",
        tags = ["vms", "tags"],
    }]
    async fn set_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<SetMetadataRequest>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete tag
    ///
    /// Removes a specific tag.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/tags/{key}",
        tags = ["vms", "tags"],
    }]
    async fn delete_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmMetadataKeyPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    /// Delete all tags
    ///
    /// Removes all tags.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/tags",
        tags = ["vms", "tags"],
    }]
    async fn delete_all_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;

    // ========================================================================
    // Role Tags Endpoints
    // ========================================================================

    /// Add role tags (merge)
    ///
    /// Adds role tags to the VM.
    #[endpoint {
        method = POST,
        path = "/vms/{uuid}/role_tags",
        tags = ["vms", "role_tags"],
    }]
    async fn add_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleTagsPath>,
        body: TypedBody<AddRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Set role tags (replace all)
    ///
    /// Replaces all role tags on the VM.
    #[endpoint {
        method = PUT,
        path = "/vms/{uuid}/role_tags",
        tags = ["vms", "role_tags"],
    }]
    async fn set_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleTagsPath>,
        body: TypedBody<SetRoleTagsRequest>,
    ) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

    /// Delete single role tag
    ///
    /// Removes a specific role tag from the VM.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/role_tags/{role_tag}",
        tags = ["vms", "role_tags"],
    }]
    async fn delete_role_tag(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleTagPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Delete all role tags
    ///
    /// Removes all role tags from the VM.
    #[endpoint {
        method = DELETE,
        path = "/vms/{uuid}/role_tags",
        tags = ["vms", "role_tags"],
    }]
    async fn delete_all_role_tags(
        rqctx: RequestContext<Self::Context>,
        path: Path<RoleTagsPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    // ========================================================================
    // Statuses Endpoint
    // ========================================================================

    /// Get statuses for multiple VMs
    ///
    /// Returns status information for multiple VMs by UUID.
    #[endpoint {
        method = GET,
        path = "/statuses",
        tags = ["statuses"],
    }]
    async fn list_statuses(
        rqctx: RequestContext<Self::Context>,
        query: Query<GetStatusesQuery>,
    ) -> Result<HttpResponseOk<StatusesResponse>, HttpError>;

    // ========================================================================
    // Migration Endpoints
    // ========================================================================

    /// List migrations
    ///
    /// Returns all migration records.
    #[endpoint {
        method = GET,
        path = "/migrations",
        tags = ["migrations"],
    }]
    async fn list_migrations(
        rqctx: RequestContext<Self::Context>,
        query: Query<ListMigrationsQuery>,
    ) -> Result<HttpResponseOk<Vec<Migration>>, HttpError>;

    /// Get migration for VM
    ///
    /// Returns the migration record for a specific VM.
    #[endpoint {
        method = GET,
        path = "/migrations/{uuid}",
        tags = ["migrations"],
    }]
    async fn get_migration(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
    ) -> Result<HttpResponseOk<Migration>, HttpError>;

    /// Delete migration record
    ///
    /// Removes the migration record for a VM.
    #[endpoint {
        method = DELETE,
        path = "/migrations/{uuid}",
        tags = ["migrations"],
    }]
    async fn delete_migration(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Watch migration progress
    ///
    /// Streams migration progress updates via WebSocket (application/x-json-stream).
    /// Returns newline-delimited JSON objects for progress events.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/migrations/{uuid}/watch",
        tags = ["migrations"],
    }]
    async fn watch_migration(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    /// Store migration record (internal)
    ///
    /// Internal endpoint for cn-agent to store migration records.
    #[endpoint {
        method = POST,
        path = "/migrations/{uuid}/store",
        tags = ["migrations"],
    }]
    async fn store_migration_record(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
        body: TypedBody<StoreMigrationRecordRequest>,
    ) -> Result<HttpResponseOk<Migration>, HttpError>;

    /// Report migration progress (internal)
    ///
    /// Internal endpoint for cn-agent to report migration progress.
    #[endpoint {
        method = POST,
        path = "/migrations/{uuid}/progress",
        tags = ["migrations"],
    }]
    async fn report_migration_progress(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
        body: TypedBody<MigrationProgressRequest>,
    ) -> Result<HttpResponseOk<Migration>, HttpError>;

    /// Update VM server UUID (internal)
    ///
    /// Internal endpoint to update the server UUID after migration.
    #[endpoint {
        method = POST,
        path = "/migrations/{uuid}/updateVmServerUuid",
        tags = ["migrations"],
    }]
    async fn update_vm_server_uuid(
        rqctx: RequestContext<Self::Context>,
        path: Path<MigrationPath>,
        body: TypedBody<UpdateVmServerUuidRequest>,
    ) -> Result<HttpResponseOk<Vm>, HttpError>;
}
