// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance disk subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use dialoguer::Confirm;

use crate::output::{json, table};

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
    #[arg(long)]
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
        let mut tbl = table::create_table(&["SHORTID", "SIZE_MB", "BOOT", "STATE"]);
        for disk in &disks {
            let short_id = &disk.id.to_string()[..8];
            tbl.add_row(vec![
                short_id,
                &disk.size.to_string(),
                if disk.boot.unwrap_or(false) {
                    "yes"
                } else {
                    "no"
                },
                &format!("{:?}", disk.state).to_lowercase(),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
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
        println!("ID:    {}", disk.id);
        println!("Size:  {} MiB", disk.size);
        println!("Boot:  {}", disk.boot.unwrap_or(false));
        println!("State: {:?}", disk.state);
    }

    Ok(())
}

async fn add_disk(args: DiskAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;
    let id_str = machine_id.to_string();

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
        super::wait::wait_for_state(machine_id, "running", args.wait_timeout, client).await?;
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
