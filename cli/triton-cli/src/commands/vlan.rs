// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Fabric VLAN management commands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct VlanListArgs {
    /// Filters in FIELD=VALUE format (e.g., vlan_id=2, name=myvlan)
    #[arg(value_name = "FILTER")]
    pub filters: Vec<String>,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum VlanCommand {
    /// List VLANs
    #[command(alias = "ls")]
    List(VlanListArgs),
    /// Get VLAN details
    Get(VlanGetArgs),
    /// Create VLAN
    Create(VlanCreateArgs),
    /// Delete VLAN
    #[command(alias = "rm")]
    Delete(VlanDeleteArgs),
    /// Update VLAN
    Update(VlanUpdateArgs),
    /// List networks on VLAN
    Networks(VlanNetworksArgs),
}

#[derive(Args, Clone)]
pub struct VlanGetArgs {
    /// VLAN ID or name
    pub vlan: String,
}

#[derive(Args, Clone)]
pub struct VlanCreateArgs {
    /// VLAN ID (1-4095) - positional argument
    pub vlan_id: u16,

    /// VLAN name
    #[arg(long, short = 'n')]
    pub name: String,

    /// Description
    #[arg(long, short = 'D')]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct VlanDeleteArgs {
    /// VLAN ID(s) or name(s)
    #[arg(required = true)]
    pub vlans: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct VlanUpdateArgs {
    /// VLAN ID or name
    pub vlan: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
    /// Read update data from JSON file (use '-' for stdin)
    #[arg(short = 'f', long = "file")]
    pub file: Option<PathBuf>,
}

#[derive(Args, Clone)]
pub struct VlanNetworksArgs {
    /// VLAN ID or name
    pub vlan: String,

    #[command(flatten)]
    pub table: TableFormatArgs,
}

impl VlanCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_vlans(args, client, use_json).await,
            Self::Get(args) => get_vlan(args, client, use_json).await,
            Self::Create(args) => create_vlan(args, client, use_json).await,
            Self::Delete(args) => delete_vlans(args, client).await,
            Self::Update(args) => update_vlan(args, client, use_json).await,
            Self::Networks(args) => list_vlan_networks(args, client, use_json).await,
        }
    }
}

async fn list_vlans(args: VlanListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await?;

    let vlans = response.into_inner();

    // Apply client-side filters (like node-triton)
    let vlans: Vec<_> = vlans
        .into_iter()
        .filter(|vlan| {
            for filter in &args.filters {
                if let Some((field, value)) = filter.split_once('=') {
                    match field {
                        "vlan_id" => {
                            if let Ok(id) = value.parse::<u16>()
                                && vlan.vlan_id != id
                            {
                                return false;
                            }
                        }
                        "name" => {
                            if vlan.name != value {
                                return false;
                            }
                        }
                        "description" => {
                            let desc = vlan.description.as_deref().unwrap_or("");
                            if desc != value {
                                return false;
                            }
                        }
                        _ => {} // Ignore unknown fields
                    }
                }
            }
            true
        })
        .collect();

    if use_json {
        // Output NDJSON format (one JSON object per line) like node-triton
        for vlan in &vlans {
            println!("{}", serde_json::to_string(&vlan)?);
        }
    } else {
        let mut tbl = TableBuilder::new(&["VLAN_ID", "NAME", "DESCRIPTION"]);
        for vlan in &vlans {
            tbl.add_row(vec![
                vlan.vlan_id.to_string(),
                vlan.name.clone(),
                vlan.description.clone().unwrap_or_else(|| "-".to_string()),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

async fn get_vlan(args: VlanGetArgs, client: &TypedClient, _use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let vlan_id = resolve_vlan(&args.vlan, client).await?;

    let response = client
        .inner()
        .get_fabric_vlan()
        .account(account)
        .vlan_id(vlan_id)
        .send()
        .await?;

    let vlan = response.into_inner();

    // Always output JSON for 'get' commands (matching node-triton behavior)
    json::print_json(&vlan)?;

    Ok(())
}

/// Resolve VLAN name or ID to numeric VLAN ID
async fn resolve_vlan(id_or_name: &str, client: &TypedClient) -> Result<u16> {
    // Try parsing as numeric ID first
    if let Ok(vlan_id) = id_or_name.parse::<u16>() {
        return Ok(vlan_id);
    }

    // Otherwise, look up by name
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await?;

    let vlans = response.into_inner();

    for vlan in &vlans {
        if vlan.name == id_or_name {
            return Ok(vlan.vlan_id);
        }
    }

    Err(anyhow::anyhow!("VLAN not found: {}", id_or_name))
}

async fn create_vlan(args: VlanCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::CreateFabricVlanRequest {
        vlan_id: args.vlan_id,
        name: args.name.clone(),
        description: args.description.clone(),
    };

    let response = client
        .inner()
        .create_fabric_vlan()
        .account(account)
        .body(request)
        .send()
        .await?;
    let vlan = response.into_inner();

    if use_json {
        // Output JSON only (node-triton compat)
        json::print_json(&vlan)?;
    } else {
        println!("Created VLAN {} ({})", vlan.vlan_id, vlan.name);
    }

    Ok(())
}

async fn delete_vlans(args: VlanDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for vlan in &args.vlans {
        let vlan_id = resolve_vlan(vlan, client).await?;

        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete VLAN {}?", vlan_id))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_fabric_vlan()
            .account(account)
            .vlan_id(vlan_id)
            .send()
            .await?;

        println!("Deleted VLAN {}", vlan_id);
    }

    Ok(())
}

async fn update_vlan(args: VlanUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let vlan_id = resolve_vlan(&args.vlan, client).await?;

    // Parse update data from file or command line
    let (name, description) = if let Some(file_path) = &args.file {
        let content = if file_path.as_os_str() == "-" {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            std::fs::read_to_string(file_path)?
        };
        let data: serde_json::Value = serde_json::from_str(&content)?;
        let name = data
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or(args.name.clone());
        let description = data
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or(args.description.clone());
        (name, description)
    } else {
        (args.name.clone(), args.description.clone())
    };

    let request = cloudapi_client::types::UpdateFabricVlanRequest { name, description };

    let response = client
        .inner()
        .update_fabric_vlan()
        .account(account)
        .vlan_id(vlan_id)
        .body(request)
        .send()
        .await?;
    let vlan = response.into_inner();

    println!("Updated VLAN {}", vlan.vlan_id);

    if use_json {
        json::print_json(&vlan)?;
    }

    Ok(())
}

async fn list_vlan_networks(
    args: VlanNetworksArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let vlan_id = resolve_vlan(&args.vlan, client).await?;

    let response = client
        .inner()
        .list_fabric_networks()
        .account(account)
        .vlan_id(vlan_id)
        .send()
        .await?;

    let networks = response.into_inner();

    if use_json {
        // Output NDJSON format (one JSON object per line) like node-triton
        for net in &networks {
            println!("{}", serde_json::to_string(&net)?);
        }
    } else {
        let mut tbl = TableBuilder::new(&["SHORTID", "NAME", "SUBNET", "GATEWAY"])
            .with_long_headers(&["ID", "PUBLIC"]);
        for net in &networks {
            tbl.add_row(vec![
                net.id.to_string()[..8].to_string(),
                net.name.clone(),
                net.subnet.clone().unwrap_or_else(|| "-".to_string()),
                net.gateway.clone().unwrap_or_else(|| "-".to_string()),
                net.id.to_string(),
                if net.public { "yes" } else { "no" }.to_string(),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}
