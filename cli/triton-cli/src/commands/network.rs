// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Network management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum NetworkCommand {
    /// List networks
    #[command(alias = "ls")]
    List,
    /// Get network details
    Get(NetworkGetArgs),
    /// Get default network
    GetDefault,
    /// Set default network
    SetDefault(NetworkSetDefaultArgs),
    /// Manage network IPs
    Ip {
        #[command(subcommand)]
        command: NetworkIpCommand,
    },
}

#[derive(Subcommand, Clone)]
pub enum NetworkIpCommand {
    /// List IPs in a network
    #[command(alias = "ls")]
    List(NetworkIpListArgs),
    /// Get IP details
    Get(NetworkIpGetArgs),
    /// Update IP reservation
    Update(NetworkIpUpdateArgs),
}

#[derive(Args, Clone)]
pub struct NetworkGetArgs {
    /// Network ID or name
    pub network: String,
}

#[derive(Args, Clone)]
pub struct NetworkSetDefaultArgs {
    /// Network ID or name
    pub network: String,
}

#[derive(Args, Clone)]
pub struct NetworkIpListArgs {
    /// Network ID
    pub network: String,
}

#[derive(Args, Clone)]
pub struct NetworkIpGetArgs {
    /// Network ID
    pub network: String,
    /// IP address
    pub ip: String,
}

#[derive(Args, Clone)]
pub struct NetworkIpUpdateArgs {
    /// Network ID
    pub network: String,
    /// IP address
    pub ip: String,
    /// Reserve the IP
    #[arg(long)]
    pub reserve: Option<bool>,
}

impl NetworkCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List => list_networks(client, use_json).await,
            Self::Get(args) => get_network(args, client, use_json).await,
            Self::GetDefault => get_default_network(client, use_json).await,
            Self::SetDefault(args) => set_default_network(args, client).await,
            Self::Ip { command } => command.run(client, use_json).await,
        }
    }
}

impl NetworkIpCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_network_ips(args, client, use_json).await,
            Self::Get(args) => get_network_ip(args, client, use_json).await,
            Self::Update(args) => update_network_ip(args, client, use_json).await,
        }
    }
}

async fn list_networks(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let networks = response.into_inner();

    if use_json {
        json::print_json(&networks)?;
    } else {
        let mut tbl = table::create_table(&["SHORTID", "NAME", "SUBNET", "GATEWAY", "PUBLIC"]);
        for net in &networks {
            tbl.add_row(vec![
                &net.id.to_string()[..8],
                &net.name,
                net.subnet.as_deref().unwrap_or("-"),
                net.gateway.as_deref().unwrap_or("-"),
                if net.public { "yes" } else { "no" },
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_network(args: NetworkGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let network_id = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .get_network()
        .account(account)
        .network(&network_id)
        .send()
        .await?;

    let network = response.into_inner();

    if use_json {
        json::print_json(&network)?;
    } else {
        println!("ID:      {}", network.id);
        println!("Name:    {}", network.name);
        println!("Subnet:  {}", network.subnet.as_deref().unwrap_or("-"));
        println!("Gateway: {}", network.gateway.as_deref().unwrap_or("-"));
        println!("Public:  {}", network.public);
        if let Some(fabric) = network.fabric {
            println!("Fabric:  {}", fabric);
        }
    }

    Ok(())
}

async fn get_default_network(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client.inner().get_config().account(account).send().await?;

    let config = response.into_inner();

    if use_json {
        json::print_json(&config)?;
    } else if let Some(default_network) = &config.default_network {
        println!("{}", default_network);
    } else {
        println!("No default network configured");
    }

    Ok(())
}

async fn set_default_network(args: NetworkSetDefaultArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;
    let network_id = resolve_network(&args.network, client).await?;
    let network_uuid: cloudapi_client::Uuid = network_id.parse()?;

    let request = cloudapi_client::types::UpdateConfigRequest {
        default_network: Some(network_uuid.to_string()),
    };

    client
        .inner()
        .update_config()
        .account(account)
        .body(request)
        .send()
        .await?;

    println!("Default network set to {}", network_id);

    Ok(())
}

async fn list_network_ips(
    args: NetworkIpListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let network_id = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .list_network_ips()
        .account(account)
        .network(&network_id)
        .send()
        .await?;

    let ips = response.into_inner();

    if use_json {
        json::print_json(&ips)?;
    } else {
        let mut tbl = table::create_table(&["IP", "RESERVED", "MANAGED", "OWNER"]);
        for ip in &ips {
            let reserved_str = if ip.reserved { "yes".to_string() } else { "no".to_string() };
            let managed_str = if ip.managed.unwrap_or(false) { "yes".to_string() } else { "no".to_string() };
            let owner_str = ip
                .owner_uuid
                .as_ref()
                .map(|u| u.to_string()[..8].to_string())
                .unwrap_or_else(|| "-".to_string());
            tbl.add_row(vec![
                &ip.ip,
                &reserved_str,
                &managed_str,
                &owner_str,
            ]);
        }
        table::print_table(tbl);
    }

    Ok(())
}

async fn get_network_ip(
    args: NetworkIpGetArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let network_id = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .get_network_ip()
        .account(account)
        .network(&network_id)
        .ip_address(&args.ip)
        .send()
        .await?;

    let ip = response.into_inner();

    if use_json {
        json::print_json(&ip)?;
    } else {
        println!("IP:       {}", ip.ip);
        println!("Reserved: {}", ip.reserved);
        println!("Managed:  {}", ip.managed.unwrap_or(false));
        if let Some(owner) = &ip.owner_uuid {
            println!("Owner:    {}", owner);
        }
    }

    Ok(())
}

async fn update_network_ip(
    args: NetworkIpUpdateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let network_id = resolve_network(&args.network, client).await?;

    let reserved = args.reserve.unwrap_or(false);
    let request = cloudapi_client::types::UpdateNetworkIpRequest { reserved };

    let response = client
        .inner()
        .update_network_ip()
        .account(account)
        .network(&network_id)
        .ip_address(&args.ip)
        .body(request)
        .send()
        .await?;

    let ip = response.into_inner();

    println!("Updated IP {}", ip.ip);

    if use_json {
        json::print_json(&ip)?;
    }

    Ok(())
}

/// Resolve network name or short ID to full UUID
pub async fn resolve_network(id_or_name: &str, client: &TypedClient) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let networks = response.into_inner();

    // Try short ID match first
    if id_or_name.len() >= 8 {
        for net in &networks {
            if net.id.to_string().starts_with(id_or_name) {
                return Ok(net.id.to_string());
            }
        }
    }

    // Try exact name match
    for net in &networks {
        if net.name == id_or_name {
            return Ok(net.id.to_string());
        }
    }

    Err(anyhow::anyhow!("Network not found: {}", id_or_name))
}
