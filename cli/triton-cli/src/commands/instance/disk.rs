// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance disk subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use dialoguer::Confirm;
use std::collections::HashSet;
use std::fmt;
use std::io::IsTerminal;
use std::str::FromStr;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::{CreateDiskRequest, Disk, DiskSize};

use crate::define_columns;
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

    /// Disk size in MiB or "remaining"
    pub size: AddDiskSize,

    /// Block size in bytes
    pub block_size: Option<u64>,

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddDiskSize {
    Mib(u64),
    Remaining,
}

impl AddDiskSize {
    fn to_wire_size(&self) -> DiskSize {
        match self {
            Self::Mib(size) => DiskSize::Uint64(*size),
            Self::Remaining => DiskSize::String("remaining".to_string()),
        }
    }
}

impl FromStr for AddDiskSize {
    type Err = String;

    fn from_str(size: &str) -> std::result::Result<Self, Self::Err> {
        if size == "remaining" {
            return Ok(Self::Remaining);
        }

        let size = size
            .parse::<u64>()
            .map_err(|_| "SIZE must be a number or \"remaining\"".to_string())?;

        if size == 0 {
            return Err("SIZE must be greater than zero or \"remaining\"".to_string());
        }

        Ok(Self::Mib(size))
    }
}

impl fmt::Display for AddDiskSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mib(size) => write!(f, "{size}"),
            Self::Remaining => f.write_str("remaining"),
        }
    }
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
    let account = client.effective_account();

    let response = client
        .inner()
        .list_machine_disks()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let mut disks = response.into_inner();
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

async fn get_disk(args: DiskGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
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
    let account = client.effective_account();
    let id_str = machine_id.to_string();

    // List existing disks before adding (baseline for --wait)
    let existing_disk_ids: HashSet<_> = client
        .inner()
        .list_machine_disks()
        .account(account)
        .machine(machine_id)
        .send()
        .await?
        .into_inner()
        .into_iter()
        .map(|disk| disk.id)
        .collect();

    let request = build_create_disk_request(&args.size, args.block_size);

    client
        .inner()
        .create_machine_disk()
        .account(account)
        .machine(machine_id)
        .body(request)
        .send()
        .await?;

    if !use_json {
        match &args.size {
            AddDiskSize::Mib(size) => eprintln!("Adding disk ({size} MiB)"),
            AddDiskSize::Remaining => eprintln!("Adding disk (remaining)"),
        }
    }

    if args.wait {
        super::wait::wait_for_new_disk(machine_id, &existing_disk_ids, args.wait_timeout, client)
            .await?;
        eprintln!("Disk addition complete for {}", &id_str[..8]);
    }

    Ok(())
}

fn build_create_disk_request(size: &AddDiskSize, block_size: Option<u64>) -> CreateDiskRequest {
    CreateDiskRequest {
        size: size.to_wire_size(),
        block_size,
        pci_slot: None,
    }
}

async fn resize_disk(args: DiskResizeArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let disk_id: uuid::Uuid = args.disk.parse()?;

    let request = triton_gateway_client::ResizeDiskRequest {
        size: args.size as u64,
        dangerous_allow_shrink: Some(args.dangerous_allow_shrink),
    };

    client
        .resize_disk(account, &machine_id, &disk_id, &request)
        .await?;

    println!("Resizing disk {} to {} MiB", &args.disk[..8], args.size);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_create_disk_request_with_numeric_size_and_block_size() {
        let request = build_create_disk_request(&AddDiskSize::Mib(2048), Some(8192));

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "size": 2048,
                "block_size": 8192
            })
        );
    }

    #[test]
    fn builds_create_disk_request_with_remaining_size() {
        let request = build_create_disk_request(&AddDiskSize::Remaining, None);

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "size": "remaining"
            })
        );
    }
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
    let account = client.effective_account();
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
