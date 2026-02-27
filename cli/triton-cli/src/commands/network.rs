// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Network management commands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use crate::output::json::{self, print_json_stream};
use crate::output::table::{TableBuilder, TableFormatArgs};

#[derive(Args, Clone)]
pub struct NetworkListArgs {
    /// Filter by public network (true or false)
    #[arg(long, value_parser = parse_bool_filter)]
    pub public: Option<bool>,

    #[command(flatten)]
    pub table: TableFormatArgs,

    /// Filters in key=value format (e.g., public=true)
    ///
    /// Supported filter keys: public
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
}

/// Parse a boolean filter value ("true" or "false")
fn parse_bool_filter(s: &str) -> Result<bool, String> {
    match s.to_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value '{}'. Use 'true' or 'false'.",
            s
        )),
    }
}

#[derive(Subcommand, Clone)]
pub enum NetworkCommand {
    /// List networks
    #[command(visible_alias = "ls")]
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
    #[command(visible_alias = "rm")]
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
    #[command(visible_alias = "ls")]
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
    /// Validate arguments that don't require a client/profile.
    /// Call this before building the client so validation errors are
    /// reported even when no profile is configured.
    pub fn pre_validate(&self) -> Result<()> {
        if let Self::Create(args) = self
            && args.gateway.is_none()
            && !args.no_nat
        {
            anyhow::bail!("without a --gateway (-g), you must specify --no-nat (-x)");
        }
        Ok(())
    }

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

/// Valid filter keys for positional key=value arguments
const VALID_FILTERS: &[&str] = &["public"];

/// Check if a filter key is valid
fn is_valid_filter(key: &str) -> bool {
    VALID_FILTERS.contains(&key)
}

/// Apply positional key=value filters to the NetworkListArgs, merging with any
/// existing --flag values. Positional filters override flags if both are set.
fn apply_positional_filters(args: &mut NetworkListArgs) -> Result<()> {
    for filter in std::mem::take(&mut args.filters) {
        let (key, value) = filter
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid filter '{}': must be key=value", filter))?;

        if !is_valid_filter(key) {
            anyhow::bail!(
                "Unknown filter '{}'. Valid filters: {}",
                key,
                VALID_FILTERS.join(", ")
            );
        }

        match key {
            "public" => {
                args.public = Some(parse_bool_filter(value).map_err(|e| anyhow::anyhow!(e))?);
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

async fn list_networks(
    mut args: NetworkListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    apply_positional_filters(&mut args)?;
    let account = client.effective_account();
    let response = client
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let all_networks = response.into_inner();

    // Apply public filter if specified
    let mut networks: Vec<_> = if let Some(public_filter) = args.public {
        all_networks
            .into_iter()
            .filter(|net| net.public == public_filter)
            .collect()
    } else {
        all_networks
    };

    // Sort by public (true first), then by name to match node-triton behavior
    networks.sort_by(|a, b| {
        // Public networks come first (reverse bool comparison: true > false)
        match b.public.cmp(&a.public) {
            std::cmp::Ordering::Equal => a.name.cmp(&b.name),
            other => other,
        }
    });

    if use_json {
        print_json_stream(&networks)?;
    } else {
        // node-triton columns: SHORTID, NAME, SUBNET, GATEWAY, FABRIC, VLAN, PUBLIC
        let mut tbl = TableBuilder::new(&[
            "SHORTID", "NAME", "SUBNET", "GATEWAY", "FABRIC", "VLAN", "PUBLIC",
        ])
        .with_long_headers(&["ID"]);
        for net in &networks {
            tbl.add_row(vec![
                net.id.to_string()[..8].to_string(),
                net.name.clone(),
                net.subnet.clone().unwrap_or_else(|| "-".to_string()),
                net.gateway.clone().unwrap_or_else(|| "-".to_string()),
                // FABRIC: show true/false, or - if not present
                net.fabric
                    .map(|f| if f { "true" } else { "false" }.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                // VLAN: show ID or - if not a fabric network
                net.vlan_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                // PUBLIC: show true/false
                if net.public { "true" } else { "false" }.to_string(),
                net.id.to_string(),
            ]);
        }
        tbl.print(&args.table)?;
    }

    Ok(())
}

async fn get_network(args: NetworkGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let network_uuid = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .get_network()
        .account(account)
        .network(network_uuid)
        .send()
        .await?;

    let network = response.into_inner();

    if use_json {
        json::print_json(&network)?;
    } else {
        json::print_json_pretty(&network)?;
    }

    Ok(())
}

async fn get_default_network(client: &TypedClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
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
    let account = client.effective_account();
    let network_uuid = resolve_network(&args.network, client).await?;

    let request = cloudapi_client::types::UpdateConfigRequest {
        default_network: Some(network_uuid),
    };

    client
        .inner()
        .update_config()
        .account(account)
        .body(request)
        .send()
        .await?;

    println!("Default network set to {}", network_uuid);

    Ok(())
}

async fn create_network(
    args: NetworkCreateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();

    // Note: gateway/no-nat validation is in NetworkCommand::pre_validate()
    // which runs before client creation, so it works even without a profile.

    // Build resolvers from comma-separated or multiple flags (default to empty)
    let resolvers = Some(match args.resolver {
        Some(r) => r
            .iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_string())
            .collect(),
        None => vec![],
    });

    // Parse routes from SUBNET=IP format into a JSON object (default to empty)
    let routes = Some(match args.route {
        Some(route_list) => {
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
        }
        None => serde_json::Value::Object(serde_json::Map::new()),
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
    let network_id_str = network.id.to_string();

    println!(
        "Created network {} ({})",
        &network.name,
        &network_id_str[..8]
    );

    if use_json {
        json::print_json(&network)?;
    }

    Ok(())
}

async fn delete_network(args: NetworkDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = client.effective_account();

    // Resolve network and get its details
    let (network_id, network_name, vlan_id) = resolve_fabric_network(&args.network, client).await?;

    // Confirm unless forced
    if !args.force {
        use dialoguer::Confirm;
        if !Confirm::new()
            .with_prompt(format!(
                "Delete network '{}' ({})?",
                network_name,
                &network_id[..8]
            ))
            .default(false)
            .interact()?
        {
            return Ok(());
        }
    }

    let network_uuid: uuid::Uuid = network_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid network UUID: {}", network_id))?;

    client
        .inner()
        .delete_fabric_network()
        .account(account)
        .vlan_id(vlan_id)
        .id(network_uuid)
        .send()
        .await?;

    println!("Deleted network {} ({})", network_name, &network_id[..8]);

    Ok(())
}

/// Resolve network name or ID to (UUID string, name, vlan_id) for fabric networks
async fn resolve_fabric_network(
    id_or_name: &str,
    client: &TypedClient,
) -> Result<(String, String, u16)> {
    let account = client.effective_account();

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
            let net_id_str = net.id.to_string();
            // Match by UUID
            if net_id_str == id_or_name {
                return Ok((net_id_str, net.name.clone(), vlan.vlan_id));
            }
            // Match by short ID
            if id_or_name.len() >= 8 && net_id_str.starts_with(id_or_name) {
                return Ok((net_id_str, net.name.clone(), vlan.vlan_id));
            }
            // Match by name
            if net.name == id_or_name {
                return Ok((net_id_str, net.name.clone(), vlan.vlan_id));
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
    let account = client.effective_account();
    let network_uuid = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .list_network_ips()
        .account(account)
        .network(network_uuid)
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
        tbl.print(&args.table)?;
    }

    Ok(())
}

async fn get_network_ip(
    args: NetworkIpGetArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let network_uuid = resolve_network(&args.network, client).await?;

    let response = client
        .inner()
        .get_network_ip()
        .account(account)
        .network(network_uuid)
        .ip_address(&args.ip)
        .send()
        .await?;

    let ip = response.into_inner();

    if use_json {
        json::print_json(&ip)?;
    } else {
        json::print_json_pretty(&ip)?;
    }

    Ok(())
}

async fn update_network_ip(
    args: NetworkIpUpdateArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = client.effective_account();
    let network_uuid = resolve_network(&args.network, client).await?;

    // Parse update data from file or command line
    let reserved = if let Some(file_path) = &args.file {
        let content = if file_path.as_os_str() == "-" {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        } else {
            tokio::fs::read_to_string(file_path).await?
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
        .network(network_uuid)
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
pub async fn resolve_network(id_or_name: &str, client: &TypedClient) -> Result<uuid::Uuid> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        // If already a UUID, use it directly (matches node-triton's _stepNetId
        // which short-circuits on UUID input without a GET call)
        return Ok(uuid);
    }

    let account = client.effective_account();
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
                return Ok(net.id);
            }
        }
    }

    // Try exact name match
    for net in &networks {
        if net.name == id_or_name {
            return Ok(net.id);
        }
    }

    Err(crate::errors::ResourceNotFoundError(format!("Network not found: {}", id_or_name)).into())
}

/// Resolve network, always validating via GET even for UUIDs.
/// Matches node-triton's `getNetwork` which does a GET for UUIDs
/// (unlike `_stepNetId` which short-circuits).
pub async fn resolve_network_with_get(
    id_or_name: &str,
    client: &TypedClient,
) -> Result<uuid::Uuid> {
    let account = client.effective_account();

    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        // Validate the network exists via GET
        client
            .inner()
            .get_network()
            .account(account)
            .network(uuid.to_string())
            .send()
            .await?;
        return Ok(uuid);
    }

    // For names/short IDs, list and match
    resolve_network(id_or_name, client).await
}
