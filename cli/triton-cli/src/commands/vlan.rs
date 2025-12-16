// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Fabric VLAN management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum VlanCommand {
    /// List VLANs
    #[command(alias = "ls")]
    List,
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
    /// VLAN ID
    pub vlan_id: u16,
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
    /// VLAN ID(s)
    pub vlan_ids: Vec<u16>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct VlanUpdateArgs {
    /// VLAN ID
    pub vlan_id: u16,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Args, Clone)]
pub struct VlanNetworksArgs {
    /// VLAN ID
    pub vlan_id: u16,
}

impl VlanCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_vlans(client, use_json).await,
            Self::Get(args) => get_vlan(args, client, use_json).await,
            Self::Create(args) => create_vlan(args, client, use_json).await,
            Self::Delete(args) => delete_vlans(args, client).await,
            Self::Update(args) => update_vlan(args, client, use_json).await,
            Self::Networks(args) => list_vlan_networks(args, client, use_json).await,
        }
    }
}

async fn list_vlans(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await?;

    let vlans = response.into_inner();

    if use_json {
        json::print_json(&vlans)?;
    } else {
        let mut tbl = table::create_table(&["VLAN_ID", "NAME", "DESCRIPTION"]);
        for vlan in &vlans {
            tbl.add_row(vec![
                &vlan.vlan_id.to_string(),
                &vlan.name,
                vlan.description.as_deref().unwrap_or("-"),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_vlan(args: VlanGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .get_fabric_vlan()
        .account(account)
        .vlan_id(args.vlan_id)
        .send()
        .await?;

    let vlan = response.into_inner();

    if use_json {
        json::print_json(&vlan)?;
    } else {
        println!("VLAN ID:     {}", vlan.vlan_id);
        println!("Name:        {}", vlan.name);
        if let Some(desc) = &vlan.description {
            println!("Description: {}", desc);
        }
    }

    Ok(())
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

    println!("Created VLAN {} ({})", vlan.vlan_id, vlan.name);

    if use_json {
        json::print_json(&vlan)?;
    }

    Ok(())
}

async fn delete_vlans(args: VlanDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for vlan_id in &args.vlan_ids {
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
            .vlan_id(*vlan_id)
            .send()
            .await?;

        println!("Deleted VLAN {}", vlan_id);
    }

    Ok(())
}

async fn update_vlan(args: VlanUpdateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::UpdateFabricVlanRequest {
        name: args.name.clone(),
        description: args.description.clone(),
    };

    let response = client
        .inner()
        .update_fabric_vlan()
        .account(account)
        .vlan_id(args.vlan_id)
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
    let response = client
        .inner()
        .list_fabric_networks()
        .account(account)
        .vlan_id(args.vlan_id)
        .send()
        .await?;

    let networks = response.into_inner();

    if use_json {
        json::print_json(&networks)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "SUBNET", "GATEWAY"]);
        for net in &networks {
            tbl.add_row(vec![
                &net.id.to_string()[..8],
                &net.name,
                net.subnet.as_deref().unwrap_or("-"),
                net.gateway.as_deref().unwrap_or("-"),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}
