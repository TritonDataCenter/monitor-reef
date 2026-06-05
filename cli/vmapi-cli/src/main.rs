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

//! VMAPI CLI - Command-line interface for Triton VMAPI
//!
//! This CLI provides access to all VMAPI endpoints for managing virtual machines
//! in a Triton datacenter. VMAPI is an internal API used by other Triton services.
//!
//! # Environment Variables
//!
//! - `VMAPI_URL` - VMAPI base URL (default: http://localhost)

use anyhow::Result;
use clap::{Parser, Subcommand};
use uuid::Uuid;
// Use the generated types from vmapi_client::types
use vmapi_client::types::MigrationAction;
use vmapi_client::{Client, TypedClient, types};
// Re-exported API types for TypedClient methods
use vmapi_client::{
    AddNicsRequest, CreateDiskRequest, MigrateVmRequest, ResizeDiskRequest, UpdateNicsRequest,
    UpdateVmRequest,
};

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

#[derive(Parser)]
#[command(name = "vmapi", version, about = "CLI for Triton VMAPI")]
struct Cli {
    /// VMAPI base URL
    #[arg(long, env = "VMAPI_URL", default_value = "http://localhost")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ========================================================================
    // Ping
    // ========================================================================
    /// Health check endpoint
    Ping,

    // ========================================================================
    // VMs
    // ========================================================================
    /// List VMs
    #[command(name = "list-vms")]
    ListVms {
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Filter by server UUID
        #[arg(long)]
        server_uuid: Option<Uuid>,
        /// Filter by state
        #[arg(long, value_enum)]
        state: Option<types::VmState>,
        /// Filter by brand
        #[arg(long, value_enum)]
        brand: Option<types::VmBrand>,
        /// Filter by alias
        #[arg(long)]
        alias: Option<String>,
        /// Pagination limit
        #[arg(long)]
        limit: Option<i64>,
        /// Pagination offset
        #[arg(long)]
        offset: Option<i64>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Count VMs (HEAD request)
    #[command(name = "head-vms")]
    HeadVms {
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Filter by state
        #[arg(long, value_enum)]
        state: Option<types::VmState>,
    },

    /// Get VM details
    #[command(name = "get-vm")]
    GetVm {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Check VM existence (HEAD request)
    #[command(name = "head-vm")]
    HeadVm {
        /// VM UUID
        uuid: Uuid,
    },

    /// Create a new VM
    #[command(name = "create-vm")]
    CreateVm {
        /// Owner UUID (required)
        #[arg(long)]
        owner_uuid: Uuid,
        /// VM alias/name
        #[arg(long)]
        alias: Option<String>,
        /// Image UUID
        #[arg(long)]
        image_uuid: Option<Uuid>,
        /// Billing ID (package UUID)
        #[arg(long)]
        billing_id: Option<Uuid>,
        /// RAM in MB
        #[arg(long)]
        ram: Option<u64>,
        /// Server UUID for placement
        #[arg(long)]
        server_uuid: Option<Uuid>,
        /// Brand (bhyve, kvm, joyent, etc.)
        #[arg(long)]
        brand: Option<String>,
    },

    /// Bulk update VMs for a server
    #[command(name = "put-vms")]
    PutVms {
        /// Server UUID
        #[arg(long)]
        server_uuid: Uuid,
        /// VMs JSON array
        #[arg(long)]
        vms_json: String,
    },

    /// Replace VM object
    #[command(name = "put-vm")]
    PutVm {
        /// VM UUID
        uuid: Uuid,
        /// VM JSON object
        #[arg(long)]
        vm_json: String,
    },

    /// Delete/destroy a VM
    #[command(name = "delete-vm")]
    DeleteVm {
        /// VM UUID
        uuid: Uuid,
    },

    /// Get VM process info
    #[command(name = "get-vm-proc")]
    GetVmProc {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // VM Actions
    // ========================================================================
    /// Start a VM
    #[command(name = "start-vm")]
    StartVm {
        /// VM UUID
        uuid: Uuid,
        /// Don't error if already running
        #[arg(long)]
        idempotent: bool,
    },

    /// Stop a VM
    #[command(name = "stop-vm")]
    StopVm {
        /// VM UUID
        uuid: Uuid,
        /// Don't error if already stopped
        #[arg(long)]
        idempotent: bool,
    },

    /// Kill a VM (send signal)
    #[command(name = "kill-vm")]
    KillVm {
        /// VM UUID
        uuid: Uuid,
        /// Signal to send (e.g., SIGTERM, SIGKILL)
        #[arg(long)]
        signal: Option<String>,
        /// Don't error if already stopped
        #[arg(long)]
        idempotent: bool,
    },

    /// Reboot a VM
    #[command(name = "reboot-vm")]
    RebootVm {
        /// VM UUID
        uuid: Uuid,
        /// Don't error if not running
        #[arg(long)]
        idempotent: bool,
    },

    /// Reprovision a VM with a new image
    #[command(name = "reprovision-vm")]
    ReprovisionVm {
        /// VM UUID
        uuid: Uuid,
        /// Image UUID to reprovision with
        #[arg(long)]
        image_uuid: Uuid,
    },

    /// Update VM properties
    #[command(name = "update-vm")]
    UpdateVm {
        /// VM UUID
        uuid: Uuid,
        /// New alias
        #[arg(long)]
        alias: Option<String>,
        /// RAM in MB
        #[arg(long)]
        ram: Option<u64>,
        /// CPU cap (percentage)
        #[arg(long)]
        cpu_cap: Option<u32>,
        /// Disk quota in GB
        #[arg(long)]
        quota: Option<u64>,
        /// Firewall enabled
        #[arg(long)]
        firewall_enabled: Option<bool>,
    },

    /// Add NICs to a VM
    #[command(name = "add-nics")]
    AddNics {
        /// VM UUID
        uuid: Uuid,
        /// Network UUIDs (comma-separated)
        #[arg(long)]
        networks: Option<String>,
        /// MAC addresses of pre-created NICs (comma-separated)
        #[arg(long)]
        macs: Option<String>,
    },

    /// Update NICs on a VM
    #[command(name = "update-nics")]
    UpdateNics {
        /// VM UUID
        uuid: Uuid,
        /// NICs JSON array
        #[arg(long)]
        nics_json: String,
    },

    /// Remove NICs from a VM
    #[command(name = "remove-nics")]
    RemoveNics {
        /// VM UUID
        uuid: Uuid,
        /// MAC addresses to remove (comma-separated)
        #[arg(long)]
        macs: String,
    },

    /// Create a snapshot
    #[command(name = "create-snapshot")]
    CreateSnapshot {
        /// VM UUID
        uuid: Uuid,
        /// Snapshot name (optional, auto-generated if not provided)
        #[arg(long)]
        name: Option<String>,
    },

    /// Rollback to a snapshot
    #[command(name = "rollback-snapshot")]
    RollbackSnapshot {
        /// VM UUID
        uuid: Uuid,
        /// Snapshot name
        #[arg(long)]
        name: String,
    },

    /// Delete a snapshot
    #[command(name = "delete-snapshot")]
    DeleteSnapshot {
        /// VM UUID
        uuid: Uuid,
        /// Snapshot name
        #[arg(long)]
        name: String,
    },

    /// Create a disk (bhyve only)
    #[command(name = "create-disk")]
    CreateDisk {
        /// VM UUID
        uuid: Uuid,
        /// Disk size in MB (or "remaining" for remaining space)
        #[arg(long)]
        size: String,
        /// PCI slot (optional)
        #[arg(long)]
        pci_slot: Option<String>,
    },

    /// Resize a disk (bhyve only)
    #[command(name = "resize-disk")]
    ResizeDisk {
        /// VM UUID
        uuid: Uuid,
        /// PCI slot of disk
        #[arg(long)]
        pci_slot: String,
        /// New size in MB
        #[arg(long)]
        size: u64,
        /// Allow shrinking (dangerous)
        #[arg(long)]
        dangerous_allow_shrink: bool,
    },

    /// Delete a disk (bhyve only)
    #[command(name = "delete-disk")]
    DeleteDisk {
        /// VM UUID
        uuid: Uuid,
        /// PCI slot of disk
        #[arg(long)]
        pci_slot: String,
    },

    /// Migrate a VM
    #[command(name = "migrate-vm")]
    MigrateVm {
        /// VM UUID
        uuid: Uuid,
        /// Migration action
        #[arg(long, value_enum)]
        action: Option<MigrationAction>,
        /// Target server UUID
        #[arg(long)]
        target_server_uuid: Option<Uuid>,
    },

    // ========================================================================
    // Jobs
    // ========================================================================
    /// List jobs
    #[command(name = "list-jobs")]
    ListJobs {
        /// Filter by VM UUID
        #[arg(long)]
        vm_uuid: Option<Uuid>,
        /// Filter by task name
        #[arg(long)]
        task: Option<String>,
        /// Pagination limit
        #[arg(long)]
        limit: Option<i64>,
        /// Pagination offset
        #[arg(long)]
        offset: Option<i64>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get job details
    #[command(name = "get-job")]
    GetJob {
        /// Job UUID
        job_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Wait for job completion
    #[command(name = "wait-job")]
    WaitJob {
        /// Job UUID
        job_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Post job results (internal workflow callback)
    #[command(name = "post-job-results")]
    PostJobResults {
        /// Job results JSON
        #[arg(long)]
        results_json: String,
    },

    /// List jobs for a specific VM
    #[command(name = "list-vm-jobs")]
    ListVmJobs {
        /// VM UUID
        uuid: Uuid,
        /// Pagination limit
        #[arg(long)]
        limit: Option<i64>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Customer Metadata
    // ========================================================================
    /// List customer metadata
    #[command(name = "list-customer-metadata")]
    ListCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get customer metadata key
    #[command(name = "get-customer-metadata")]
    GetCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata key
        key: String,
    },

    /// Add customer metadata (merge)
    #[command(name = "add-customer-metadata")]
    AddCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata JSON object
        #[arg(long)]
        metadata_json: String,
    },

    /// Set customer metadata (replace all)
    #[command(name = "set-customer-metadata")]
    SetCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata JSON object
        #[arg(long)]
        metadata_json: String,
    },

    /// Delete customer metadata key
    #[command(name = "delete-customer-metadata")]
    DeleteCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata key
        key: String,
    },

    /// Delete all customer metadata
    #[command(name = "delete-all-customer-metadata")]
    DeleteAllCustomerMetadata {
        /// VM UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Internal Metadata
    // ========================================================================
    /// List internal metadata
    #[command(name = "list-internal-metadata")]
    ListInternalMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get internal metadata key
    #[command(name = "get-internal-metadata")]
    GetInternalMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata key
        key: String,
    },

    /// Add internal metadata (merge)
    #[command(name = "add-internal-metadata")]
    AddInternalMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata JSON object
        #[arg(long)]
        metadata_json: String,
    },

    /// Set internal metadata (replace all)
    #[command(name = "set-internal-metadata")]
    SetInternalMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata JSON object
        #[arg(long)]
        metadata_json: String,
    },

    /// Delete internal metadata key
    #[command(name = "delete-internal-metadata")]
    DeleteInternalMetadata {
        /// VM UUID
        uuid: Uuid,
        /// Metadata key
        key: String,
    },

    /// Delete all internal metadata
    #[command(name = "delete-all-internal-metadata")]
    DeleteAllInternalMetadata {
        /// VM UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Tags
    // ========================================================================
    /// List tags
    #[command(name = "list-tags")]
    ListTags {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get tag value
    #[command(name = "get-tag")]
    GetTag {
        /// VM UUID
        uuid: Uuid,
        /// Tag key
        key: String,
    },

    /// Add tags (merge)
    #[command(name = "add-tags")]
    AddTags {
        /// VM UUID
        uuid: Uuid,
        /// Tags JSON object
        #[arg(long)]
        tags_json: String,
    },

    /// Set tags (replace all)
    #[command(name = "set-tags")]
    SetTags {
        /// VM UUID
        uuid: Uuid,
        /// Tags JSON object
        #[arg(long)]
        tags_json: String,
    },

    /// Delete tag
    #[command(name = "delete-tag")]
    DeleteTag {
        /// VM UUID
        uuid: Uuid,
        /// Tag key
        key: String,
    },

    /// Delete all tags
    #[command(name = "delete-all-tags")]
    DeleteAllTags {
        /// VM UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Role Tags
    // ========================================================================
    /// Add role tags
    #[command(name = "add-role-tags")]
    AddRoleTags {
        /// VM UUID
        uuid: Uuid,
        /// Role tags (comma-separated UUIDs)
        #[arg(long)]
        role_tags: String,
    },

    /// Set role tags (replace all)
    #[command(name = "set-role-tags")]
    SetRoleTags {
        /// VM UUID
        uuid: Uuid,
        /// Role tags (comma-separated UUIDs)
        #[arg(long)]
        role_tags: String,
    },

    /// Delete a role tag
    #[command(name = "delete-role-tag")]
    DeleteRoleTag {
        /// VM UUID
        uuid: Uuid,
        /// Role tag UUID
        role_tag: Uuid,
    },

    /// Delete all role tags
    #[command(name = "delete-all-role-tags")]
    DeleteAllRoleTags {
        /// VM UUID
        uuid: Uuid,
    },

    // ========================================================================
    // Statuses
    // ========================================================================
    /// Get statuses for multiple VMs
    #[command(name = "list-statuses")]
    ListStatuses {
        /// VM UUIDs (comma-separated)
        #[arg(long)]
        uuids: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Migrations
    // ========================================================================
    /// List migrations
    #[command(name = "list-migrations")]
    ListMigrations {
        /// Filter by state
        #[arg(long, value_enum)]
        state: Option<types::MigrationState>,
        /// Filter by source server UUID
        #[arg(long)]
        source_server_uuid: Option<Uuid>,
        /// Pagination limit
        #[arg(long)]
        limit: Option<i64>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get migration for a VM
    #[command(name = "get-migration")]
    GetMigration {
        /// VM UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete migration record
    #[command(name = "delete-migration")]
    DeleteMigration {
        /// VM UUID
        uuid: Uuid,
    },

    /// Store migration record (internal)
    #[command(name = "store-migration-record")]
    StoreMigrationRecord {
        /// VM UUID
        uuid: Uuid,
        /// Migration record JSON
        #[arg(long)]
        record_json: String,
    },

    /// Report migration progress (internal)
    #[command(name = "report-migration-progress")]
    ReportMigrationProgress {
        /// VM UUID
        uuid: Uuid,
        /// Progress JSON
        #[arg(long)]
        progress_json: String,
    },

    /// Update VM server UUID after migration (internal)
    #[command(name = "update-vm-server-uuid")]
    UpdateVmServerUuid {
        /// VM UUID
        uuid: Uuid,
        /// New server UUID
        #[arg(long)]
        server_uuid: Uuid,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);
    let typed_client = TypedClient::new(&cli.base_url);

    match cli.command {
        // ====================================================================
        // Ping
        // ====================================================================
        Commands::Ping => {
            let resp = client.ping().send().await?;
            println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
        }

        // ====================================================================
        // VMs
        // ====================================================================
        Commands::ListVms {
            owner_uuid,
            server_uuid,
            state,
            brand,
            alias,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_vms();
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            if let Some(v) = server_uuid {
                req = req.server_uuid(v);
            }
            if let Some(v) = state {
                req = req.state(v);
            }
            if let Some(v) = brand {
                req = req.brand(v);
            }
            if let Some(v) = &alias {
                req = req.alias(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req.send().await?;
            let vms = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vms)?);
            } else {
                for vm in &vms {
                    let alias_str = vm.alias.as_deref().unwrap_or("<unnamed>");
                    println!(
                        "{}: {} ({})",
                        vm.uuid,
                        alias_str,
                        enum_to_display(&vm.state)
                    );
                }
                println!("\nTotal: {} VMs", vms.len());
            }
        }

        Commands::HeadVms { owner_uuid, state } => {
            let mut req = client.head_vms();
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            if let Some(v) = state {
                req = req.state(v);
            }
            // HEAD returns response with headers; we just confirm success
            let _resp = req.send().await?;
            println!("VMs exist (HEAD succeeded)");
        }

        Commands::GetVm { uuid, raw } => {
            let resp = client.get_vm().uuid(uuid).send().await?;
            let vm = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vm)?);
            } else {
                let alias_str = vm.alias.as_deref().unwrap_or("<unnamed>");
                println!("UUID: {}", vm.uuid);
                println!("Alias: {}", alias_str);
                println!("Brand: {}", enum_to_display(&vm.brand));
                println!("State: {}", enum_to_display(&vm.state));
                println!("Owner: {}", vm.owner_uuid);
                if let Some(ram) = vm.ram {
                    println!("RAM: {} MB", ram);
                }
                if let Some(server) = vm.server_uuid {
                    println!("Server: {}", server);
                }
            }
        }

        Commands::HeadVm { uuid } => {
            let _resp = client.head_vm().uuid(uuid).send().await?;
            println!("VM {} exists", uuid);
        }

        Commands::CreateVm {
            owner_uuid,
            alias,
            image_uuid,
            billing_id,
            ram,
            server_uuid,
            brand,
        } => {
            // Build CreateVmRequest using serde_json deserialization
            let body: types::CreateVmRequest = serde_json::from_value(serde_json::json!({
                "owner_uuid": owner_uuid,
                "alias": alias,
                "image_uuid": image_uuid,
                "billing_id": billing_id,
                "ram": ram,
                "server_uuid": server_uuid,
                "brand": brand,
            }))
            .map_err(|e| anyhow::anyhow!("Failed to build request: {}", e))?;
            let resp = client.create_vm().body(body).send().await?;
            let result = resp.into_inner();
            println!("VM creation initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::PutVms {
            server_uuid,
            vms_json,
        } => {
            let vms: Vec<serde_json::Value> = serde_json::from_str(&vms_json)?;
            let body: types::PutVmsRequest = serde_json::from_value(serde_json::json!({
                "server_uuid": server_uuid,
                "vms": vms,
            }))
            .map_err(|e| anyhow::anyhow!("Failed to build request: {}", e))?;
            let resp = client.put_vms().body(body).send().await?;
            println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
        }

        Commands::PutVm { uuid, vm_json } => {
            let vm: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&vm_json)?;
            let body = types::PutVmRequest::from(vm);
            let resp = client.put_vm().uuid(uuid).body(body).send().await?;
            println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
        }

        Commands::DeleteVm { uuid } => {
            let resp = client.delete_vm().uuid(uuid).send().await?;
            let result = resp.into_inner();
            println!("VM deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::GetVmProc { uuid, raw } => {
            let resp = client.get_vm_proc().uuid(uuid).send().await?;
            let proc_info = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&proc_info)?);
            } else {
                println!("Process info for VM {}:", uuid);
                println!("{}", serde_json::to_string_pretty(&proc_info)?);
            }
        }

        // ====================================================================
        // VM Actions
        // ====================================================================
        Commands::StartVm { uuid, idempotent } => {
            let result = typed_client.start_vm(&uuid, idempotent).await?;
            println!("VM start initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::StopVm { uuid, idempotent } => {
            let result = typed_client.stop_vm(&uuid, idempotent).await?;
            println!("VM stop initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::KillVm {
            uuid,
            signal,
            idempotent,
        } => {
            let result = typed_client.kill_vm(&uuid, signal, idempotent).await?;
            println!("VM kill initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::RebootVm { uuid, idempotent } => {
            let result = typed_client.reboot_vm(&uuid, idempotent).await?;
            println!("VM reboot initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::ReprovisionVm { uuid, image_uuid } => {
            let result = typed_client.reprovision_vm(&uuid, image_uuid).await?;
            println!("VM reprovision initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::UpdateVm {
            uuid,
            alias,
            ram,
            cpu_cap,
            quota,
            firewall_enabled,
        } => {
            let request = UpdateVmRequest {
                alias,
                ram,
                cpu_cap,
                quota,
                firewall_enabled,
                max_swap: None,
                max_physical_memory: None,
                max_locked_memory: None,
                max_lwps: None,
                zfs_io_priority: None,
                billing_id: None,
                resolvers: None,
                do_not_reboot: None,
                owner_uuid: None,
                customer_metadata: None,
                internal_metadata: None,
                tags: None,
                maintain_resolvers: None,
            };
            let result = typed_client.update_vm(&uuid, &request).await?;
            println!("VM update initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::AddNics {
            uuid,
            networks,
            macs,
        } => {
            let request = AddNicsRequest {
                networks: networks.map(|s| {
                    s.split(',')
                        .map(|v| serde_json::Value::String(v.trim().to_string()))
                        .collect()
                }),
                macs: macs.map(|s| s.split(',').map(|v| v.trim().to_string()).collect()),
            };
            let result = typed_client.add_nics(&uuid, &request).await?;
            println!("Add NICs initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::UpdateNics { uuid, nics_json } => {
            let nics: Vec<vmapi_client::NicSpec> = serde_json::from_str(&nics_json)?;
            let request = UpdateNicsRequest { nics };
            let result = typed_client.update_nics(&uuid, &request).await?;
            println!("Update NICs initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::RemoveNics { uuid, macs } => {
            let macs_vec: Vec<String> = macs.split(',').map(|s| s.trim().to_string()).collect();
            let result = typed_client.remove_nics(&uuid, macs_vec).await?;
            println!("Remove NICs initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::CreateSnapshot { uuid, name } => {
            let result = typed_client.create_snapshot(&uuid, name).await?;
            println!("Snapshot creation initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::RollbackSnapshot { uuid, name } => {
            let result = typed_client.rollback_snapshot(&uuid, name).await?;
            println!("Snapshot rollback initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteSnapshot { uuid, name } => {
            let result = typed_client.delete_snapshot(&uuid, name).await?;
            println!("Snapshot deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::CreateDisk {
            uuid,
            size,
            pci_slot,
        } => {
            let size_value: serde_json::Value = if size == "remaining" {
                serde_json::json!("remaining")
            } else {
                size.parse::<u64>()
                    .map(|n| serde_json::json!(n))
                    .map_err(|_| anyhow::anyhow!("Invalid size: must be a number or 'remaining'"))?
            };
            let request = CreateDiskRequest {
                size: size_value,
                pci_slot,
                disk_uuid: None,
            };
            let result = typed_client.create_disk(&uuid, &request).await?;
            println!("Disk creation initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::ResizeDisk {
            uuid,
            pci_slot,
            size,
            dangerous_allow_shrink,
        } => {
            let request = ResizeDiskRequest {
                pci_slot,
                size,
                dangerous_allow_shrink: if dangerous_allow_shrink {
                    Some(true)
                } else {
                    None
                },
            };
            let result = typed_client.resize_disk(&uuid, &request).await?;
            println!("Disk resize initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteDisk { uuid, pci_slot } => {
            let result = typed_client.delete_disk(&uuid, pci_slot).await?;
            println!("Disk deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::MigrateVm {
            uuid,
            action,
            target_server_uuid,
        } => {
            // Convert Progenitor MigrationAction (with ValueEnum) to API MigrationAction
            let migration_action = action
                .map(|a| serde_json::from_value(serde_json::to_value(a)?))
                .transpose()?;
            let request = MigrateVmRequest {
                migration_action,
                target_server_uuid,
                affinity: None,
            };
            let result = typed_client.migrate_vm(&uuid, &request).await?;
            println!("VM migration initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        // ====================================================================
        // Jobs
        // ====================================================================
        Commands::ListJobs {
            vm_uuid,
            task,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_jobs();
            if let Some(v) = vm_uuid {
                req = req.vm_uuid(v);
            }
            if let Some(v) = &task {
                req = req.task(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req.send().await?;
            let jobs = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else {
                for job in &jobs {
                    println!("{}: {}", job.uuid, enum_to_display(&job.execution));
                }
                println!("\nTotal: {} jobs", jobs.len());
            }
        }

        Commands::GetJob { job_uuid, raw } => {
            let resp = client.get_job().job_uuid(job_uuid).send().await?;
            let job = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&job)?);
            } else {
                println!("Job UUID: {}", job.uuid);
                println!("Execution: {}", enum_to_display(&job.execution));
                if let Some(vm) = job.vm_uuid {
                    println!("VM UUID: {}", vm);
                }
                if let Some(name) = &job.name {
                    println!("Name: {}", name);
                }
            }
        }

        Commands::WaitJob { job_uuid, raw } => {
            let resp = client.wait_job().job_uuid(job_uuid).send().await?;
            let job = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&job)?);
            } else {
                println!("Job completed:");
                println!("  UUID: {}", job.uuid);
                println!("  Execution: {}", enum_to_display(&job.execution));
            }
        }

        Commands::PostJobResults { results_json } => {
            let body: types::PostJobResultsRequest = serde_json::from_str(&results_json)?;
            let resp = client.post_job_results().body(body).send().await?;
            println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
        }

        Commands::ListVmJobs { uuid, limit, raw } => {
            let mut req = client.list_vm_jobs().uuid(uuid);
            if let Some(v) = limit {
                req = req.limit(v);
            }
            let resp = req.send().await?;
            let jobs = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else {
                for job in &jobs {
                    println!("{}: {}", job.uuid, enum_to_display(&job.execution));
                }
                println!("\nTotal: {} jobs for VM {}", jobs.len(), uuid);
            }
        }

        // ====================================================================
        // Customer Metadata
        // ====================================================================
        Commands::ListCustomerMetadata { uuid, raw } => {
            let resp = client.list_customer_metadata().uuid(uuid).send().await?;
            let metadata = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&metadata)?);
            } else {
                println!("Customer metadata for VM {}:", uuid);
                for (key, value) in metadata.iter() {
                    println!("  {}: {}", key, value);
                }
            }
        }

        Commands::GetCustomerMetadata { uuid, key } => {
            let resp = client
                .get_customer_metadata()
                .uuid(uuid)
                .key(&key)
                .send()
                .await?;
            let value = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&value)?);
        }

        Commands::AddCustomerMetadata {
            uuid,
            metadata_json,
        } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&metadata_json)?;
            let body = types::AddMetadataRequest(metadata);
            let resp = client
                .add_customer_metadata()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Customer metadata update initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::SetCustomerMetadata {
            uuid,
            metadata_json,
        } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&metadata_json)?;
            let body = types::SetMetadataRequest(metadata);
            let resp = client
                .set_customer_metadata()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Customer metadata replacement initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteCustomerMetadata { uuid, key } => {
            let resp = client
                .delete_customer_metadata()
                .uuid(uuid)
                .key(&key)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Customer metadata key '{}' deletion initiated:", key);
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteAllCustomerMetadata { uuid } => {
            let resp = client
                .delete_all_customer_metadata()
                .uuid(uuid)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("All customer metadata deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        // ====================================================================
        // Internal Metadata
        // ====================================================================
        Commands::ListInternalMetadata { uuid, raw } => {
            let resp = client.list_internal_metadata().uuid(uuid).send().await?;
            let metadata = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&metadata)?);
            } else {
                println!("Internal metadata for VM {}:", uuid);
                for (key, value) in metadata.iter() {
                    println!("  {}: {}", key, value);
                }
            }
        }

        Commands::GetInternalMetadata { uuid, key } => {
            let resp = client
                .get_internal_metadata()
                .uuid(uuid)
                .key(&key)
                .send()
                .await?;
            let value = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&value)?);
        }

        Commands::AddInternalMetadata {
            uuid,
            metadata_json,
        } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&metadata_json)?;
            let body = types::AddMetadataRequest(metadata);
            let resp = client
                .add_internal_metadata()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Internal metadata update initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::SetInternalMetadata {
            uuid,
            metadata_json,
        } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&metadata_json)?;
            let body = types::SetMetadataRequest(metadata);
            let resp = client
                .set_internal_metadata()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Internal metadata replacement initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteInternalMetadata { uuid, key } => {
            let resp = client
                .delete_internal_metadata()
                .uuid(uuid)
                .key(&key)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("Internal metadata key '{}' deletion initiated:", key);
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteAllInternalMetadata { uuid } => {
            let resp = client
                .delete_all_internal_metadata()
                .uuid(uuid)
                .send()
                .await?;
            let result = resp.into_inner();
            println!("All internal metadata deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        // ====================================================================
        // Tags
        // ====================================================================
        Commands::ListTags { uuid, raw } => {
            let resp = client.list_tags().uuid(uuid).send().await?;
            let tags = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&tags)?);
            } else {
                println!("Tags for VM {}:", uuid);
                for (key, value) in tags.iter() {
                    println!("  {}: {}", key, value);
                }
            }
        }

        Commands::GetTag { uuid, key } => {
            let resp = client.get_tag().uuid(uuid).key(&key).send().await?;
            let value = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&value)?);
        }

        Commands::AddTags { uuid, tags_json } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&tags_json)?;
            let body = types::AddMetadataRequest(metadata);
            let resp = client.add_tags().uuid(uuid).body(body).send().await?;
            let result = resp.into_inner();
            println!("Tags update initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::SetTags { uuid, tags_json } => {
            let metadata: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&tags_json)?;
            let body = types::SetMetadataRequest(metadata);
            let resp = client.set_tags().uuid(uuid).body(body).send().await?;
            let result = resp.into_inner();
            println!("Tags replacement initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteTag { uuid, key } => {
            let resp = client.delete_tag().uuid(uuid).key(&key).send().await?;
            let result = resp.into_inner();
            println!("Tag '{}' deletion initiated:", key);
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        Commands::DeleteAllTags { uuid } => {
            let resp = client.delete_all_tags().uuid(uuid).send().await?;
            let result = resp.into_inner();
            println!("All tags deletion initiated:");
            println!("  VM UUID: {}", result.vm_uuid);
            if let Some(job) = result.job_uuid {
                println!("  Job UUID: {}", job);
            }
        }

        // ====================================================================
        // Role Tags
        // ====================================================================
        Commands::AddRoleTags { uuid, role_tags } => {
            let tags: Vec<String> = role_tags.split(',').map(|s| s.trim().to_string()).collect();
            let body = types::AddRoleTagsRequest { role_tags: tags };
            let resp = client.add_role_tags().uuid(uuid).body(body).send().await?;
            let result = resp.into_inner();
            println!("Role tags: {}", result.role_tags.join(", "));
        }

        Commands::SetRoleTags { uuid, role_tags } => {
            let tags: Vec<String> = role_tags.split(',').map(|s| s.trim().to_string()).collect();
            let body = types::SetRoleTagsRequest { role_tags: tags };
            let resp = client.set_role_tags().uuid(uuid).body(body).send().await?;
            let result = resp.into_inner();
            println!("Role tags: {}", result.role_tags.join(", "));
        }

        Commands::DeleteRoleTag { uuid, role_tag } => {
            client
                .delete_role_tag()
                .uuid(uuid)
                .role_tag(role_tag)
                .send()
                .await?;
            println!("Role tag {} deleted from VM {}", role_tag, uuid);
        }

        Commands::DeleteAllRoleTags { uuid } => {
            client.delete_all_role_tags().uuid(uuid).send().await?;
            println!("All role tags deleted from VM {}", uuid);
        }

        // ====================================================================
        // Statuses
        // ====================================================================
        Commands::ListStatuses { uuids, raw } => {
            let uuid_str: String = uuids
                .split(',')
                .map(|s| s.trim())
                .collect::<Vec<_>>()
                .join(",");
            let resp = client.list_statuses().uuids(uuid_str).send().await?;
            let statuses = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&statuses)?);
            } else {
                println!("VM Statuses:");
                for (uuid, status) in statuses.iter() {
                    println!("  {}: {}", uuid, enum_to_display(&status.state));
                }
            }
        }

        // ====================================================================
        // Migrations
        // ====================================================================
        Commands::ListMigrations {
            state,
            source_server_uuid,
            limit,
            raw,
        } => {
            let mut req = client.list_migrations();
            if let Some(v) = state {
                req = req.state(v);
            }
            if let Some(v) = source_server_uuid {
                req = req.source_server_uuid(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            let resp = req.send().await?;
            let migrations = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&migrations)?);
            } else {
                for m in &migrations {
                    println!(
                        "{}: {} ({})",
                        m.vm_uuid,
                        enum_to_display(&m.state),
                        enum_to_display(&m.phase)
                    );
                }
                println!("\nTotal: {} migrations", migrations.len());
            }
        }

        Commands::GetMigration { uuid, raw } => {
            let resp = client.get_migration().uuid(uuid).send().await?;
            let migration = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&migration)?);
            } else {
                println!("Migration for VM {}:", migration.vm_uuid);
                println!("  State: {}", enum_to_display(&migration.state));
                println!("  Phase: {}", enum_to_display(&migration.phase));
                if let Some(src) = migration.source_server_uuid {
                    println!("  Source server: {}", src);
                }
                if let Some(tgt) = migration.target_server_uuid {
                    println!("  Target server: {}", tgt);
                }
            }
        }

        Commands::DeleteMigration { uuid } => {
            client.delete_migration().uuid(uuid).send().await?;
            println!("Migration record deleted for VM {}", uuid);
        }

        Commands::StoreMigrationRecord { uuid, record_json } => {
            let body: types::StoreMigrationRecordRequest = serde_json::from_str(&record_json)?;
            let resp = client
                .store_migration_record()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let migration = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&migration)?);
        }

        Commands::ReportMigrationProgress {
            uuid,
            progress_json,
        } => {
            let body: types::MigrationProgressRequest = serde_json::from_str(&progress_json)?;
            let resp = client
                .report_migration_progress()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let migration = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&migration)?);
        }

        Commands::UpdateVmServerUuid { uuid, server_uuid } => {
            let body = types::UpdateVmServerUuidRequest { server_uuid };
            let resp = client
                .update_vm_server_uuid()
                .uuid(uuid)
                .body(body)
                .send()
                .await?;
            let vm = resp.into_inner();
            println!("VM {} server UUID updated to {}", vm.uuid, server_uuid);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Test that the CLI structure is valid and has no conflicts.
    #[test]
    fn verify_cli_structure() {
        Cli::command().debug_assert();
    }
}
