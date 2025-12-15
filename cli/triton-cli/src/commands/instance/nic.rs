// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance NIC subcommands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use dialoguer::Confirm;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum NicCommand {
    /// List NICs on an instance
    #[command(alias = "ls")]
    List(NicListArgs),

    /// Get NIC details
    Get(NicGetArgs),

    /// Add a NIC to an instance
    Add(NicAddArgs),

    /// Remove a NIC from an instance
    #[command(alias = "rm")]
    Remove(NicRemoveArgs),
}

#[derive(Args, Clone)]
pub struct NicListArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct NicGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// NIC MAC address
    pub mac: String,
}

#[derive(Args, Clone)]
pub struct NicAddArgs {
    /// Instance ID or name
    pub instance: String,

    /// Network ID
    #[arg(long)]
    pub network: String,
}

#[derive(Args, Clone)]
pub struct NicRemoveArgs {
    /// Instance ID or name
    pub instance: String,

    /// NIC MAC address
    pub mac: String,

    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl NicCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_nics(args, client, use_json).await,
            Self::Get(args) => get_nic(args, client, use_json).await,
            Self::Add(args) => add_nic(args, client).await,
            Self::Remove(args) => remove_nic(args, client).await,
        }
    }
}

async fn list_nics(args: NicListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_nics()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let nics = response.into_inner();

    if use_json {
        json::print_json(&nics)?;
    } else {
        let mut tbl = table::create_table(&["MAC", "IP", "NETWORK", "PRIMARY"]);
        for nic in &nics {
            tbl.add_row(vec![
                &nic.mac,
                &nic.ip,
                &nic.network.to_string(),
                &(if nic.primary { "yes" } else { "no" }).to_string(),
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_nic(args: NicGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_nic()
        .account(account)
        .machine(&machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    let nic = response.into_inner();

    if use_json {
        json::print_json(&nic)?;
    } else {
        println!("MAC:     {}", nic.mac);
        println!("IP:      {}", nic.ip);
        println!("Network: {}", nic.network);
        println!("Primary: {}", nic.primary);
    }

    Ok(())
}

async fn add_nic(args: NicAddArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let request = cloudapi_client::types::AddNicRequest {
        network: args.network.clone(),
    };

    let response = client
        .inner()
        .add_nic()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    let nic = response.into_inner();
    println!("Added NIC {} with IP {}", nic.mac, nic.ip);

    Ok(())
}

async fn remove_nic(args: NicRemoveArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Remove NIC {}?", args.mac))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .inner()
        .remove_nic()
        .account(account)
        .machine(&machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    println!("Removed NIC {}", args.mac);

    Ok(())
}
