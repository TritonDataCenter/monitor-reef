// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! NAPI CLI - Command-line interface for Triton NAPI (Networking API)
//!
//! This CLI provides access to all NAPI endpoints for managing NICs,
//! NIC tags, networks, network pools, IPs, aggregations, fabric VLANs,
//! and fabric networks.
//!
//! # Environment Variables
//!
//! - `NAPI_URL` - NAPI base URL (default: http://localhost)

use anyhow::Result;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use napi_client::Client;
use napi_client::types;

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

#[derive(Parser)]
#[command(name = "napi", version, about = "CLI for Triton NAPI (Networking API)")]
struct Cli {
    /// NAPI base URL
    #[arg(long, env = "NAPI_URL", default_value = "http://localhost")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Health check endpoint
    Ping,

    /// Manage NICs
    #[command(subcommand)]
    Nic(NicCommands),

    /// Manage NIC tags
    #[command(name = "nic-tag", subcommand)]
    NicTag(NicTagCommands),

    /// Manage networks
    #[command(subcommand)]
    Network(NetworkCommands),

    /// Manage network pools
    #[command(subcommand)]
    Pool(PoolCommands),

    /// Manage network IPs
    #[command(subcommand)]
    Ip(IpCommands),

    /// Manage link aggregations
    #[command(subcommand)]
    Aggregation(AggregationCommands),

    /// Manage fabric VLANs
    #[command(name = "fabric-vlan", subcommand)]
    FabricVlan(FabricVlanCommands),

    /// Manage fabric networks
    #[command(name = "fabric-network", subcommand)]
    FabricNetwork(FabricNetworkCommands),

    /// Search for IPs across all networks
    #[command(name = "search-ips")]
    SearchIps {
        /// IP address to search for (required)
        ip: String,
        /// Filter by belongs_to_type
        #[arg(long)]
        belongs_to_type: Option<String>,
        /// Filter by belongs_to_uuid
        #[arg(long)]
        belongs_to_uuid: Option<String>,
        /// Filter by fabric
        #[arg(long)]
        fabric: Option<bool>,
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Run garbage collection
    Gc {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },
}

// ============================================================================
// NIC subcommands
// ============================================================================

#[derive(Subcommand)]
enum NicCommands {
    /// List NICs
    List {
        /// Filter by belongs_to_type
        #[arg(long)]
        belongs_to_type: Option<String>,
        /// Filter by belongs_to_uuid
        #[arg(long)]
        belongs_to_uuid: Option<String>,
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<String>,
        /// Filter by CN UUID
        #[arg(long)]
        cn_uuid: Option<String>,
        /// Filter by network UUID
        #[arg(long)]
        network_uuid: Option<String>,
        /// Filter by NIC tag
        #[arg(long)]
        nic_tag: Option<String>,
        /// Filter by NIC tags provided
        #[arg(long)]
        nic_tags_provided: Option<String>,
        /// Filter by state
        #[arg(long)]
        state: Option<String>,
        /// Filter by underlay
        #[arg(long)]
        underlay: Option<bool>,
        /// Filter by allow_dhcp_spoofing
        #[arg(long)]
        allow_dhcp_spoofing: Option<bool>,
        /// Filter by allow_ip_spoofing
        #[arg(long)]
        allow_ip_spoofing: Option<bool>,
        /// Filter by allow_mac_spoofing
        #[arg(long)]
        allow_mac_spoofing: Option<bool>,
        /// Filter by allow_restricted_traffic
        #[arg(long)]
        allow_restricted_traffic: Option<bool>,
        /// Filter by allow_unfiltered_promisc
        #[arg(long)]
        allow_unfiltered_promisc: Option<bool>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a NIC by MAC address
    Get {
        /// MAC address (colon-separated, dash-separated, or bare hex)
        mac: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a NIC
    Create {
        /// UUID of the object this NIC belongs to
        #[arg(long)]
        belongs_to_uuid: Uuid,
        /// Type of the object this NIC belongs to
        #[arg(long, value_enum)]
        belongs_to_type: types::BelongsToType,
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// MAC address (auto-generated if omitted)
        #[arg(long)]
        mac: Option<String>,
        /// IP address
        #[arg(long)]
        ip: Option<String>,
        /// Network UUID
        #[arg(long)]
        network_uuid: Option<Uuid>,
        /// NIC tag
        #[arg(long)]
        nic_tag: Option<String>,
        /// CN UUID
        #[arg(long)]
        cn_uuid: Option<Uuid>,
        /// NIC model
        #[arg(long)]
        model: Option<String>,
        /// VLAN ID
        #[arg(long)]
        vlan_id: Option<u32>,
        /// NIC state
        #[arg(long, value_enum)]
        state: Option<types::NicState>,
        /// Set as primary NIC
        #[arg(long)]
        primary: Option<bool>,
        /// Reserved
        #[arg(long)]
        reserved: Option<bool>,
        /// Underlay NIC
        #[arg(long)]
        underlay: Option<bool>,
        /// Allow DHCP spoofing
        #[arg(long)]
        allow_dhcp_spoofing: Option<bool>,
        /// Allow IP spoofing
        #[arg(long)]
        allow_ip_spoofing: Option<bool>,
        /// Allow MAC spoofing
        #[arg(long)]
        allow_mac_spoofing: Option<bool>,
        /// Allow restricted traffic
        #[arg(long)]
        allow_restricted_traffic: Option<bool>,
        /// Allow unfiltered promiscuous
        #[arg(long)]
        allow_unfiltered_promisc: Option<bool>,
        /// Check owner
        #[arg(long)]
        check_owner: Option<bool>,
        /// NIC tags available (comma-separated)
        #[arg(long, value_delimiter = ',')]
        nic_tags_available: Option<Vec<String>>,
        /// NIC tags provided (comma-separated)
        #[arg(long, value_delimiter = ',')]
        nic_tags_provided: Option<Vec<String>>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a NIC
    Update {
        /// MAC address of the NIC to update
        mac: String,
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Belongs to UUID
        #[arg(long)]
        belongs_to_uuid: Option<Uuid>,
        /// Belongs to type
        #[arg(long, value_enum)]
        belongs_to_type: Option<types::BelongsToType>,
        /// NIC state
        #[arg(long, value_enum)]
        state: Option<types::NicState>,
        /// NIC tag
        #[arg(long)]
        nic_tag: Option<String>,
        /// NIC model
        #[arg(long)]
        model: Option<String>,
        /// Network UUID
        #[arg(long)]
        network_uuid: Option<Uuid>,
        /// CN UUID
        #[arg(long)]
        cn_uuid: Option<Uuid>,
        /// VLAN ID
        #[arg(long)]
        vlan_id: Option<u32>,
        /// Set as primary NIC
        #[arg(long)]
        primary: Option<bool>,
        /// Allow DHCP spoofing
        #[arg(long)]
        allow_dhcp_spoofing: Option<bool>,
        /// Allow IP spoofing
        #[arg(long)]
        allow_ip_spoofing: Option<bool>,
        /// Allow MAC spoofing
        #[arg(long)]
        allow_mac_spoofing: Option<bool>,
        /// Allow restricted traffic
        #[arg(long)]
        allow_restricted_traffic: Option<bool>,
        /// Allow unfiltered promiscuous
        #[arg(long)]
        allow_unfiltered_promisc: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a NIC
    Delete {
        /// MAC address of the NIC to delete
        mac: String,
    },

    /// Provision a NIC on a specific network
    #[command(name = "create-on-network")]
    CreateOnNetwork {
        /// Network UUID
        #[arg(long)]
        network_uuid: Uuid,
        /// UUID of the object this NIC belongs to
        #[arg(long)]
        belongs_to_uuid: Uuid,
        /// Type of the object this NIC belongs to
        #[arg(long, value_enum)]
        belongs_to_type: types::BelongsToType,
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// MAC address (auto-generated if omitted)
        #[arg(long)]
        mac: Option<String>,
        /// IP address
        #[arg(long)]
        ip: Option<String>,
        /// CN UUID
        #[arg(long)]
        cn_uuid: Option<Uuid>,
        /// Check owner
        #[arg(long)]
        check_owner: Option<bool>,
        /// Reserved
        #[arg(long)]
        reserved: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },
}

// ============================================================================
// NIC Tag subcommands
// ============================================================================

#[derive(Subcommand)]
enum NicTagCommands {
    /// List NIC tags
    List {
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a NIC tag by name
    Get {
        /// NIC tag name
        name: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a NIC tag
    Create {
        /// NIC tag name
        #[arg(long)]
        name: String,
        /// UUID (auto-generated if omitted)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// MTU value
        #[arg(long)]
        mtu: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a NIC tag
    Update {
        /// Current NIC tag name
        name: String,
        /// New NIC tag name
        #[arg(long)]
        new_name: Option<String>,
        /// New MTU value
        #[arg(long)]
        mtu: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a NIC tag
    Delete {
        /// NIC tag name
        name: String,
    },
}

// ============================================================================
// Network subcommands
// ============================================================================

#[derive(Subcommand)]
enum NetworkCommands {
    /// List networks
    List {
        /// Filter by UUID prefix
        #[arg(long)]
        uuid: Option<String>,
        /// Filter by fabric
        #[arg(long)]
        fabric: Option<bool>,
        /// Filter by address family
        #[arg(long, value_enum)]
        family: Option<types::NetworkFamily>,
        /// Filter by name
        #[arg(long)]
        name: Option<String>,
        /// Filter by NIC tag
        #[arg(long)]
        nic_tag: Option<String>,
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<String>,
        /// Filter by provisionable_by UUID
        #[arg(long)]
        provisionable_by: Option<String>,
        /// Filter by VLAN ID
        #[arg(long)]
        vlan_id: Option<u32>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a network by UUID (or "admin" for the admin network)
    Get {
        /// Network UUID or "admin"
        uuid: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a network
    Create {
        /// Network name
        #[arg(long)]
        name: String,
        /// NIC tag
        #[arg(long)]
        nic_tag: String,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Subnet (CIDR notation)
        #[arg(long)]
        subnet: Option<String>,
        /// Provision start IP
        #[arg(long)]
        provision_start_ip: Option<String>,
        /// Provision end IP
        #[arg(long)]
        provision_end_ip: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Gateway IP
        #[arg(long)]
        gateway: Option<String>,
        /// Resolvers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        resolvers: Option<Vec<String>>,
        /// Address family
        #[arg(long, value_enum)]
        family: Option<types::NetworkFamily>,
        /// MTU
        #[arg(long)]
        mtu: Option<u32>,
        /// UUID (auto-generated if omitted)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// Mark as fabric network
        #[arg(long)]
        fabric: Option<bool>,
        /// Use subnet allocation
        #[arg(long)]
        subnet_alloc: Option<bool>,
        /// Subnet prefix length (for subnet_alloc)
        #[arg(long)]
        subnet_prefix: Option<u32>,
        /// Enable internet NAT
        #[arg(long)]
        internet_nat: Option<bool>,
        /// Owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// VNET ID
        #[arg(long)]
        vnet_id: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a network
    Update {
        /// Network UUID
        uuid: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// New gateway
        #[arg(long)]
        gateway: Option<String>,
        /// New resolvers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        resolvers: Option<Vec<String>>,
        /// New provision start IP
        #[arg(long)]
        provision_start_ip: Option<String>,
        /// New provision end IP
        #[arg(long)]
        provision_end_ip: Option<String>,
        /// Owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a network
    Delete {
        /// Network UUID
        uuid: String,
    },
}

// ============================================================================
// Network Pool subcommands
// ============================================================================

#[derive(Subcommand)]
enum PoolCommands {
    /// List network pools
    List {
        /// Filter by UUID prefix
        #[arg(long)]
        uuid: Option<String>,
        /// Filter by name
        #[arg(long)]
        name: Option<String>,
        /// Filter by address family
        #[arg(long, value_enum)]
        family: Option<types::NetworkFamily>,
        /// Filter by network UUID
        #[arg(long)]
        networks: Option<String>,
        /// Filter by provisionable_by UUID
        #[arg(long)]
        provisionable_by: Option<String>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a network pool by UUID
    Get {
        /// Pool UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a network pool
    Create {
        /// Pool name
        #[arg(long)]
        name: String,
        /// Network UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        networks: Vec<Uuid>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// UUID (auto-generated if omitted)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a network pool
    Update {
        /// Pool UUID
        uuid: Uuid,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New network UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        networks: Option<Vec<Uuid>>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// New owner UUIDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owner_uuids: Option<Vec<Uuid>>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a network pool
    Delete {
        /// Pool UUID
        uuid: Uuid,
    },
}

// ============================================================================
// IP subcommands
// ============================================================================

#[derive(Subcommand)]
enum IpCommands {
    /// List IPs on a network
    List {
        /// Network UUID
        #[arg(long)]
        network_uuid: Uuid,
        /// Filter by belongs_to_type
        #[arg(long)]
        belongs_to_type: Option<String>,
        /// Filter by belongs_to_uuid
        #[arg(long)]
        belongs_to_uuid: Option<String>,
        /// Filter by owner UUID
        #[arg(long)]
        owner_uuid: Option<String>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get an IP on a network
    Get {
        /// Network UUID
        #[arg(long)]
        network_uuid: Uuid,
        /// IP address
        ip_addr: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update an IP on a network
    Update {
        /// Network UUID
        #[arg(long)]
        network_uuid: Uuid,
        /// IP address
        ip_addr: String,
        /// Belongs to type
        #[arg(long)]
        belongs_to_type: Option<String>,
        /// Belongs to UUID
        #[arg(long)]
        belongs_to_uuid: Option<Uuid>,
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Option<Uuid>,
        /// Reserved
        #[arg(long)]
        reserved: Option<bool>,
        /// Free (setting to true deletes the IP assignment)
        #[arg(long)]
        free: Option<bool>,
        /// Unassign (mutually exclusive with free)
        #[arg(long)]
        unassign: Option<bool>,
        /// Check owner
        #[arg(long)]
        check_owner: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },
}

// ============================================================================
// Aggregation subcommands
// ============================================================================

#[derive(Subcommand)]
enum AggregationCommands {
    /// List aggregations
    List {
        /// Filter by belongs_to_uuid
        #[arg(long)]
        belongs_to_uuid: Option<String>,
        /// Filter by MACs
        #[arg(long)]
        macs: Option<String>,
        /// Filter by NIC tags provided
        #[arg(long)]
        nic_tags_provided: Option<String>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get an aggregation by ID
    Get {
        /// Aggregation ID (format: {belongs_to_uuid}-{name})
        id: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create an aggregation
    Create {
        /// Aggregation name
        #[arg(long)]
        name: String,
        /// MAC addresses (comma-separated, colon-separated format)
        #[arg(long, value_delimiter = ',')]
        macs: Vec<String>,
        /// LACP mode
        #[arg(long, value_enum)]
        lacp_mode: Option<types::LacpMode>,
        /// NIC tags provided (comma-separated)
        #[arg(long, value_delimiter = ',')]
        nic_tags_provided: Option<Vec<String>>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update an aggregation
    Update {
        /// Aggregation ID (format: {belongs_to_uuid}-{name})
        id: String,
        /// New LACP mode
        #[arg(long, value_enum)]
        lacp_mode: Option<types::LacpMode>,
        /// New NIC tags provided (comma-separated)
        #[arg(long, value_delimiter = ',')]
        nic_tags_provided: Option<Vec<String>>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete an aggregation
    Delete {
        /// Aggregation ID (format: {belongs_to_uuid}-{name})
        id: String,
    },
}

// ============================================================================
// Fabric VLAN subcommands
// ============================================================================

#[derive(Subcommand)]
enum FabricVlanCommands {
    /// List fabric VLANs for an owner
    List {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// Field selection filter
        #[arg(long)]
        fields: Option<String>,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a fabric VLAN
    Get {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        vlan_id: u32,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a fabric VLAN
    Create {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// VLAN name
        #[arg(long)]
        name: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a fabric VLAN
    Update {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        vlan_id: u32,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a fabric VLAN
    Delete {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        vlan_id: u32,
    },
}

// ============================================================================
// Fabric Network subcommands
// ============================================================================

#[derive(Subcommand)]
enum FabricNetworkCommands {
    /// List fabric networks on a VLAN
    List {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Limit results
        #[arg(long)]
        limit: Option<u32>,
        /// Offset into results
        #[arg(long)]
        offset: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Get a fabric network
    Get {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Network UUID
        uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Create a fabric network on a VLAN
    Create {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Network name
        #[arg(long)]
        name: String,
        /// Subnet (CIDR notation)
        #[arg(long)]
        subnet: String,
        /// Provision start IP
        #[arg(long)]
        provision_start_ip: String,
        /// Provision end IP
        #[arg(long)]
        provision_end_ip: String,
        /// Gateway IP
        #[arg(long)]
        gateway: Option<String>,
        /// Resolvers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        resolvers: Option<Vec<String>>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Enable internet NAT
        #[arg(long)]
        internet_nat: Option<bool>,
        /// UUID (auto-generated if omitted)
        #[arg(long)]
        uuid: Option<Uuid>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Update a fabric network
    Update {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Network UUID
        uuid: Uuid,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// New gateway
        #[arg(long)]
        gateway: Option<String>,
        /// New resolvers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        resolvers: Option<Vec<String>>,
        /// New provision start IP
        #[arg(long)]
        provision_start_ip: Option<String>,
        /// New provision end IP
        #[arg(long)]
        provision_end_ip: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Delete a fabric network
    Delete {
        /// Owner UUID
        #[arg(long)]
        owner_uuid: Uuid,
        /// VLAN ID
        #[arg(long)]
        vlan_id: u32,
        /// Network UUID
        uuid: Uuid,
    },
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        Commands::Ping => {
            let resp = client
                .ping()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Ping failed: {}", e))?;
            let ping = resp.into_inner();
            println!("{}", serde_json::to_string_pretty(&ping)?);
        }

        Commands::Nic(cmd) => handle_nic(&client, cmd).await?,
        Commands::NicTag(cmd) => handle_nic_tag(&client, cmd).await?,
        Commands::Network(cmd) => handle_network(&client, cmd).await?,
        Commands::Pool(cmd) => handle_pool(&client, cmd).await?,
        Commands::Ip(cmd) => handle_ip(&client, cmd).await?,
        Commands::Aggregation(cmd) => handle_aggregation(&client, cmd).await?,
        Commands::FabricVlan(cmd) => handle_fabric_vlan(&client, cmd).await?,
        Commands::FabricNetwork(cmd) => handle_fabric_network(&client, cmd).await?,

        Commands::SearchIps {
            ip,
            belongs_to_type,
            belongs_to_uuid,
            fabric,
            owner_uuid,
            raw,
        } => {
            let mut req = client.search_ips().ip(ip);
            if let Some(v) = belongs_to_type {
                req = req.belongs_to_type(v);
            }
            if let Some(v) = belongs_to_uuid {
                req = req.belongs_to_uuid(v);
            }
            if let Some(v) = fabric {
                req = req.fabric(v);
            }
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Search IPs failed: {}", e))?;
            let ips = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&ips)?);
            } else {
                for ip_rec in &ips {
                    print_ip_summary(ip_rec);
                }
                println!("({} IPs)", ips.len());
            }
        }

        Commands::Gc { raw } => {
            let resp = client
                .run_gc()
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("GC failed: {}", e))?;
            let gc = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&gc)?);
            } else {
                println!("GC completed");
                println!(
                    "  Start: rss={}, heap_total={}, heap_used={}",
                    gc.start.rss, gc.start.heap_total, gc.start.heap_used
                );
                println!(
                    "  End:   rss={}, heap_total={}, heap_used={}",
                    gc.end.rss, gc.end.heap_total, gc.end.heap_used
                );
            }
        }
    }

    Ok(())
}

// ============================================================================
// NIC handler
// ============================================================================

async fn handle_nic(client: &Client, cmd: NicCommands) -> Result<()> {
    match cmd {
        NicCommands::List {
            belongs_to_type,
            belongs_to_uuid,
            owner_uuid,
            cn_uuid,
            network_uuid,
            nic_tag,
            nic_tags_provided,
            state,
            underlay,
            allow_dhcp_spoofing,
            allow_ip_spoofing,
            allow_mac_spoofing,
            allow_restricted_traffic,
            allow_unfiltered_promisc,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_nics();
            if let Some(v) = belongs_to_type {
                req = req.belongs_to_type(v);
            }
            if let Some(v) = belongs_to_uuid {
                req = req.belongs_to_uuid(v);
            }
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            if let Some(v) = cn_uuid {
                req = req.cn_uuid(v);
            }
            if let Some(v) = network_uuid {
                req = req.network_uuid(v);
            }
            if let Some(v) = nic_tag {
                req = req.nic_tag(v);
            }
            if let Some(v) = nic_tags_provided {
                req = req.nic_tags_provided(v);
            }
            if let Some(v) = state {
                req = req.state(v);
            }
            if let Some(v) = underlay {
                req = req.underlay(v);
            }
            if let Some(v) = allow_dhcp_spoofing {
                req = req.allow_dhcp_spoofing(v);
            }
            if let Some(v) = allow_ip_spoofing {
                req = req.allow_ip_spoofing(v);
            }
            if let Some(v) = allow_mac_spoofing {
                req = req.allow_mac_spoofing(v);
            }
            if let Some(v) = allow_restricted_traffic {
                req = req.allow_restricted_traffic(v);
            }
            if let Some(v) = allow_unfiltered_promisc {
                req = req.allow_unfiltered_promisc(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List NICs failed: {}", e))?;
            let nics = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nics)?);
            } else {
                for nic in &nics {
                    print_nic_summary(nic);
                }
                println!("({} NICs)", nics.len());
            }
        }

        NicCommands::Get { mac, raw } => {
            let resp = client
                .get_nic()
                .mac(mac)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get NIC failed: {}", e))?;
            let nic = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nic)?);
            } else {
                print_nic_detail(&nic);
            }
        }

        NicCommands::Create {
            belongs_to_uuid,
            belongs_to_type,
            owner_uuid,
            mac,
            ip,
            network_uuid,
            nic_tag,
            cn_uuid,
            model,
            vlan_id,
            state,
            primary,
            reserved,
            underlay,
            allow_dhcp_spoofing,
            allow_ip_spoofing,
            allow_mac_spoofing,
            allow_restricted_traffic,
            allow_unfiltered_promisc,
            check_owner,
            nic_tags_available,
            nic_tags_provided,
            raw,
        } => {
            let builder = client.create_nic().body_map(|b| {
                let mut b = b
                    .belongs_to_uuid(belongs_to_uuid)
                    .belongs_to_type(belongs_to_type)
                    .owner_uuid(owner_uuid);
                if let Some(v) = mac {
                    b = b.mac(v);
                }
                if let Some(v) = ip {
                    b = b.ip(v);
                }
                if let Some(v) = network_uuid {
                    b = b.network_uuid(v);
                }
                if let Some(v) = nic_tag {
                    b = b.nic_tag(v);
                }
                if let Some(v) = cn_uuid {
                    b = b.cn_uuid(v);
                }
                if let Some(v) = model {
                    b = b.model(v);
                }
                if let Some(v) = vlan_id {
                    b = b.vlan_id(v);
                }
                if let Some(v) = state {
                    b = b.state(v);
                }
                if let Some(v) = primary {
                    b = b.primary(v);
                }
                if let Some(v) = reserved {
                    b = b.reserved(v);
                }
                if let Some(v) = underlay {
                    b = b.underlay(v);
                }
                if let Some(v) = allow_dhcp_spoofing {
                    b = b.allow_dhcp_spoofing(v);
                }
                if let Some(v) = allow_ip_spoofing {
                    b = b.allow_ip_spoofing(v);
                }
                if let Some(v) = allow_mac_spoofing {
                    b = b.allow_mac_spoofing(v);
                }
                if let Some(v) = allow_restricted_traffic {
                    b = b.allow_restricted_traffic(v);
                }
                if let Some(v) = allow_unfiltered_promisc {
                    b = b.allow_unfiltered_promisc(v);
                }
                if let Some(v) = check_owner {
                    b = b.check_owner(v);
                }
                if let Some(v) = nic_tags_available {
                    b = b.nic_tags_available(v);
                }
                if let Some(v) = nic_tags_provided {
                    b = b.nic_tags_provided(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create NIC failed: {}", e))?;
            let nic = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nic)?);
            } else {
                print_nic_detail(&nic);
            }
        }

        NicCommands::Update {
            mac,
            owner_uuid,
            belongs_to_uuid,
            belongs_to_type,
            state,
            nic_tag,
            model,
            network_uuid,
            cn_uuid,
            vlan_id,
            primary,
            allow_dhcp_spoofing,
            allow_ip_spoofing,
            allow_mac_spoofing,
            allow_restricted_traffic,
            allow_unfiltered_promisc,
            raw,
        } => {
            let builder = client.update_nic().mac(mac).body_map(|b| {
                let mut b = b;
                if let Some(v) = owner_uuid {
                    b = b.owner_uuid(v);
                }
                if let Some(v) = belongs_to_uuid {
                    b = b.belongs_to_uuid(v);
                }
                if let Some(v) = belongs_to_type {
                    b = b.belongs_to_type(v);
                }
                if let Some(v) = state {
                    b = b.state(v);
                }
                if let Some(v) = nic_tag {
                    b = b.nic_tag(v);
                }
                if let Some(v) = model {
                    b = b.model(v);
                }
                if let Some(v) = network_uuid {
                    b = b.network_uuid(v);
                }
                if let Some(v) = cn_uuid {
                    b = b.cn_uuid(v);
                }
                if let Some(v) = vlan_id {
                    b = b.vlan_id(v);
                }
                if let Some(v) = primary {
                    b = b.primary(v);
                }
                if let Some(v) = allow_dhcp_spoofing {
                    b = b.allow_dhcp_spoofing(v);
                }
                if let Some(v) = allow_ip_spoofing {
                    b = b.allow_ip_spoofing(v);
                }
                if let Some(v) = allow_mac_spoofing {
                    b = b.allow_mac_spoofing(v);
                }
                if let Some(v) = allow_restricted_traffic {
                    b = b.allow_restricted_traffic(v);
                }
                if let Some(v) = allow_unfiltered_promisc {
                    b = b.allow_unfiltered_promisc(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update NIC failed: {}", e))?;
            let nic = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nic)?);
            } else {
                print_nic_detail(&nic);
            }
        }

        NicCommands::Delete { mac } => {
            client
                .delete_nic()
                .mac(&mac)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete NIC failed: {}", e))?;
            println!("NIC {} deleted", mac);
        }

        NicCommands::CreateOnNetwork {
            network_uuid,
            belongs_to_uuid,
            belongs_to_type,
            owner_uuid,
            mac,
            ip,
            cn_uuid,
            check_owner,
            reserved,
            raw,
        } => {
            let builder = client
                .create_network_nic()
                .uuid(network_uuid)
                .body_map(|b| {
                    let mut b = b
                        .belongs_to_uuid(belongs_to_uuid)
                        .belongs_to_type(belongs_to_type)
                        .owner_uuid(owner_uuid);
                    if let Some(v) = mac {
                        b = b.mac(v);
                    }
                    if let Some(v) = ip {
                        b = b.ip(v);
                    }
                    if let Some(v) = cn_uuid {
                        b = b.cn_uuid(v);
                    }
                    if let Some(v) = check_owner {
                        b = b.check_owner(v);
                    }
                    if let Some(v) = reserved {
                        b = b.reserved(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create network NIC failed: {}", e))?;
            let nic = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&nic)?);
            } else {
                print_nic_detail(&nic);
            }
        }
    }
    Ok(())
}

// ============================================================================
// NIC Tag handler
// ============================================================================

async fn handle_nic_tag(client: &Client, cmd: NicTagCommands) -> Result<()> {
    match cmd {
        NicTagCommands::List { limit, offset, raw } => {
            let mut req = client.list_nic_tags();
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List NIC tags failed: {}", e))?;
            let tags = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&tags)?);
            } else {
                for tag in &tags {
                    println!("{} {} mtu={}", tag.uuid, tag.name, tag.mtu);
                }
                println!("({} NIC tags)", tags.len());
            }
        }

        NicTagCommands::Get { name, raw } => {
            let resp = client
                .get_nic_tag()
                .name(name)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get NIC tag failed: {}", e))?;
            let tag = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&tag)?);
            } else {
                print_nic_tag_detail(&tag);
            }
        }

        NicTagCommands::Create {
            name,
            uuid,
            mtu,
            raw,
        } => {
            let builder = client.create_nic_tag().body_map(|b| {
                let mut b = b.name(name);
                if let Some(v) = uuid {
                    b = b.uuid(v);
                }
                if let Some(v) = mtu {
                    b = b.mtu(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create NIC tag failed: {}", e))?;
            let tag = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&tag)?);
            } else {
                print_nic_tag_detail(&tag);
            }
        }

        NicTagCommands::Update {
            name,
            new_name,
            mtu,
            raw,
        } => {
            let builder = client.update_nic_tag().name(&name).body_map(|b| {
                let mut b = b;
                if let Some(v) = new_name {
                    b = b.name(v);
                }
                if let Some(v) = mtu {
                    b = b.mtu(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update NIC tag failed: {}", e))?;
            let tag = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&tag)?);
            } else {
                print_nic_tag_detail(&tag);
            }
        }

        NicTagCommands::Delete { name } => {
            client
                .delete_nic_tag()
                .name(&name)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete NIC tag failed: {}", e))?;
            println!("NIC tag {} deleted", name);
        }
    }
    Ok(())
}

// ============================================================================
// Network handler
// ============================================================================

async fn handle_network(client: &Client, cmd: NetworkCommands) -> Result<()> {
    match cmd {
        NetworkCommands::List {
            uuid,
            fabric,
            family,
            name,
            nic_tag,
            owner_uuid,
            provisionable_by,
            vlan_id,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_networks();
            if let Some(v) = uuid {
                req = req.uuid(v);
            }
            if let Some(v) = fabric {
                req = req.fabric(v);
            }
            if let Some(v) = family {
                req = req.family(v);
            }
            if let Some(v) = name {
                req = req.name(v);
            }
            if let Some(v) = nic_tag {
                req = req.nic_tag(v);
            }
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            if let Some(v) = provisionable_by {
                req = req.provisionable_by(v);
            }
            if let Some(v) = vlan_id {
                req = req.vlan_id(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List networks failed: {}", e))?;
            let networks = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&networks)?);
            } else {
                for net in &networks {
                    print_network_summary(net);
                }
                println!("({} networks)", networks.len());
            }
        }

        NetworkCommands::Get { uuid, raw } => {
            let resp = client
                .get_network()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        NetworkCommands::Create {
            name,
            nic_tag,
            vlan_id,
            subnet,
            provision_start_ip,
            provision_end_ip,
            description,
            gateway,
            resolvers,
            family,
            mtu,
            uuid,
            fabric,
            subnet_alloc,
            subnet_prefix,
            internet_nat,
            owner_uuids,
            vnet_id,
            raw,
        } => {
            let builder = client.create_network().body_map(|b| {
                let mut b = b.name(name).nic_tag(nic_tag).vlan_id(vlan_id);
                if let Some(v) = subnet {
                    b = b.subnet(v);
                }
                if let Some(v) = provision_start_ip {
                    b = b.provision_start_ip(v);
                }
                if let Some(v) = provision_end_ip {
                    b = b.provision_end_ip(v);
                }
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = gateway {
                    b = b.gateway(v);
                }
                if let Some(v) = resolvers {
                    b = b.resolvers(v);
                }
                if let Some(v) = family {
                    b = b.family(v);
                }
                if let Some(v) = mtu {
                    b = b.mtu(v);
                }
                if let Some(v) = uuid {
                    b = b.uuid(v);
                }
                if let Some(v) = fabric {
                    b = b.fabric(v);
                }
                if let Some(v) = subnet_alloc {
                    b = b.subnet_alloc(v);
                }
                if let Some(v) = subnet_prefix {
                    b = b.subnet_prefix(v);
                }
                if let Some(v) = internet_nat {
                    b = b.internet_nat(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                if let Some(v) = vnet_id {
                    b = b.vnet_id(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        NetworkCommands::Update {
            uuid,
            name,
            description,
            gateway,
            resolvers,
            provision_start_ip,
            provision_end_ip,
            owner_uuids,
            raw,
        } => {
            let builder = client.update_network().uuid(&uuid).body_map(|b| {
                let mut b = b;
                if let Some(v) = name {
                    b = b.name(v);
                }
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = gateway {
                    b = b.gateway(v);
                }
                if let Some(v) = resolvers {
                    b = b.resolvers(v);
                }
                if let Some(v) = provision_start_ip {
                    b = b.provision_start_ip(v);
                }
                if let Some(v) = provision_end_ip {
                    b = b.provision_end_ip(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        NetworkCommands::Delete { uuid } => {
            client
                .delete_network()
                .uuid(&uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete network failed: {}", e))?;
            println!("Network {} deleted", uuid);
        }
    }
    Ok(())
}

// ============================================================================
// Network Pool handler
// ============================================================================

async fn handle_pool(client: &Client, cmd: PoolCommands) -> Result<()> {
    match cmd {
        PoolCommands::List {
            uuid,
            name,
            family,
            networks,
            provisionable_by,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_network_pools();
            if let Some(v) = uuid {
                req = req.uuid(v);
            }
            if let Some(v) = name {
                req = req.name(v);
            }
            if let Some(v) = family {
                req = req.family(v);
            }
            if let Some(v) = networks {
                req = req.networks(v);
            }
            if let Some(v) = provisionable_by {
                req = req.provisionable_by(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List network pools failed: {}", e))?;
            let pools = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&pools)?);
            } else {
                for pool in &pools {
                    println!(
                        "{} {} [{}] networks={}",
                        pool.uuid,
                        pool.name,
                        enum_to_display(&pool.family),
                        pool.networks.len()
                    );
                }
                println!("({} pools)", pools.len());
            }
        }

        PoolCommands::Get { uuid, raw } => {
            let resp = client
                .get_network_pool()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get network pool failed: {}", e))?;
            let pool = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&pool)?);
            } else {
                print_pool_detail(&pool);
            }
        }

        PoolCommands::Create {
            name,
            networks,
            description,
            owner_uuids,
            uuid,
            raw,
        } => {
            let builder = client.create_network_pool().body_map(|b| {
                let mut b = b.name(name).networks(networks);
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                if let Some(v) = uuid {
                    b = b.uuid(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create network pool failed: {}", e))?;
            let pool = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&pool)?);
            } else {
                print_pool_detail(&pool);
            }
        }

        PoolCommands::Update {
            uuid,
            name,
            networks,
            description,
            owner_uuids,
            raw,
        } => {
            let builder = client.update_network_pool().uuid(uuid).body_map(|b| {
                let mut b = b;
                if let Some(v) = name {
                    b = b.name(v);
                }
                if let Some(v) = networks {
                    b = b.networks(v);
                }
                if let Some(v) = description {
                    b = b.description(v);
                }
                if let Some(v) = owner_uuids {
                    b = b.owner_uuids(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update network pool failed: {}", e))?;
            let pool = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&pool)?);
            } else {
                print_pool_detail(&pool);
            }
        }

        PoolCommands::Delete { uuid } => {
            client
                .delete_network_pool()
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete network pool failed: {}", e))?;
            println!("Network pool {} deleted", uuid);
        }
    }
    Ok(())
}

// ============================================================================
// IP handler
// ============================================================================

async fn handle_ip(client: &Client, cmd: IpCommands) -> Result<()> {
    match cmd {
        IpCommands::List {
            network_uuid,
            belongs_to_type,
            belongs_to_uuid,
            owner_uuid,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_ips().uuid(network_uuid);
            if let Some(v) = belongs_to_type {
                req = req.belongs_to_type(v);
            }
            if let Some(v) = belongs_to_uuid {
                req = req.belongs_to_uuid(v);
            }
            if let Some(v) = owner_uuid {
                req = req.owner_uuid(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List IPs failed: {}", e))?;
            let ips = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&ips)?);
            } else {
                for ip_rec in &ips {
                    print_ip_summary(ip_rec);
                }
                println!("({} IPs)", ips.len());
            }
        }

        IpCommands::Get {
            network_uuid,
            ip_addr,
            raw,
        } => {
            let resp = client
                .get_ip()
                .uuid(network_uuid)
                .ip_addr(ip_addr)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get IP failed: {}", e))?;
            let ip_rec = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&ip_rec)?);
            } else {
                print_ip_detail(&ip_rec);
            }
        }

        IpCommands::Update {
            network_uuid,
            ip_addr,
            belongs_to_type,
            belongs_to_uuid,
            owner_uuid,
            reserved,
            free,
            unassign,
            check_owner,
            raw,
        } => {
            let builder = client
                .update_ip()
                .uuid(network_uuid)
                .ip_addr(ip_addr)
                .body_map(|b| {
                    let mut b = b;
                    if let Some(v) = belongs_to_type {
                        b = b.belongs_to_type(v);
                    }
                    if let Some(v) = belongs_to_uuid {
                        b = b.belongs_to_uuid(v);
                    }
                    if let Some(v) = owner_uuid {
                        b = b.owner_uuid(v);
                    }
                    if let Some(v) = reserved {
                        b = b.reserved(v);
                    }
                    if let Some(v) = free {
                        b = b.free(v);
                    }
                    if let Some(v) = unassign {
                        b = b.unassign(v);
                    }
                    if let Some(v) = check_owner {
                        b = b.check_owner(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update IP failed: {}", e))?;
            let ip_rec = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&ip_rec)?);
            } else {
                print_ip_detail(&ip_rec);
            }
        }
    }
    Ok(())
}

// ============================================================================
// Aggregation handler
// ============================================================================

async fn handle_aggregation(client: &Client, cmd: AggregationCommands) -> Result<()> {
    match cmd {
        AggregationCommands::List {
            belongs_to_uuid,
            macs,
            nic_tags_provided,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_aggregations();
            if let Some(v) = belongs_to_uuid {
                req = req.belongs_to_uuid(v);
            }
            if let Some(v) = macs {
                req = req.macs(v);
            }
            if let Some(v) = nic_tags_provided {
                req = req.nic_tags_provided(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List aggregations failed: {}", e))?;
            let aggs = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&aggs)?);
            } else {
                for agg in &aggs {
                    println!(
                        "{} {} [{}] macs={}",
                        agg.id,
                        agg.name,
                        enum_to_display(&agg.lacp_mode),
                        agg.macs.join(", ")
                    );
                }
                println!("({} aggregations)", aggs.len());
            }
        }

        AggregationCommands::Get { id, raw } => {
            let resp = client
                .get_aggregation()
                .id(id)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get aggregation failed: {}", e))?;
            let agg = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&agg)?);
            } else {
                print_aggregation_detail(&agg);
            }
        }

        AggregationCommands::Create {
            name,
            macs,
            lacp_mode,
            nic_tags_provided,
            raw,
        } => {
            let builder = client.create_aggregation().body_map(|b| {
                let mut b = b.name(name).macs(macs);
                if let Some(v) = lacp_mode {
                    b = b.lacp_mode(v);
                }
                if let Some(v) = nic_tags_provided {
                    b = b.nic_tags_provided(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create aggregation failed: {}", e))?;
            let agg = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&agg)?);
            } else {
                print_aggregation_detail(&agg);
            }
        }

        AggregationCommands::Update {
            id,
            lacp_mode,
            nic_tags_provided,
            raw,
        } => {
            let builder = client.update_aggregation().id(&id).body_map(|b| {
                let mut b = b;
                if let Some(v) = lacp_mode {
                    b = b.lacp_mode(v);
                }
                if let Some(v) = nic_tags_provided {
                    b = b.nic_tags_provided(v);
                }
                b
            });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update aggregation failed: {}", e))?;
            let agg = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&agg)?);
            } else {
                print_aggregation_detail(&agg);
            }
        }

        AggregationCommands::Delete { id } => {
            client
                .delete_aggregation()
                .id(&id)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete aggregation failed: {}", e))?;
            println!("Aggregation {} deleted", id);
        }
    }
    Ok(())
}

// ============================================================================
// Fabric VLAN handler
// ============================================================================

async fn handle_fabric_vlan(client: &Client, cmd: FabricVlanCommands) -> Result<()> {
    match cmd {
        FabricVlanCommands::List {
            owner_uuid,
            fields,
            limit,
            offset,
            raw,
        } => {
            let mut req = client.list_fabric_vlans().owner_uuid(owner_uuid);
            if let Some(v) = fields {
                req = req.fields(v);
            }
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List fabric VLANs failed: {}", e))?;
            let vlans = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vlans)?);
            } else {
                for vlan in &vlans {
                    let name_str = vlan.name.as_deref().unwrap_or("<unnamed>");
                    println!(
                        "vlan_id={} vnet_id={} {} owner={}",
                        vlan.vlan_id, vlan.vnet_id, name_str, vlan.owner_uuid
                    );
                }
                println!("({} VLANs)", vlans.len());
            }
        }

        FabricVlanCommands::Get {
            owner_uuid,
            vlan_id,
            raw,
        } => {
            let resp = client
                .get_fabric_vlan()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get fabric VLAN failed: {}", e))?;
            let vlan = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vlan)?);
            } else {
                print_fabric_vlan_detail(&vlan);
            }
        }

        FabricVlanCommands::Create {
            owner_uuid,
            vlan_id,
            name,
            description,
            raw,
        } => {
            let builder = client
                .create_fabric_vlan()
                .owner_uuid(owner_uuid)
                .body_map(|b| {
                    let mut b = b.vlan_id(vlan_id);
                    if let Some(v) = name {
                        b = b.name(v);
                    }
                    if let Some(v) = description {
                        b = b.description(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create fabric VLAN failed: {}", e))?;
            let vlan = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vlan)?);
            } else {
                print_fabric_vlan_detail(&vlan);
            }
        }

        FabricVlanCommands::Update {
            owner_uuid,
            vlan_id,
            name,
            description,
            raw,
        } => {
            let builder = client
                .update_fabric_vlan()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .body_map(|b| {
                    let mut b = b;
                    if let Some(v) = name {
                        b = b.name(v);
                    }
                    if let Some(v) = description {
                        b = b.description(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update fabric VLAN failed: {}", e))?;
            let vlan = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&vlan)?);
            } else {
                print_fabric_vlan_detail(&vlan);
            }
        }

        FabricVlanCommands::Delete {
            owner_uuid,
            vlan_id,
        } => {
            client
                .delete_fabric_vlan()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete fabric VLAN failed: {}", e))?;
            println!("Fabric VLAN {} deleted", vlan_id);
        }
    }
    Ok(())
}

// ============================================================================
// Fabric Network handler
// ============================================================================

async fn handle_fabric_network(client: &Client, cmd: FabricNetworkCommands) -> Result<()> {
    match cmd {
        FabricNetworkCommands::List {
            owner_uuid,
            vlan_id,
            limit,
            offset,
            raw,
        } => {
            let mut req = client
                .list_fabric_networks()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id);
            if let Some(v) = limit {
                req = req.limit(v);
            }
            if let Some(v) = offset {
                req = req.offset(v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("List fabric networks failed: {}", e))?;
            let networks = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&networks)?);
            } else {
                for net in &networks {
                    print_network_summary(net);
                }
                println!("({} fabric networks)", networks.len());
            }
        }

        FabricNetworkCommands::Get {
            owner_uuid,
            vlan_id,
            uuid,
            raw,
        } => {
            let resp = client
                .get_fabric_network()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Get fabric network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        FabricNetworkCommands::Create {
            owner_uuid,
            vlan_id,
            name,
            subnet,
            provision_start_ip,
            provision_end_ip,
            gateway,
            resolvers,
            description,
            internet_nat,
            uuid,
            raw,
        } => {
            let builder = client
                .create_fabric_network()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .body_map(|b| {
                    let mut b = b
                        .name(name)
                        .subnet(subnet)
                        .provision_start_ip(provision_start_ip)
                        .provision_end_ip(provision_end_ip);
                    if let Some(v) = gateway {
                        b = b.gateway(v);
                    }
                    if let Some(v) = resolvers {
                        b = b.resolvers(v);
                    }
                    if let Some(v) = description {
                        b = b.description(v);
                    }
                    if let Some(v) = internet_nat {
                        b = b.internet_nat(v);
                    }
                    if let Some(v) = uuid {
                        b = b.uuid(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Create fabric network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        FabricNetworkCommands::Update {
            owner_uuid,
            vlan_id,
            uuid,
            name,
            description,
            gateway,
            resolvers,
            provision_start_ip,
            provision_end_ip,
            raw,
        } => {
            let builder = client
                .update_fabric_network()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .uuid(uuid)
                .body_map(|b| {
                    let mut b = b;
                    if let Some(v) = name {
                        b = b.name(v);
                    }
                    if let Some(v) = description {
                        b = b.description(v);
                    }
                    if let Some(v) = gateway {
                        b = b.gateway(v);
                    }
                    if let Some(v) = resolvers {
                        b = b.resolvers(v);
                    }
                    if let Some(v) = provision_start_ip {
                        b = b.provision_start_ip(v);
                    }
                    if let Some(v) = provision_end_ip {
                        b = b.provision_end_ip(v);
                    }
                    b
                });
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Update fabric network failed: {}", e))?;
            let net = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&net)?);
            } else {
                print_network_detail(&net);
            }
        }

        FabricNetworkCommands::Delete {
            owner_uuid,
            vlan_id,
            uuid,
        } => {
            client
                .delete_fabric_network()
                .owner_uuid(owner_uuid)
                .vlan_id(vlan_id)
                .uuid(uuid)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Delete fabric network failed: {}", e))?;
            println!("Fabric network {} deleted", uuid);
        }
    }
    Ok(())
}

// ============================================================================
// Display helpers
// ============================================================================

fn print_nic_summary(nic: &types::Nic) {
    let ip_str = nic.ip.as_deref().unwrap_or("(none)");
    println!(
        "{} {} {} owner={} ip={}",
        nic.mac,
        enum_to_display(&nic.state),
        enum_to_display(&nic.belongs_to_type),
        nic.owner_uuid,
        ip_str,
    );
}

fn print_nic_detail(nic: &types::Nic) {
    println!("MAC:           {}", nic.mac);
    println!("State:         {}", enum_to_display(&nic.state));
    println!(
        "Belongs to:    {} {}",
        enum_to_display(&nic.belongs_to_type),
        nic.belongs_to_uuid
    );
    println!("Owner:         {}", nic.owner_uuid);
    println!("Primary:       {}", nic.primary);
    if let Some(ref ip) = nic.ip {
        println!("IP:            {}", ip);
    }
    if let Some(ref network_uuid) = nic.network_uuid {
        println!("Network:       {}", network_uuid);
    }
    if let Some(ref nic_tag) = nic.nic_tag {
        println!("NIC Tag:       {}", nic_tag);
    }
    if let Some(vlan_id) = nic.vlan_id {
        println!("VLAN ID:       {}", vlan_id);
    }
    if let Some(mtu) = nic.mtu {
        println!("MTU:           {}", mtu);
    }
    if let Some(ref gateway) = nic.gateway {
        println!("Gateway:       {}", gateway);
    }
    if let Some(ref netmask) = nic.netmask {
        println!("Netmask:       {}", netmask);
    }
    if let Some(ref resolvers) = nic.resolvers {
        println!("Resolvers:     {}", resolvers.join(", "));
    }
    if let Some(ref cn_uuid) = nic.cn_uuid {
        println!("CN UUID:       {}", cn_uuid);
    }
    if let Some(ref model) = nic.model {
        println!("Model:         {}", model);
    }
    if let Some(fabric) = nic.fabric {
        println!("Fabric:        {}", fabric);
    }
    if let Some(ref tags) = nic.nic_tags_provided {
        println!("Tags Provided: {}", tags.join(", "));
    }
    println!("Created:       {}", nic.created_timestamp);
    println!("Modified:      {}", nic.modified_timestamp);
}

fn print_nic_tag_detail(tag: &types::NicTag) {
    println!("UUID:   {}", tag.uuid);
    println!("Name:   {}", tag.name);
    println!("MTU:    {}", tag.mtu);
}

fn print_network_summary(net: &types::Network) {
    println!(
        "{} {} [{}] subnet={} vlan={}",
        net.uuid,
        net.name,
        enum_to_display(&net.family),
        net.subnet,
        net.vlan_id,
    );
}

fn print_network_detail(net: &types::Network) {
    println!("UUID:        {}", net.uuid);
    println!("Name:        {}", net.name);
    println!("Family:      {}", enum_to_display(&net.family));
    println!("Subnet:      {}", net.subnet);
    println!("NIC Tag:     {}", net.nic_tag);
    println!("VLAN ID:     {}", net.vlan_id);
    println!("MTU:         {}", net.mtu);
    println!("Start IP:    {}", net.provision_start_ip);
    println!("End IP:      {}", net.provision_end_ip);
    println!("Resolvers:   {}", net.resolvers.join(", "));
    if let Some(ref gw) = net.gateway {
        println!("Gateway:     {}", gw);
    }
    if let Some(ref desc) = net.description {
        println!("Description: {}", desc);
    }
    if let Some(fabric) = net.fabric {
        println!("Fabric:      {}", fabric);
    }
    if let Some(vnet_id) = net.vnet_id {
        println!("VNET ID:     {}", vnet_id);
    }
    if let Some(ref owners) = net.owner_uuids {
        let uuids: Vec<String> = owners.iter().map(|u| u.to_string()).collect();
        println!("Owners:      {}", uuids.join(", "));
    }
}

fn print_pool_detail(pool: &types::NetworkPool) {
    println!("UUID:        {}", pool.uuid);
    println!("Name:        {}", pool.name);
    println!("Family:      {}", enum_to_display(&pool.family));
    if let Some(ref desc) = pool.description {
        println!("Description: {}", desc);
    }
    let net_uuids: Vec<String> = pool.networks.iter().map(|u| u.to_string()).collect();
    println!("Networks:    {}", net_uuids.join(", "));
    if let Some(ref tags) = pool.nic_tags_present {
        println!("NIC Tags:    {}", tags.join(", "));
    }
    if let Some(ref owners) = pool.owner_uuids {
        let uuids: Vec<String> = owners.iter().map(|u| u.to_string()).collect();
        println!("Owners:      {}", uuids.join(", "));
    }
}

fn print_ip_summary(ip_rec: &types::Ip) {
    let status = if ip_rec.free {
        "free"
    } else if ip_rec.reserved {
        "reserved"
    } else {
        "assigned"
    };
    println!("{} [{}] network={}", ip_rec.ip, status, ip_rec.network_uuid,);
}

fn print_ip_detail(ip_rec: &types::Ip) {
    println!("IP:          {}", ip_rec.ip);
    println!("Network:     {}", ip_rec.network_uuid);
    println!("Reserved:    {}", ip_rec.reserved);
    println!("Free:        {}", ip_rec.free);
    if let Some(ref bt) = ip_rec.belongs_to_type {
        println!("Belongs to:  {}", bt);
    }
    if let Some(ref bu) = ip_rec.belongs_to_uuid {
        println!("Belongs UUID:{}", bu);
    }
    if let Some(ref ou) = ip_rec.owner_uuid {
        println!("Owner:       {}", ou);
    }
}

fn print_aggregation_detail(agg: &types::Aggregation) {
    println!("ID:          {}", agg.id);
    println!("Name:        {}", agg.name);
    println!("Belongs to:  {}", agg.belongs_to_uuid);
    println!("LACP Mode:   {}", enum_to_display(&agg.lacp_mode));
    println!("MACs:        {}", agg.macs.join(", "));
    if let Some(ref tags) = agg.nic_tags_provided {
        println!("Tags:        {}", tags.join(", "));
    }
}

fn print_fabric_vlan_detail(vlan: &types::FabricVlan) {
    println!("VLAN ID:     {}", vlan.vlan_id);
    println!("VNET ID:     {}", vlan.vnet_id);
    println!("Owner:       {}", vlan.owner_uuid);
    if let Some(ref name) = vlan.name {
        println!("Name:        {}", name);
    }
    if let Some(ref desc) = vlan.description {
        println!("Description: {}", desc);
    }
}
