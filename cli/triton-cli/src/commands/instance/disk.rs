// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance disk subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_api::Disk;
use dialoguer::Confirm;
use std::io::IsTerminal;

use crate::client::AnyClient;
use crate::define_columns;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::output::{json, opt_enum_to_display};
use crate::{dispatch, dispatch_with_types};

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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_disks(args, client, use_json).await,
            Self::Get(args) => get_disk(args, client, use_json).await,
            Self::Add(args) => add_disk(args, client, use_json).await,
            Self::Resize(args) => resize_disk(args, client).await,
            Self::Delete(args) => delete_disk(args, client).await,
        }
    }
}

pub async fn list_disks(args: DiskListArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    let mut disks: Vec<Disk> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_machine_disks()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Vec<Disk>>(serde_json::to_value(&resp)?)?
    });

    disks.sort_by(|a, b| {
        let slot_cmp = a.pci_slot.cmp(&b.pci_slot);
        if slot_cmp == std::cmp::Ordering::Equal {
            a.id.cmp(&b.id)
        } else {
            slot_cmp
        }
    });

    if use_json {
        json::print_json_stream(&disks)?;
    } else {
        define_columns! {
            DiskColumn for Disk, long_from: 3, {
                ShortId("SHORTID") => |disk| disk.id.to_string()[..8].to_string(),
                Size("SIZE") => |disk| disk.size.to_string(),
                PciSlot("PCI_SLOT") => |disk| {
                    disk.pci_slot.clone().unwrap_or_else(|| "-".to_string())
                },
                // --- long-only columns below ---
                Id("ID") => |disk| disk.id.to_string(),
                Boot("BOOT") => |disk| {
                    if disk.boot.unwrap_or(false) { "yes".to_string() } else { "no".to_string() }
                },
                State("STATE") => |disk| opt_enum_to_display(disk.state.as_ref()),
            }
        }

        TableBuilder::from_enum_columns::<DiskColumn, _>(&disks, Some(DiskColumn::LONG_FROM))
            .with_right_aligned(&["SIZE"])
            .print(&args.table)?;
    }

    Ok(())
}

async fn get_disk(args: DiskGetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let disk_id: uuid::Uuid = args.disk.parse()?;

    let disk_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_machine_disk()
            .account(account)
            .machine(machine_id)
            .disk(disk_id)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&disk_json)?;
    } else {
        json::print_json_pretty(&disk_json)?;
    }

    Ok(())
}

async fn add_disk(args: DiskAddArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    let size = args.size as u64;

    let (disk_id_short, disk_size, disk_json): (String, u64, serde_json::Value) =
        dispatch_with_types!(client, |c, t| {
            let body = t::CreateDiskRequest {
                size,
                pci_slot: None,
            };
            let resp = c
                .inner()
                .create_machine_disk()
                .account(account)
                .machine(machine_id)
                .body(body)
                .send()
                .await?
                .into_inner();
            let value = serde_json::to_value(&resp)?;
            let id_short = value
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s[..8.min(s.len())].to_string())
                .unwrap_or_default();
            let size = value.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            (id_short, size, value)
        });

    eprintln!("Added disk {} ({} MiB)", disk_id_short, disk_size);

    if args.wait {
        super::wait::wait_for_state(
            machine_id,
            cloudapi_client::types::MachineState::Running,
            args.wait_timeout,
            client,
        )
        .await?;
        eprintln!("Instance {} is running", &id_str[..8]);
    }

    if use_json {
        json::print_json(&disk_json)?;
    }

    Ok(())
}

async fn resize_disk(args: DiskResizeArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let disk_id: uuid::Uuid = args.disk.parse()?;

    // `resize_machine_disk` is an action-dispatch endpoint: the generated
    // builder takes `serde_json::Value` so the body is literal JSON.
    let body = serde_json::json!({
        "action": "resize",
        "size": args.size,
        "dangerous_allow_shrink": args.dangerous_allow_shrink,
    });

    dispatch!(client, |c| {
        c.inner()
            .resize_machine_disk()
            .account(account)
            .machine(machine_id)
            .disk(disk_id)
            .body(body)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    println!("Resizing disk {} to {} MiB", &args.disk[..8], args.size);

    Ok(())
}

async fn delete_disk(args: DiskDeleteArgs, client: &AnyClient) -> Result<()> {
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
    let account = client.effective_account();
    let disk_id: uuid::Uuid = args.disk.parse()?;

    dispatch!(client, |c| {
        c.inner()
            .delete_machine_disk()
            .account(account)
            .machine(machine_id)
            .disk(disk_id)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    println!("Deleted disk {}", &args.disk[..8]);

    Ok(())
}
