// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance disk subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::Disk;
use dialoguer::Confirm;
use std::io::IsTerminal;

use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::output::{json, opt_enum_to_display};

#[derive(Subcommand, Clone)]
pub enum DiskCommand {
    /// List disks on an instance
    #[command(visible_alias = "ls")]
    List(DiskListArgs),

    /// Get disk details
    Get(DiskGetArgs),

    /// Add a disk to an instance
    Add(DiskAddArgs),

    /// Resize a disk
    Resize(DiskResizeArgs),

    /// Delete a disk
    #[command(visible_alias = "rm")]
    Delete(DiskDeleteArgs),
}

#[derive(Args, Clone)]
pub struct DiskListArgs {
    /// Instance ID or name
    pub instance: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Args, Clone)]
pub struct DiskGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Disk ID
    pub disk: String,
}

#[derive(Args, Clone)]
pub struct DiskAddArgs {
    /// Instance ID or name
    pub instance: String,

    /// Disk size in MiB
    pub size: i64,

    /// Disk name (optional, must be unique per instance)
    #[arg(long)]
    pub name: Option<String>,

    /// Wait for disk addition to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct DiskResizeArgs {
    /// Instance ID or name
    pub instance: String,

    /// Disk ID
    pub disk: String,

    /// New disk size in MiB (can only increase)
    #[arg(long)]
    pub size: i64,

    /// Allow dangerous resize (may cause data loss)
    #[arg(long)]
    pub dangerous_allow_shrink: bool,
}

#[derive(Args, Clone)]
pub struct DiskDeleteArgs {
    /// Instance ID or name
    pub instance: String,

    /// Disk ID
    pub disk: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl DiskCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_disks(args, client, use_json).await,
            Self::Get(args) => get_disk(args, client, use_json).await,
            Self::Add(args) => add_disk(args, client, use_json).await,
            Self::Resize(args) => resize_disk(args, client).await,
            Self::Delete(args) => delete_disk(args, client).await,
        }
    }
}

pub async fn list_disks(args: DiskListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_machine_disks()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let disks = response.into_inner();

    if use_json {
        json::print_json_stream(&disks)?;
    } else {
        let short_cols = ["shortid", "size", "pci_slot"];
        let long_cols = ["id", "boot", "state"];

        let mut tbl = TableBuilder::new(&["SHORTID", "SIZE", "PCI_SLOT"])
            .with_long_headers(&["ID", "BOOT", "STATE"])
            .with_right_aligned(&["SIZE"]);

        let all_cols: Vec<&str> = short_cols.iter().chain(long_cols.iter()).copied().collect();
        for disk in &disks {
            let row = all_cols
                .iter()
                .map(|col| get_disk_field_value(disk, col))
                .collect();
            tbl.add_row(row);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

fn get_disk_field_value(disk: &Disk, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "id" => disk.id.to_string(),
        "shortid" => disk.id.to_string()[..8].to_string(),
        "size" => disk.size.to_string(),
        "pci_slot" | "pci slot" => disk.pci_slot.clone().unwrap_or_else(|| "-".to_string()),
        "boot" => {
            if disk.boot.unwrap_or(false) {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        "state" => opt_enum_to_display(disk.state.as_ref()),
        _ => "-".to_string(),
    }
}

async fn get_disk(args: DiskGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let disk_id: uuid::Uuid = args.disk.parse()?;

    let response = client
        .inner()
        .get_machine_disk()
        .account(account)
        .machine(machine_id)
        .disk(disk_id)
        .send()
        .await?;

    let disk = response.into_inner();

    if use_json {
        json::print_json(&disk)?;
    } else {
        json::print_json_pretty(&disk)?;
    }

    Ok(())
}

async fn add_disk(args: DiskAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let id_str = machine_id.to_string();

    // List existing disks before adding (baseline for --wait)
    let _existing_disks = client
        .inner()
        .list_machine_disks()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let request = cloudapi_client::types::CreateDiskRequest {
        size: args.size as u64,
        pci_slot: None,
    };

    let response = client
        .inner()
        .create_machine_disk()
        .account(account)
        .machine(machine_id)
        .body(request)
        .send()
        .await?;

    let disk = response.into_inner();

    println!(
        "Added disk {} ({} MiB)",
        &disk.id.to_string()[..8],
        disk.size
    );

    if args.wait {
        super::wait::wait_for_state(
            machine_id,
            cloudapi_client::types::MachineState::Running,
            args.wait_timeout,
            client,
        )
        .await?;
        println!("Instance {} is running", &id_str[..8]);
    }

    if use_json {
        json::print_json(&disk)?;
    }

    Ok(())
}

async fn resize_disk(args: DiskResizeArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let disk_id: uuid::Uuid = args.disk.parse()?;

    let request = cloudapi_client::ResizeDiskRequest {
        size: args.size as u64,
        dangerous_allow_shrink: Some(args.dangerous_allow_shrink),
    };

    client
        .resize_disk(account, &machine_id, &disk_id, &request)
        .await?;

    println!("Resizing disk {} to {} MiB", &args.disk[..8], args.size);

    Ok(())
}

async fn delete_disk(args: DiskDeleteArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && std::io::stdin().is_terminal()
        && !Confirm::new()
            .with_prompt(format!("Delete disk {}?", &args.disk))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let disk_id: uuid::Uuid = args.disk.parse()?;

    client
        .inner()
        .delete_machine_disk()
        .account(account)
        .machine(machine_id)
        .disk(disk_id)
        .send()
        .await?;

    println!("Deleted disk {}", &args.disk[..8]);

    Ok(())
}
