// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Create cluster metadata

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_client::TypedClient;
use std::collections::HashSet;
use uuid::Uuid;

use super::state::ClusterState;
use crate::output::json;

#[derive(Args, Clone)]
pub struct CreateArgs {
    /// Cluster name
    #[arg(long)]
    pub name: String,

    /// Cluster description
    #[arg(long)]
    pub description: Option<String>,

    /// Fabric network ID or name to use for cluster networking
    #[arg(long)]
    pub fabric: Option<String>,

    /// Create a new fabric network for the cluster
    #[arg(long, conflicts_with = "fabric")]
    pub create_fabric: bool,
}

pub async fn run(args: CreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // Parse fabric network UUID if provided
    let fabric_network_id = if let Some(fabric) = &args.fabric {
        Some(Uuid::parse_str(fabric)?)
    } else if args.create_fabric {
        eprintln!("==> Creating fabric network for cluster");
        let network_id = create_fabric_network(&args.name, client).await?;
        Some(network_id)
    } else {
        // Default to create-fabric behavior
        None
    };

    // Create cluster state
    let cluster = ClusterState::new(
        args.name.clone(),
        args.description.clone(),
        fabric_network_id,
    );

    // Save to disk
    cluster.save().await?;

    if use_json {
        json::print_json(&cluster)?;
    } else {
        println!("Created cluster {} ({})", cluster.name, cluster.uuid);
        println!("State saved to: {}", cluster.cluster_dir()?.display());
        if let Some(fabric_id) = fabric_network_id {
            println!("Fabric network: {}", &fabric_id.to_string()[..8]);
        }
    }

    Ok(())
}

/// Create a new fabric network for the cluster
///
/// This function:
/// 1. Discovers available fabric VLANs
/// 2. Selects a VLAN to use (picks the first one)
/// 3. Lists existing networks on that VLAN
/// 4. Allocates a unique /24 subnet in the 10.0.0.0/8 range
/// 5. Creates the fabric network with appropriate settings
///
/// Returns the UUID of the created network
async fn create_fabric_network(cluster_name: &str, client: &TypedClient) -> Result<Uuid> {
    let account = client.effective_account();

    // 1. List all fabric VLANs
    eprintln!("    Discovering fabric VLANs");
    let vlans_response = client
        .inner()
        .list_fabric_vlans()
        .account(account)
        .send()
        .await
        .context("Failed to list fabric VLANs")?;

    let vlans = vlans_response.into_inner();

    if vlans.is_empty() {
        anyhow::bail!("No fabric VLANs found. Please create a VLAN first with: triton vlan create");
    }

    // 2. Select the first VLAN (or we could make this configurable)
    let vlan = &vlans[0];
    eprintln!("    Using VLAN {} ({})", vlan.vlan_id, vlan.name);

    // 3. List existing networks on this VLAN to find used subnets
    let networks_response = client
        .inner()
        .list_fabric_networks()
        .account(account)
        .vlan_id(vlan.vlan_id)
        .send()
        .await
        .context("Failed to list existing networks")?;

    let existing_networks = networks_response.into_inner();

    // 4. Find an available /24 subnet in the 10.0.0.0/8 range
    let used_subnets: HashSet<String> = existing_networks
        .iter()
        .filter_map(|net| net.subnet.clone())
        .collect();

    let subnet = find_available_subnet(&used_subnets)?;
    eprintln!("    Allocated subnet: {}", subnet);

    // 5. Create the fabric network
    let network_name = format!("{}-fabric", cluster_name);

    // Parse subnet to get base IP and calculate provision range
    // For a /24 network like 10.0.1.0/24:
    // - provision_start_ip: 10.0.1.5 (skip .0-.4 for gateway, etc.)
    // - provision_end_ip: 10.0.1.250 (reserve .251-.255)
    // - gateway: 10.0.1.1
    let (base_ip, prefix) = subnet
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Invalid subnet format: {}", subnet))?;

    if prefix != "24" {
        anyhow::bail!("Expected /24 subnet, got /{}", prefix);
    }

    // Extract the network portion (e.g., "10.0.1" from "10.0.1.0")
    let octets: Vec<&str> = base_ip.split('.').collect();
    if octets.len() != 4 {
        anyhow::bail!("Invalid IP address: {}", base_ip);
    }

    let network_base = format!("{}.{}.{}", octets[0], octets[1], octets[2]);
    let gateway = format!("{}.1", network_base);
    let provision_start = format!("{}.5", network_base);
    let provision_end = format!("{}.250", network_base);

    // Use Google's DNS servers as resolvers
    let resolvers = vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()];

    let request = cloudapi_client::types::CreateFabricNetworkRequest {
        name: network_name.clone(),
        description: Some(format!("Fabric network for K8s cluster {}", cluster_name)),
        subnet: subnet.clone(),
        provision_start_ip: provision_start,
        provision_end_ip: provision_end,
        gateway: Some(gateway.clone()),
        resolvers: Some(resolvers),
        routes: Some(serde_json::Value::Object(serde_json::Map::new())),
        internet_nat: None, // Enable NAT (default behavior)
    };

    eprintln!("    Creating fabric network '{}'", network_name);
    let response = client
        .inner()
        .create_fabric_network()
        .account(account)
        .vlan_id(vlan.vlan_id)
        .body(request)
        .send()
        .await
        .context("Failed to create fabric network")?;

    let network = response.into_inner();
    eprintln!(
        "    Created network {} ({})",
        network.name,
        &network.id.to_string()[..8]
    );
    eprintln!("    Subnet: {}", subnet);
    eprintln!("    Gateway: {}", gateway);

    Ok(network.id)
}

/// Find an available /24 subnet in the 10.0.0.0/8 range
///
/// Searches through 10.0.0.0/24, 10.0.1.0/24, ... 10.0.255.0/24,
/// then 10.1.0.0/24, etc. until finding an unused subnet.
///
/// # Arguments
/// * `used_subnets` - Set of already-used subnets in CIDR notation
///
/// # Returns
/// An available subnet in CIDR notation (e.g., "10.0.1.0/24")
fn find_available_subnet(used_subnets: &HashSet<String>) -> Result<String> {
    // Search through 10.0.0.0/24 -> 10.255.255.0/24
    for second_octet in 0..=255 {
        for third_octet in 0..=255 {
            let subnet = format!("10.{}.{}.0/24", second_octet, third_octet);
            if !used_subnets.contains(&subnet) {
                return Ok(subnet);
            }
        }
    }

    anyhow::bail!("No available /24 subnets found in 10.0.0.0/8 range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_available_subnet_empty() {
        let used = HashSet::new();
        let subnet = find_available_subnet(&used).unwrap();
        assert_eq!(subnet, "10.0.0.0/24");
    }

    #[test]
    fn test_find_available_subnet_first_used() {
        let mut used = HashSet::new();
        used.insert("10.0.0.0/24".to_string());

        let subnet = find_available_subnet(&used).unwrap();
        assert_eq!(subnet, "10.0.1.0/24");
    }

    #[test]
    fn test_find_available_subnet_multiple_used() {
        let mut used = HashSet::new();
        used.insert("10.0.0.0/24".to_string());
        used.insert("10.0.1.0/24".to_string());
        used.insert("10.0.2.0/24".to_string());

        let subnet = find_available_subnet(&used).unwrap();
        assert_eq!(subnet, "10.0.3.0/24");
    }

    #[test]
    fn test_find_available_subnet_wraps_to_next_octet() {
        let mut used = HashSet::new();
        // Use all of 10.0.x.0/24
        for i in 0..=255 {
            used.insert(format!("10.0.{}.0/24", i));
        }

        let subnet = find_available_subnet(&used).unwrap();
        assert_eq!(subnet, "10.1.0.0/24");
    }
}
