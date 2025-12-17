// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Network management commands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json;
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct NetworkListArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum NetworkCommand {
    /// List networks
    #[command(alias = "ls")]
    List(NetworkListArgs),
    /// Get network details
    Get(NetworkGetArgs),
    /// Get default network
    GetDefault,
    /// Set default network
    SetDefault(NetworkSetDefaultArgs),
    /// Create a fabric network
    Create(NetworkCreateArgs),
    /// Delete a fabric network
    #[command(alias = "rm")]
    Delete(NetworkDeleteArgs),
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

    #[command(flatten)]
    pub table: TableFormatArgs,
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
    /// Read update data from JSON file (use '-' for stdin)
    #[arg(short = 'f', long = "file")]
    pub file: Option<PathBuf>,
}

#[derive(Args, Clone)]
pub struct NetworkCreateArgs {
    /// VLAN ID for the fabric network (positional argument)
    pub vlan_id: u16,

    /// Network name
    #[arg(long, short = 'n')]
    pub name: String,

    /// Description
    #[arg(long, short = 'D')]
    pub description: Option<String>,

    /// Subnet in CIDR notation (e.g., 10.0.0.0/24)
    #[arg(long, short = 's')]
    pub subnet: String,

    /// First assignable IP address on the network
    #[arg(long = "start-ip", short = 'S', visible_alias = "provision-start")]
    pub start_ip: String,

    /// Last assignable IP address on the network
    #[arg(long = "end-ip", short = 'E', visible_alias = "provision-end")]
    pub end_ip: String,

    /// Default gateway IP address
    #[arg(long, short = 'g')]
    pub gateway: Option<String>,

    /// DNS resolver IP address (can be specified multiple times)
    #[arg(long, short = 'r')]
    pub resolver: Option<Vec<String>>,

    /// Static route in SUBNET=IP format (can be specified multiple times)
    #[arg(long, short = 'R')]
    pub route: Option<Vec<String>>,

    /// Disable internet NAT (no NAT zone on gateway)
    #[arg(long = "no-nat", short = 'x')]
    pub no_nat: bool,
}

#[derive(Args, Clone)]
pub struct NetworkDeleteArgs {
    /// Network ID or name
    pub network: String,

    /// Force deletion without confirmation
    #[arg(long, short)]
    pub force: bool,
}

impl NetworkCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_networks(args, client, use_json).await,
            Self::Get(args) => get_network(args, client, use_json).await,
            Self::GetDefault => get_default_network(client, use_json).await,
            Self::SetDefault(args) => set_default_network(args, client).await,
            Self::Create(args) => create_network(args, client, use_json).await,
            Self::Delete(args) => delete_network(args, client).await,
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

async fn list_networks(args: NetworkListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
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
        let mut tbl = TableBuilder::new(&["SHORTID", "NAME", "SUBNET", "GATEWAY", "PUBLIC"])
            .with_long_headers(&["ID", "FABRIC", "VLAN"]);
        for net in &networks {
            tbl.add_row(vec![
                net.id.to_string()[..8].to_string(),
                net.name.clone(),
                net.subnet.clone().unwrap_or_else(|| "-".to_string()),
                net.gateway.clone().unwrap_or_else(|| "-".to_string()),
                if net.public { "yes" } else { "no" }.to_string(),
                net.id.to_string(),
                net.fabric
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                net.vlan_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ]);
        }
        tbl.print(&args.table);
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

async fn create_network(
    args: NetworkCreateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;

    // Build resolvers from comma-separated or multiple flags
    let resolvers = args.resolver.map(|r| {
        r.iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_string())
            .collect()
    });

    // Parse routes from SUBNET=IP format into a JSON object
    let routes = args.route.map(|route_list| {
        let mut route_map = serde_json::Map::new();
        for route in route_list {
            if let Some((subnet, gateway)) = route.split_once('=') {
                route_map.insert(
                    subnet.to_string(),
                    serde_json::Value::String(gateway.to_string()),
                );
            }
        }
        serde_json::Value::Object(route_map)
    });

    let request = cloudapi_client::types::CreateFabricNetworkRequest {
        name: args.name.clone(),
        description: args.description,
        subnet: args.subnet,
        provision_start_ip: args.start_ip,
        provision_end_ip: args.end_ip,
        gateway: args.gateway,
        resolvers,
        routes,
        internet_nat: if args.no_nat { Some(false) } else { None },
    };

    let response = client
        .inner()
        .create_fabric_network()
        .account(account)
        .vlan_id(args.vlan_id)
        .body(request)
        .send()
        .await?;

    let network = response.into_inner();

    println!(
        "Created network {} ({})",
        &network.name,
        &network.id[..8.min(network.id.len())]
    );

    if use_json {
        json::print_json(&network)?;
    }

    Ok(())
}

async fn delete_network(args: NetworkDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    // Resolve network and get its details
    let (network_id, network_name, vlan_id) = resolve_fabric_network(&args.network, client).await?;

    // Confirm unless forced
    if !args.force {
        println!(
            "Are you sure you want to delete network '{}' ({})? [y/N]",
            network_name,
            &network_id[..8]
        );
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    client
        .inner()
        .delete_fabric_network()
        .account(account)
        .vlan_id(vlan_id)
        .id(&network_id)
        .send()
        .await?;

    println!("Deleted network {} ({})", network_name, &network_id[..8]);

    Ok(())
}

/// Resolve network name or ID to (UUID, name, vlan_id) for fabric networks
async fn resolve_fabric_network(
    id_or_name: &str,
    client: &TypedClient,
) -> Result<(String, String, u16)> {
    let account = &client.auth_config().account;

    // List all fabric VLANs first
    let vlans_response = client
        .inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await?;
    let vlans = vlans_response.into_inner();

    // Search through all VLANs for the network
    for vlan in &vlans {
        let networks_response = client
            .inner()
            .list_fabric_networks()
            .account(account)
            .vlan_id(vlan.vlan_id)
            .send()
            .await?;
        let networks = networks_response.into_inner();

        for net in &networks {
            // Match by UUID
            if net.id == id_or_name {
                return Ok((net.id.clone(), net.name.clone(), vlan.vlan_id));
            }
            // Match by short ID
            if id_or_name.len() >= 8 && net.id.starts_with(id_or_name) {
                return Ok((net.id.clone(), net.name.clone(), vlan.vlan_id));
            }
            // Match by name
            if net.name == id_or_name {
                return Ok((net.id.clone(), net.name.clone(), vlan.vlan_id));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Fabric network not found: {}. Note: only fabric networks can be deleted.",
        id_or_name
    ))
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
        let mut tbl = TableBuilder::new(&["IP", "RESERVED", "MANAGED", "OWNER"])
            .with_long_headers(&["OWNERID", "BELONGS_TO_TYPE"]);
        for ip in &ips {
            let reserved_str = if ip.reserved {
                "yes".to_string()
            } else {
                "no".to_string()
            };
            let managed_str = if ip.managed.unwrap_or(false) {
                "yes".to_string()
            } else {
                "no".to_string()
            };
            let owner_str = ip
                .owner_uuid
                .as_ref()
                .map(|u| u.to_string()[..8].to_string())
                .unwrap_or_else(|| "-".to_string());
            let owner_id = ip
                .owner_uuid
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_else(|| "-".to_string());
            let belongs_to_type = ip
                .belongs_to_type
                .clone()
                .unwrap_or_else(|| "-".to_string());
            tbl.add_row(vec![
                ip.ip.clone(),
                reserved_str,
                managed_str,
                owner_str,
                owner_id,
                belongs_to_type,
            ]);
        }
        tbl.print(&args.table);
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

    // Parse update data from file or command line
    let reserved = if let Some(file_path) = &args.file {
        let content = if file_path.as_os_str() == "-" {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            std::fs::read_to_string(file_path)?
        };
        let data: serde_json::Value = serde_json::from_str(&content)?;
        data.get("reserved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    } else {
        args.reserve.unwrap_or(false)
    };

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
