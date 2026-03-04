// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use dropshot::{
    HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, Path, Query, RequestContext, TypedBody,
};

pub mod types;

use types::boot_params::*;
use types::common::*;
use types::image::*;
use types::nic::*;
use types::platform::*;
use types::server::*;
use types::task::*;
use types::vm::*;
use types::waitlist::*;

/// CNAPI — Triton Compute Node API
///
/// Manages compute nodes, dispatches tasks to cn-agent, and provides
/// endpoints for VMs, ZFS datasets, boot parameters, and more.
#[dropshot::api_description]
pub trait CnapiApi {
    type Context: Send + Sync + 'static;

    // ========================================================================
    // Ping
    // ========================================================================

    /// Return CNAPI service status
    #[endpoint { method = GET, path = "/ping", tags = ["system"] }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    // ========================================================================
    // Servers
    // ========================================================================

    /// List servers in the datacenter
    #[endpoint { method = GET, path = "/servers", tags = ["servers"] }]
    async fn server_list(
        rqctx: RequestContext<Self::Context>,
        query: Query<ServerListParams>,
    ) -> Result<HttpResponseOk<Vec<Server>>, HttpError>;

    /// Get a single server
    #[endpoint { method = GET, path = "/servers/{server_uuid}", tags = ["servers"] }]
    async fn server_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Server>, HttpError>;

    /// Update server properties
    #[endpoint { method = POST, path = "/servers/{server_uuid}", tags = ["servers"] }]
    async fn server_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<ServerUpdateParams>,
    ) -> Result<HttpResponseOk<Server>, HttpError>;

    /// Delete a server
    #[endpoint { method = DELETE, path = "/servers/{server_uuid}", tags = ["servers"] }]
    async fn server_delete(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Execute a command on a server via cn-agent
    #[endpoint { method = POST, path = "/servers/{server_uuid}/execute", tags = ["servers"] }]
    async fn command_execute(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<CommandExecuteParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Initiate server setup
    #[endpoint { method = PUT, path = "/servers/{server_uuid}/setup", tags = ["servers"] }]
    async fn server_setup(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Reboot a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/reboot", tags = ["servers"] }]
    async fn server_reboot(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Ensure an image is available on the server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/ensure-image", tags = ["servers"] }]
    async fn server_ensure_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<EnsureImageParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Install an agent on the server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/install-agent", tags = ["servers"] }]
    async fn server_install_agent(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<InstallAgentParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Uninstall agents from the server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/uninstall-agents", tags = ["servers"] }]
    async fn server_uninstall_agents(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<UninstallAgentsParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Refresh server sysinfo
    #[endpoint { method = POST, path = "/servers/{server_uuid}/sysinfo-refresh", tags = ["servers"] }]
    async fn server_sysinfo_refresh(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Register sysinfo from a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/sysinfo", tags = ["servers"] }]
    async fn server_sysinfo_register(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<SysinfoRegisterParams>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Factory reset a server
    #[endpoint { method = PUT, path = "/servers/{server_uuid}/factory-reset", tags = ["servers"] }]
    async fn server_factory_reset(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Set recovery configuration
    #[endpoint { method = POST, path = "/servers/{server_uuid}/recovery-config", tags = ["servers"] }]
    async fn server_recovery_config(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<RecoveryConfigParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Get task history for a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/task-history", tags = ["servers"] }]
    async fn server_task_history(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Vec<Task>>, HttpError>;

    /// Pause cn-agent on a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/cn-agent/pause", tags = ["servers"] }]
    async fn server_pause_cn_agent(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Resume cn-agent on a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/cn-agent/resume", tags = ["servers"] }]
    async fn server_resume_cn_agent(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Receive heartbeat event from a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/events/heartbeat", tags = ["servers"] }]
    async fn server_event_heartbeat(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<HeartbeatParams>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Receive status update event from a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/events/status", tags = ["servers"] }]
    async fn server_event_status_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<StatusUpdateParams>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// No-op endpoint (for testing connectivity)
    #[endpoint { method = POST, path = "/servers/{server_uuid}/nop", tags = ["servers"] }]
    async fn server_nop(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    // ========================================================================
    // VMs
    // ========================================================================

    /// Create a VM on a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms", tags = ["vms"] }]
    async fn vm_create(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<VmCreateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// List VMs on a server (deprecated: use VMAPI instead)
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms", tags = ["vms"] }]
    async fn vm_list(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Vec<serde_json::Value>>, HttpError>;

    /// Load VM details
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}", tags = ["vms"] }]
    async fn vm_load(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Reprovision a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/reprovision", tags = ["vms"] }]
    async fn vm_reprovision(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmReprovisionParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Update VM NICs
    ///
    /// Note: Original Node.js path was /servers/:server_uuid/vms/nics/update
    /// which conflicts with Dropshot's tree router (/vms/{uuid} vs /vms/nics).
    /// Remapped to /servers/:server_uuid/vm-nics/update.
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vm-nics/update", tags = ["vms"] }]
    async fn vm_nics_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<VmNicsUpdateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Get VM process listing
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}/proc", tags = ["vms"] }]
    async fn vm_proc(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Get VM info (vnc port, etc.)
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}/info", tags = ["vms"] }]
    async fn vm_info(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Get VM VNC connection info
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}/vnc", tags = ["vms"] }]
    async fn vm_vnc(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Get VM console info
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}/console", tags = ["vms"] }]
    async fn vm_console(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Update a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/update", tags = ["vms"] }]
    async fn vm_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmUpdateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Start a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/start", tags = ["vms"] }]
    async fn vm_start(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Stop a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/stop", tags = ["vms"] }]
    async fn vm_stop(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Reboot a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/reboot", tags = ["vms"] }]
    async fn vm_reboot(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Kill a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/kill", tags = ["vms"] }]
    async fn vm_kill(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Destroy a VM
    #[endpoint { method = DELETE, path = "/servers/{server_uuid}/vms/{uuid}", tags = ["vms"] }]
    async fn vm_destroy(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Create a VM snapshot
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/snapshots", tags = ["vms"] }]
    async fn vm_snapshot_create(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmSnapshotParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Rollback a VM snapshot
    #[endpoint { method = PUT, path = "/servers/{server_uuid}/vms/{uuid}/snapshots", tags = ["vms"] }]
    async fn vm_snapshot_rollback(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmRollbackParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Create an image from a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/images", tags = ["vms"] }]
    async fn vm_create_image(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmImageCreateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Migrate a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/migrate", tags = ["vms"] }]
    async fn vm_migrate(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<VmMigrateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Execute a Docker command in a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/docker-exec", tags = ["vms"] }]
    async fn vm_docker_exec(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<DockerExecParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Docker copy files to/from a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/docker-copy", tags = ["vms"] }]
    async fn vm_docker_copy(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<DockerCopyParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Docker build in a VM
    #[endpoint { method = POST, path = "/servers/{server_uuid}/vms/{uuid}/docker-build", tags = ["vms"] }]
    async fn vm_docker_build(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
        body: TypedBody<DockerBuildParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Get Docker stats for a VM
    #[endpoint { method = GET, path = "/servers/{server_uuid}/vms/{uuid}/docker-stats", tags = ["vms"] }]
    async fn vm_docker_stats(
        rqctx: RequestContext<Self::Context>,
        path: Path<VmPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    // ========================================================================
    // Tasks
    // ========================================================================

    /// Get task details
    #[endpoint { method = GET, path = "/tasks/{taskid}", tags = ["tasks"] }]
    async fn task_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<TaskPath>,
    ) -> Result<HttpResponseOk<Task>, HttpError>;

    /// Wait for a task to complete
    #[endpoint { method = GET, path = "/tasks/{taskid}/wait", tags = ["tasks"] }]
    async fn task_wait(
        rqctx: RequestContext<Self::Context>,
        path: Path<TaskPath>,
        query: Query<TaskWaitParams>,
    ) -> Result<HttpResponseOk<Task>, HttpError>;

    // ========================================================================
    // ZFS Datasets
    // ========================================================================

    /// List datasets on a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/datasets", tags = ["zfs"] }]
    async fn dataset_list(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Vec<serde_json::Value>>, HttpError>;

    /// Create a dataset on a server
    #[endpoint { method = POST, path = "/servers/{server_uuid}/datasets", tags = ["zfs"] }]
    async fn dataset_create(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<types::zfs::DatasetCreateParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Create a snapshot of a dataset
    #[endpoint { method = POST, path = "/servers/{server_uuid}/datasets/{dataset}/snapshot", tags = ["zfs"] }]
    async fn snapshot_create(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Rollback a dataset to a snapshot
    #[endpoint { method = POST, path = "/servers/{server_uuid}/datasets/{dataset}/rollback", tags = ["zfs"] }]
    async fn snapshot_rollback(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// List snapshots of a dataset
    #[endpoint { method = GET, path = "/servers/{server_uuid}/datasets/{dataset}/snapshots", tags = ["zfs"] }]
    async fn snapshot_list(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
    ) -> Result<HttpResponseOk<Vec<serde_json::Value>>, HttpError>;

    /// Get all dataset properties on a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/dataset-properties", tags = ["zfs"] }]
    async fn dataset_properties_get_all(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Get properties of a specific dataset
    #[endpoint { method = GET, path = "/servers/{server_uuid}/datasets/{dataset}/properties", tags = ["zfs"] }]
    async fn dataset_properties_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Set properties on a dataset
    #[endpoint { method = POST, path = "/servers/{server_uuid}/datasets/{dataset}/properties", tags = ["zfs"] }]
    async fn dataset_properties_set(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
        body: TypedBody<types::zfs::DatasetPropertiesSetParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    /// Destroy a dataset
    #[endpoint { method = DELETE, path = "/servers/{server_uuid}/datasets/{dataset}", tags = ["zfs"] }]
    async fn dataset_destroy(
        rqctx: RequestContext<Self::Context>,
        path: Path<types::zfs::DatasetPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// List zpools on a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/zpools", tags = ["zfs"] }]
    async fn zpool_list(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Vec<serde_json::Value>>, HttpError>;

    // ========================================================================
    // Boot Parameters
    // ========================================================================

    /// Get boot parameters for a server or "default"
    ///
    /// Note: Original Node.js had separate /boot/default and /boot/:server_uuid
    /// endpoints. Dropshot's tree router doesn't allow a literal and variable at
    /// the same path level, so we use a single /boot/{server_uuid} endpoint
    /// where server_uuid can be "default" or an actual UUID.
    #[endpoint { method = GET, path = "/boot/{server_uuid}", tags = ["boot_params"] }]
    async fn boot_params_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<BootParamsPath>,
    ) -> Result<HttpResponseOk<BootParams>, HttpError>;

    /// Set boot parameters for a server or "default"
    #[endpoint { method = POST, path = "/boot/{server_uuid}", tags = ["boot_params"] }]
    async fn boot_params_set(
        rqctx: RequestContext<Self::Context>,
        path: Path<BootParamsPath>,
        body: TypedBody<BootParamsBody>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Update boot parameters for a server or "default"
    #[endpoint { method = PUT, path = "/boot/{server_uuid}", tags = ["boot_params"] }]
    async fn boot_params_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<BootParamsPath>,
        body: TypedBody<BootParamsBody>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Platforms
    // ========================================================================

    /// List platforms
    #[endpoint { method = GET, path = "/platforms", tags = ["platforms"] }]
    async fn platform_list(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PlatformListResponse>, HttpError>;

    // ========================================================================
    // NICs
    // ========================================================================

    /// Update NIC on a server
    #[endpoint { method = PUT, path = "/servers/{server_uuid}/nics", tags = ["nics"] }]
    async fn nic_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<NicUpdateParams>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Dispatch NIC update task to cn-agent
    #[endpoint { method = POST, path = "/servers/{server_uuid}/nics/update", tags = ["nics"] }]
    async fn nic_update_task(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<NicUpdateTaskParams>,
    ) -> Result<HttpResponseOk<TaskResponse>, HttpError>;

    // ========================================================================
    // Images
    // ========================================================================

    /// Get image info on a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/images/{uuid}", tags = ["images"] }]
    async fn image_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<ImagePath>,
    ) -> Result<HttpResponseOk<ImageInfo>, HttpError>;

    // ========================================================================
    // Waitlist
    // ========================================================================

    /// List waitlist tickets for a server
    #[endpoint { method = GET, path = "/servers/{server_uuid}/tickets", tags = ["waitlist"] }]
    async fn server_waitlist_list(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseOk<Vec<Ticket>>, HttpError>;

    /// Get a waitlist ticket
    #[endpoint { method = GET, path = "/tickets/{ticket_uuid}", tags = ["waitlist"] }]
    async fn waitlist_ticket_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<TicketPath>,
    ) -> Result<HttpResponseOk<Ticket>, HttpError>;

    /// Create a waitlist ticket
    #[endpoint { method = POST, path = "/servers/{server_uuid}/tickets", tags = ["waitlist"] }]
    async fn waitlist_ticket_create(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
        body: TypedBody<WaitlistCreateParams>,
    ) -> Result<HttpResponseCreated<Ticket>, HttpError>;

    /// Delete all waitlist tickets for a server
    #[endpoint { method = DELETE, path = "/servers/{server_uuid}/tickets", tags = ["waitlist"] }]
    async fn waitlist_tickets_delete_all(
        rqctx: RequestContext<Self::Context>,
        path: Path<ServerPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Wait for a waitlist ticket
    #[endpoint { method = GET, path = "/tickets/{ticket_uuid}/wait", tags = ["waitlist"] }]
    async fn waitlist_ticket_wait(
        rqctx: RequestContext<Self::Context>,
        path: Path<TicketPath>,
        query: Query<WaitlistWaitParams>,
    ) -> Result<HttpResponseOk<Ticket>, HttpError>;

    /// Update a waitlist ticket
    #[endpoint { method = PUT, path = "/tickets/{ticket_uuid}", tags = ["waitlist"] }]
    async fn waitlist_ticket_update(
        rqctx: RequestContext<Self::Context>,
        path: Path<TicketPath>,
        body: TypedBody<WaitlistUpdateParams>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    /// Release a waitlist ticket
    #[endpoint { method = PUT, path = "/tickets/{ticket_uuid}/release", tags = ["waitlist"] }]
    async fn waitlist_ticket_release(
        rqctx: RequestContext<Self::Context>,
        path: Path<TicketPath>,
    ) -> Result<HttpResponseUpdatedNoContent, HttpError>;

    // ========================================================================
    // Allocations
    // ========================================================================

    /// Allocate a server for a new VM
    #[endpoint { method = POST, path = "/allocate", tags = ["allocations"] }]
    async fn allocate(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<serde_json::Value>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;
}
