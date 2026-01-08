// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance NIC subcommands

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use dialoguer::Confirm;
use serde::{Deserialize, Serialize};

use crate::output::{json, table};

#[derive(Subcommand, Clone)]
pub enum NicCommand {
    /// List NICs on an instance
    #[command(visible_alias = "ls")]
    List(NicListArgs),

    /// Get NIC details
    Get(NicGetArgs),

    /// Add a NIC to an instance
    #[command(visible_alias = "create")]
    Add(NicAddArgs),

    /// Remove a NIC from an instance
    #[command(visible_aliases = ["rm", "delete"])]
    Remove(NicRemoveArgs),
}

#[derive(Args, Clone)]
pub struct NicListArgs {
    /// Instance ID or name
    pub instance: String,

    /// Filter by field (e.g., mac=XX:XX:XX:XX:XX:XX)
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
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

    /// Network ID, name, or NICOPTS (e.g., ipv4_uuid=UUID ipv4_ips=IP)
    #[arg(required = true, num_args = 1..)]
    pub network_or_opts: Vec<String>,

    /// Make the new NIC the primary NIC for the instance
    #[arg(short = 'p', long)]
    pub primary: bool,

    /// Wait for NIC addition to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "600")]
    pub wait_timeout: u64,
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
            Self::Add(args) => add_nic(args, client, use_json).await,
            Self::Remove(args) => remove_nic(args, client).await,
        }
    }
}

/// Output struct for NIC list (matches node-triton JSON output)
#[derive(Debug, Serialize, Deserialize)]
struct NicOutput {
    ip: String,
    mac: String,
    primary: bool,
    netmask: String,
    gateway: String,
    state: String,
    network: String,
}

impl From<&cloudapi_client::types::Nic> for NicOutput {
    fn from(nic: &cloudapi_client::types::Nic) -> Self {
        NicOutput {
            ip: nic.ip.clone(),
            mac: nic.mac.clone(),
            primary: nic.primary,
            netmask: nic.netmask.clone(),
            gateway: nic.gateway.clone().unwrap_or_default(),
            state: nic.state.clone().unwrap_or_default(),
            network: nic.network.to_string(),
        }
    }
}

/// Convert netmask to CIDR notation
fn netmask_to_cidr(netmask: &str) -> u8 {
    netmask
        .split('.')
        .filter_map(|octet| octet.parse::<u8>().ok())
        .map(|byte| byte.count_ones() as u8)
        .sum()
}

pub async fn list_nics(args: NicListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_nics()
        .account(account)
        .machine(machine_id)
        .send()
        .await?;

    let mut nics: Vec<NicOutput> = response.into_inner().iter().map(NicOutput::from).collect();

    // Apply filters
    for filter in &args.filters {
        if let Some((key, value)) = filter.split_once('=') {
            nics.retain(|nic| match key {
                "ip" => nic.ip == value || nic.ip.starts_with(&format!("{}/", value)),
                "mac" => nic.mac == value,
                "state" => nic.state == value,
                "network" => nic.network == value || nic.network.starts_with(value),
                "primary" => (value == "true" && nic.primary) || (value == "false" && !nic.primary),
                "gateway" => nic.gateway == value,
                _ => true,
            });
        }
    }

    if use_json {
        // node-triton outputs NDJSON (one JSON object per line)
        for nic in &nics {
            println!("{}", serde_json::to_string(nic)?);
        }
    } else {
        // node-triton columns: IP MAC STATE NETWORK
        // IP is formatted as IP/CIDR
        let mut tbl = table::create_table(&["IP", "MAC", "STATE", "NETWORK"]);
        for nic in &nics {
            let cidr = netmask_to_cidr(&nic.netmask);
            let ip_cidr = format!("{}/{}", nic.ip, cidr);
            // Network is truncated to short ID in table view
            let short_network = nic.network.split('-').next().unwrap_or(&nic.network);
            tbl.add_row(vec![&ip_cidr, &nic.mac, &nic.state, short_network]);
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
        .machine(machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    let nic = response.into_inner();

    // node-triton outputs JSON by default for nic get
    // We always output JSON regardless of the -j flag to match node-triton behavior
    let _ = use_json; // Acknowledge but ignore the flag
    json::print_json(&NicOutput::from(&nic))?;

    Ok(())
}

/// Parse NICOPTS from arguments (e.g., ipv4_uuid=UUID ipv4_ips=IP)
fn parse_nic_opts(args: &[String]) -> Result<(String, Option<String>)> {
    let mut ipv4_uuid = None;
    let mut ipv4_ips = None;

    for arg in args {
        if let Some((key, value)) = arg.split_once('=') {
            match key {
                "ipv4_uuid" => ipv4_uuid = Some(value.to_string()),
                "ipv4_ips" => ipv4_ips = Some(value.to_string()),
                _ => return Err(anyhow!("unknown NIC option: {}", key)),
            }
        } else {
            return Err(anyhow!("invalid NIC option format: {}", arg));
        }
    }

    ipv4_uuid
        .ok_or_else(|| anyhow!("ipv4_uuid is required when using NICOPTS"))
        .map(|uuid| (uuid, ipv4_ips))
}

async fn add_nic(args: NicAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Determine if we have NICOPTS or a simple network ID
    let has_opts = args
        .network_or_opts
        .iter()
        .any(|arg| arg.contains('=') && arg.contains("ipv4_"));

    let network_str = if has_opts {
        // Parse NICOPTS
        let (uuid, _ips) = parse_nic_opts(&args.network_or_opts)?;
        // Note: CloudAPI AddNicRequest only supports 'network' field
        // ipv4_ips would need to be handled differently if supported
        uuid
    } else {
        // Simple network ID (first positional arg)
        args.network_or_opts
            .first()
            .ok_or_else(|| anyhow!("missing NETWORK argument"))?
            .clone()
    };

    let network: uuid::Uuid = network_str.parse()?;

    let request = cloudapi_client::types::AddNicRequest {
        network,
        primary: if args.primary { Some(true) } else { None },
    };

    let response = client
        .inner()
        .add_nic()
        .account(account)
        .machine(machine_id)
        .body(request)
        .send()
        .await?;

    let nic = response.into_inner();

    if args.wait && !use_json {
        println!("Creating NIC {}", nic.mac);
    }

    if args.wait {
        super::wait::wait_for_state(machine_id, "running", args.wait_timeout, client).await?;
    }

    if use_json {
        // Output full NIC JSON
        json::print_json(&NicOutput::from(&nic))?;
    } else {
        println!("Created NIC {}", nic.mac);
    }

    Ok(())
}

async fn remove_nic(args: NicRemoveArgs, client: &TypedClient) -> Result<()> {
    if !args.force
        && !Confirm::new()
            .with_prompt(format!("Delete NIC \"{}\"?", args.mac))
            .default(false)
            .interact()?
    {
        eprintln!("Aborting");
        return Ok(());
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    client
        .inner()
        .remove_nic()
        .account(account)
        .machine(machine_id)
        .mac(&args.mac)
        .send()
        .await?;

    // Match node-triton output exactly
    println!("Deleted NIC {}", args.mac);

    Ok(())
}
