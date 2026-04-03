// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton instance provisioning helpers for K8s clusters

use anyhow::{Context, Result, bail};
use cloudapi_client::TypedClient;
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;

use super::state::NodeRole;

/// Network configuration for a NIC
#[derive(Debug, Clone)]
pub struct NicConfig {
    pub mac: String,
    pub ip: String,
    pub netmask: String,
    pub gateway: Option<String>,
    pub network_id: Uuid,
    pub primary: bool,
}

/// Instance provisioning result
#[derive(Debug, Clone)]
pub struct ProvisionedInstance {
    pub instance_id: Uuid,
    pub name: String,
    pub primary_ip: Option<String>,
    pub nics: Vec<NicConfig>,
}

/// Fabric network details
#[derive(Debug, Clone)]
pub struct FabricNetworkInfo {
    #[allow(dead_code)]
    pub id: Uuid,

    pub name: String,
    pub subnet: String,
    pub gateway: Option<String>,
    pub resolvers: Vec<String>,

    #[allow(dead_code)]
    pub vlan_id: u16,
}

/// Pre-allocated node with assigned fabric IP
#[derive(Debug, Clone)]
pub struct PreallocatedNode {
    /// Node name (e.g. "cluster-ctrl-0", "cluster-worker-1")
    pub name: String,

    /// Node role (control or worker)
    pub role: NodeRole,

    /// Pre-allocated fabric IP address
    pub fabric_ip: IpAddr,
}

/// Pre-allocate fabric IPs for all cluster nodes
///
/// Allocates IPs starting from .10 in the fabric subnet.
/// Control plane nodes are allocated first, followed by workers.
///
/// # Arguments
/// * `fabric_subnet` - CIDR notation subnet string (e.g. "10.0.0.0/24")
/// * `control_count` - Number of control plane nodes
/// * `worker_count` - Number of worker nodes
/// * `cluster_name` - Cluster name for generating node names
///
/// # Returns
/// Vector of pre-allocated nodes with their assigned fabric IPs
///
/// # Errors
/// Returns error if:
/// - Subnet string is invalid
/// - Not enough IPs available in subnet
/// - Subnet is not IPv4
pub fn preallocate_fabric_ips(
    fabric_subnet: &str,
    control_count: u32,
    worker_count: u32,
    cluster_name: &str,
) -> Result<Vec<PreallocatedNode>> {
    // Parse subnet in CIDR notation (e.g. "10.0.0.0/24")
    let parts: Vec<&str> = fabric_subnet.split('/').collect();
    if parts.len() != 2 {
        bail!(
            "Invalid subnet format '{}', expected CIDR notation (e.g. 10.0.0.0/24)",
            fabric_subnet
        );
    }

    let base_ip: Ipv4Addr = parts[0]
        .parse()
        .context("Failed to parse subnet IP address")?;

    let prefix_len: u8 = parts[1]
        .parse()
        .context("Failed to parse subnet prefix length")?;

    if prefix_len > 32 {
        bail!("Invalid prefix length {}, must be <= 32", prefix_len);
    }

    // Calculate how many host addresses are available in the subnet
    // For a /24: 2^(32-24) = 256 addresses total
    // We start at .10 to avoid network address, gateway, and reserved IPs
    let host_bits = 32 - prefix_len;
    let total_hosts = 2u32.pow(host_bits as u32);

    // Starting offset: .10 in the subnet
    const STARTING_OFFSET: u32 = 10;

    let total_nodes = control_count + worker_count;
    let needed_ips = STARTING_OFFSET + total_nodes;

    if needed_ips > total_hosts {
        bail!(
            "Not enough IPs in subnet /{}: need {} IPs (starting from .{}) but only {} available",
            prefix_len,
            total_nodes,
            STARTING_OFFSET,
            total_hosts.saturating_sub(STARTING_OFFSET)
        );
    }

    let mut preallocated = Vec::new();
    let base_ip_u32 = u32::from(base_ip);

    // Allocate control plane IPs first
    for i in 0..control_count {
        let ip_offset = STARTING_OFFSET + i;
        let ip_u32 = base_ip_u32 + ip_offset;
        let ip = Ipv4Addr::from(ip_u32);

        preallocated.push(PreallocatedNode {
            name: format!("{}-ctrl-{}", cluster_name, i),
            role: NodeRole::Control,
            fabric_ip: IpAddr::V4(ip),
        });
    }

    // Allocate worker IPs
    for i in 0..worker_count {
        let ip_offset = STARTING_OFFSET + control_count + i;
        let ip_u32 = base_ip_u32 + ip_offset;
        let ip = Ipv4Addr::from(ip_u32);

        preallocated.push(PreallocatedNode {
            name: format!("{}-worker-{}", cluster_name, i),
            role: NodeRole::Worker,
            fabric_ip: IpAddr::V4(ip),
        });
    }

    Ok(preallocated)
}

/// Discover and validate fabric network
///
/// Queries fabric network details via CloudAPI and validates it exists
/// and is actually a fabric network.
pub async fn discover_fabric_network(
    network_id: Uuid,
    client: &TypedClient,
) -> Result<FabricNetworkInfo> {
    let account = client.effective_account();

    // Get network details
    let response = client
        .inner()
        .get_network()
        .account(account)
        .network(network_id.to_string())
        .send()
        .await?;

    let network = response.into_inner();

    // Validate it's a fabric network
    if !network.fabric.unwrap_or(false) {
        anyhow::bail!(
            "Network {} ({}) is not a fabric network",
            network.name,
            network_id
        );
    }

    let vlan_id = network
        .vlan_id
        .ok_or_else(|| anyhow::anyhow!("Fabric network missing vlan_id"))?;

    // Extract resolvers from network details
    let resolvers = network
        .resolvers
        .unwrap_or_default()
        .iter()
        .map(|v| v.to_string())
        .collect();

    Ok(FabricNetworkInfo {
        id: network.id,
        name: network.name,
        subnet: network.subnet.unwrap_or_default(),
        gateway: network.gateway,
        resolvers,
        vlan_id,
    })
}

/// Create instance with proper NIC configuration
///
/// For control plane: external (primary) + fabric (secondary if fabric specified)
/// For workers: fabric only (or + worker-network if specified)
///
/// If `user_data` is provided, it will be passed as cloud-init:user-data metadata.
/// This is required for Talos Linux images to receive their machine configuration.
///
/// If `fabric_ip` is provided, it will be used as the pre-allocated IP address for
/// the fabric NIC (requires fabric_network_id to be Some).
#[allow(clippy::too_many_arguments)]
pub async fn create_instance(
    name: String,
    image_id: &str,
    package_id: &str,
    role: NodeRole,
    external_network_id: Uuid,
    fabric_network_id: Option<Uuid>,
    additional_networks: &[Uuid],
    cluster_id: Uuid,
    user_data: Option<&str>,
    fabric_ip: Option<&IpAddr>,
    client: &TypedClient,
) -> Result<ProvisionedInstance> {
    let account = client.effective_account();

    // Build NIC configuration based on role
    // Note: CloudAPI automatically sets the first external network as primary,
    // so we don't set the `primary` field - it would cause "Invalid Networks" error.
    // We order networks so the primary NIC comes first.
    let networks = match role {
        NodeRole::Control => {
            // Control plane: external (first/primary) + fabric (secondary)
            let mut nets = vec![cloudapi_client::types::NetworkObject {
                ipv4_uuid: external_network_id,
                ipv4_ips: None,
                primary: None,
            }];

            if let Some(fabric_id) = fabric_network_id {
                nets.push(cloudapi_client::types::NetworkObject {
                    ipv4_uuid: fabric_id,
                    ipv4_ips: fabric_ip.map(|ip| vec![ip.to_string()]),
                    primary: None,
                });
            }

            nets
        }
        NodeRole::Worker => {
            // Workers: fabric (first/primary) + additional worker networks
            let mut nets = Vec::new();

            if let Some(fabric_id) = fabric_network_id {
                nets.push(cloudapi_client::types::NetworkObject {
                    ipv4_uuid: fabric_id,
                    ipv4_ips: fabric_ip.map(|ip| vec![ip.to_string()]),
                    primary: None,
                });
            }

            for net_id in additional_networks {
                nets.push(cloudapi_client::types::NetworkObject {
                    ipv4_uuid: *net_id,
                    ipv4_ips: None,
                    primary: None,
                });
            }

            nets
        }
    };

    // Build tags for the instance.
    // Note: Tags prefixed with "triton." are reserved and only specific whitelisted
    // tags are allowed (e.g. triton.cns.services). We use "k8s." prefix for our tags.
    let mut tags = serde_json::Map::new();
    tags.insert(
        "k8s.cluster".to_string(),
        serde_json::Value::String(cluster_id.to_string()),
    );
    tags.insert(
        "k8s.role".to_string(),
        serde_json::Value::String(match role {
            NodeRole::Control => "control".to_string(),
            NodeRole::Worker => "worker".to_string(),
        }),
    );

    // Add CNS tags
    let cns_services = match role {
        NodeRole::Control => "k8s,ctrl",
        NodeRole::Worker => "k8s,worker",
    };
    tags.insert(
        "triton.cns.services".to_string(),
        serde_json::Value::String(cns_services.to_string()),
    );

    // Build metadata for cloud-init user-data (required for Talos)
    let metadata = if let Some(data) = user_data {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "cloud-init:user-data".to_string(),
            serde_json::Value::String(data.to_string()),
        );
        Some(meta)
    } else {
        None
    };

    // Build create request
    let mut builder = cloudapi_client::types::CreateMachineRequest::builder()
        .name(name.clone())
        .image(image_id.to_string())
        .package(package_id.to_string())
        .networks(networks)
        .tags(tags)
        .firewall_enabled(true);

    if let Some(meta) = metadata {
        builder = builder.metadata(meta);
    }

    let request =
        builder
            .try_into()
            .map_err(|e: cloudapi_client::types::error::ConversionError| {
                anyhow::anyhow!("Failed to build instance request: {}", e)
            })?;

    // Create the instance with retry logic for MAC address conflicts
    // (can occur when NAPI hasn't fully released NICs from recently deleted instances)
    let mut last_error = None;
    let mut machine = None;

    for attempt in 1..=4 {
        match client.create_machine(account, &request).await {
            Ok(m) => {
                machine = Some(m);
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                // Check if this is a MAC address conflict error and we have retries left
                let is_mac_conflict = error_str.contains("mac")
                    || error_str.contains("MAC")
                    || error_str.contains("in use");

                if is_mac_conflict && attempt < 4 {
                    eprintln!(
                        "    Instance creation failed (attempt {}): MAC address conflict, retrying in 3s...",
                        attempt
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    last_error = Some(e);
                    continue;
                }
                // Not a MAC error or final attempt - fail immediately
                return Err(anyhow::anyhow!("Failed to create instance: {}", e));
            }
        }
    }

    let machine = machine.ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to create instance after 4 attempts: {}",
            last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        )
    })?;

    eprintln!(
        "Creating instance {} ({}) as {}",
        machine.name,
        &machine.id.to_string()[..8],
        match role {
            NodeRole::Control => "control plane",
            NodeRole::Worker => "worker",
        }
    );

    Ok(ProvisionedInstance {
        instance_id: machine.id,
        name: machine.name,
        primary_ip: machine.primary_ip,
        nics: Vec::new(), // Will be populated by query_instance_nics
    })
}

/// Wait for instance to reach running state
///
/// Polls instance state until "running" or timeout
pub async fn wait_for_running(
    instance_id: Uuid,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    use super::super::instance::wait::wait_for_state;

    wait_for_state(
        instance_id,
        cloudapi_client::types::MachineState::Running,
        timeout_secs,
        client,
    )
    .await
}

/// Query instance NIC data
///
/// Uses CloudAPI's ListInstanceNics to get MAC addresses, IPs, netmasks
/// Returns structured NIC data for network patch generation
pub async fn query_instance_nics(
    instance_id: Uuid,
    client: &TypedClient,
) -> Result<Vec<NicConfig>> {
    let account = client.effective_account();

    let response = client
        .inner()
        .list_nics()
        .account(account)
        .machine(instance_id)
        .send()
        .await?;

    let nics = response.into_inner();

    let mut nic_configs = Vec::new();
    for nic in nics {
        nic_configs.push(NicConfig {
            mac: nic.mac,
            ip: nic.ip,
            netmask: nic.netmask,
            gateway: nic.gateway,
            network_id: nic.network,
            primary: nic.primary,
        });
    }

    // Sort by primary first, then by IP
    nic_configs.sort_by(|a, b| {
        if a.primary != b.primary {
            b.primary.cmp(&a.primary) // primary first
        } else {
            a.ip.cmp(&b.ip)
        }
    });

    Ok(nic_configs)
}

/// Create firewall rules for cluster
///
/// Creates rules for:
/// 1. User IP → control plane (ports 6443, 50000)
/// 2. Intra-cluster communication on fabric (all ports)
pub async fn create_firewall_rules(
    cluster_id: Uuid,
    _control_plane_tag: &str,
    fabric_network_id: Option<Uuid>,
    user_ips: &[String],
    client: &TypedClient,
) -> Result<Vec<Uuid>> {
    let account = client.effective_account();
    let mut rule_ids = Vec::new();

    // Rule 1: User IP(s) → control plane (API server + Talos API)
    // Create two rules per IP address: one for K8s API (6443), one for Talos API (50000)
    for ip in user_ips {
        // Rule for Kubernetes API (port 6443)
        let rule_text = format!(
            "FROM ip {} TO tag \"k8s.cluster\" = \"{}\" ALLOW tcp PORT 6443",
            ip, cluster_id
        );

        let request = cloudapi_client::types::CreateFirewallRuleRequest {
            rule: rule_text.clone(),
            enabled: Some(true),
            log: None,
            description: Some(format!(
                "K8s cluster {} - K8s API access from {}",
                cluster_id, ip
            )),
        };

        let response = client
            .inner()
            .create_firewall_rule()
            .account(account)
            .body(request)
            .send()
            .await?;

        let rule = response.into_inner();
        eprintln!(
            "Created firewall rule {} for K8s API access from {}",
            &rule.id.to_string()[..8],
            ip
        );
        rule_ids.push(rule.id);

        // Rule for Talos API (port 50000)
        let rule_text = format!(
            "FROM ip {} TO tag \"k8s.cluster\" = \"{}\" ALLOW tcp PORT 50000",
            ip, cluster_id
        );

        let request = cloudapi_client::types::CreateFirewallRuleRequest {
            rule: rule_text.clone(),
            enabled: Some(true),
            log: None,
            description: Some(format!(
                "K8s cluster {} - Talos API access from {}",
                cluster_id, ip
            )),
        };

        let response = client
            .inner()
            .create_firewall_rule()
            .account(account)
            .body(request)
            .send()
            .await?;

        let rule = response.into_inner();
        eprintln!(
            "Created firewall rule {} for Talos API access from {}",
            &rule.id.to_string()[..8],
            ip
        );
        rule_ids.push(rule.id);
    }

    // Rule 2: Intra-cluster communication on fabric
    if fabric_network_id.is_some() {
        let rule_text = format!(
            "FROM tag \"k8s.cluster\" = \"{}\" TO tag \"k8s.cluster\" = \"{}\" ALLOW tcp PORT all",
            cluster_id, cluster_id
        );

        let request = cloudapi_client::types::CreateFirewallRuleRequest {
            rule: rule_text,
            enabled: Some(true),
            log: None,
            description: Some(format!("K8s cluster {} - Intra-cluster TCP", cluster_id)),
        };

        let response = client
            .inner()
            .create_firewall_rule()
            .account(account)
            .body(request)
            .send()
            .await?;

        let rule = response.into_inner();
        eprintln!(
            "Created firewall rule {} for intra-cluster TCP",
            &rule.id.to_string()[..8]
        );
        rule_ids.push(rule.id);

        // UDP rule for intra-cluster
        let rule_text = format!(
            "FROM tag \"k8s.cluster\" = \"{}\" TO tag \"k8s.cluster\" = \"{}\" ALLOW udp PORT all",
            cluster_id, cluster_id
        );

        let request = cloudapi_client::types::CreateFirewallRuleRequest {
            rule: rule_text,
            enabled: Some(true),
            log: None,
            description: Some(format!("K8s cluster {} - Intra-cluster UDP", cluster_id)),
        };

        let response = client
            .inner()
            .create_firewall_rule()
            .account(account)
            .body(request)
            .send()
            .await?;

        let rule = response.into_inner();
        eprintln!(
            "Created firewall rule {} for intra-cluster UDP",
            &rule.id.to_string()[..8]
        );
        rule_ids.push(rule.id);
    }

    Ok(rule_ids)
}

/// Detect user's current public IP
///
/// Tries fetching from https://api.ipify.org
/// Returns None if detection fails (caller should prompt user)
pub async fn detect_user_ip() -> Option<String> {
    match reqwest::get("https://api.ipify.org").await {
        Ok(response) => match response.text().await {
            Ok(ip) => {
                let ip = ip.trim();
                // Basic validation
                if ip.split('.').count() == 4 {
                    Some(ip.to_string())
                } else {
                    None
                }
            }
            Err(_) => None,
        },
        Err(_) => None,
    }
}

/// Provision control plane instances
///
/// Creates multiple control plane instances with proper networking.
/// If `user_data` is provided, it will be passed as cloud-init:user-data.
#[allow(clippy::too_many_arguments)]
pub async fn provision_control_plane(
    count: u32,
    image_id: &str,
    package_id: &str,
    external_network_id: Uuid,
    fabric_network_id: Option<Uuid>,
    cluster_id: Uuid,
    cluster_name: &str,
    user_data: Option<&str>,
    client: &TypedClient,
) -> Result<Vec<ProvisionedInstance>> {
    let mut instances = Vec::new();

    for i in 0..count {
        let name = format!("{}-ctrl-{}", cluster_name, i);
        let instance = create_instance(
            name,
            image_id,
            package_id,
            NodeRole::Control,
            external_network_id,
            fabric_network_id,
            &[],
            cluster_id,
            user_data,
            None, // fabric_ip - not using pre-allocated IPs for control plane yet
            client,
        )
        .await?;
        instances.push(instance);
    }

    Ok(instances)
}

/// Provision worker instances
///
/// Creates multiple worker instances with fabric networking.
/// If `user_data` is provided, it will be passed as cloud-init:user-data.
#[allow(clippy::too_many_arguments)]
pub async fn provision_workers(
    count: u32,
    image_id: &str,
    package_id: &str,
    fabric_network_id: Option<Uuid>,
    additional_networks: &[Uuid],
    cluster_id: Uuid,
    cluster_name: &str,
    external_network_id: Uuid,
    user_data: Option<&str>,
    client: &TypedClient,
) -> Result<Vec<ProvisionedInstance>> {
    let mut instances = Vec::new();

    for i in 0..count {
        let name = format!("{}-worker-{}", cluster_name, i);
        let instance = create_instance(
            name,
            image_id,
            package_id,
            NodeRole::Worker,
            external_network_id,
            fabric_network_id,
            additional_networks,
            cluster_id,
            user_data,
            None, // fabric_ip - not using pre-allocated IPs yet
            client,
        )
        .await?;
        instances.push(instance);
    }

    Ok(instances)
}

/// Provision additional worker instances starting from a specific index
///
/// Unlike `provision_workers`, this function allows specifying:
/// - Starting worker index (for naming: cluster-worker-{start_index + i})
///
/// This is used when adding workers to an existing cluster.
#[allow(clippy::too_many_arguments)]
pub async fn provision_additional_workers(
    count: u32,
    start_index: u32,
    image_id: &str,
    package_id: &str,
    fabric_network_id: Option<Uuid>,
    additional_networks: &[Uuid],
    cluster_id: Uuid,
    cluster_name: &str,
    external_network_id: Uuid,
    user_data: Option<&str>,
    client: &TypedClient,
) -> Result<Vec<ProvisionedInstance>> {
    let mut instances = Vec::new();

    for i in 0..count {
        let worker_index = start_index + i;
        let name = format!("{}-worker-{}", cluster_name, worker_index);
        let instance = create_instance(
            name,
            image_id,
            package_id,
            NodeRole::Worker,
            external_network_id,
            fabric_network_id,
            additional_networks,
            cluster_id,
            user_data,
            None, // fabric_ip - not using pre-allocated IPs
            client,
        )
        .await?;
        instances.push(instance);
    }

    Ok(instances)
}

/// Wait for all instances to be running
///
/// Polls all instances in parallel until they reach running state
pub async fn wait_for_all_running(
    instances: &[ProvisionedInstance],
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    use futures_util::future::try_join_all;

    eprintln!("Waiting for {} instances to be running...", instances.len());

    let futures: Vec<_> = instances
        .iter()
        .map(|inst| wait_for_running(inst.instance_id, timeout_secs, client))
        .collect();

    try_join_all(futures).await?;

    eprintln!("All instances are running");

    Ok(())
}

/// Query NICs for all instances
///
/// Fetches NIC data for all instances and updates their NIC configurations
pub async fn query_all_instance_nics(
    instances: &mut [ProvisionedInstance],
    client: &TypedClient,
) -> Result<()> {
    for inst in instances.iter_mut() {
        let nics = query_instance_nics(inst.instance_id, client).await?;
        inst.nics = nics;

        // Update primary_ip if not set
        if inst.primary_ip.is_none()
            && let Some(primary_nic) = inst.nics.iter().find(|n| n.primary)
        {
            inst.primary_ip = Some(primary_nic.ip.clone());
        }
    }

    Ok(())
}

/// Find an external (public) IP from a list of NICs
///
/// Queries the network for each NIC and returns the IP of the first one
/// that belongs to a public network.
pub async fn find_external_ip(nics: &[NicConfig], client: &TypedClient) -> Result<Option<String>> {
    let account = client.effective_account();

    for nic in nics {
        let response = client
            .inner()
            .get_network()
            .account(account)
            .network(nic.network_id.to_string())
            .send()
            .await;

        if let Ok(resp) = response {
            let network = resp.into_inner();
            if network.public {
                return Ok(Some(nic.ip.clone()));
            }
        }
    }

    Ok(None)
}

/// Get default external network
///
/// Queries user config for default network (validating it's public), falls back
/// to first public network
pub async fn get_default_external_network(client: &TypedClient) -> Result<Uuid> {
    let account = client.effective_account();

    // Get all networks first - we'll need this regardless
    let response = client
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await?;

    let networks = response.into_inner();

    // Try to get default network from config
    let config_response = client.inner().get_config().account(account).send().await;

    if let Ok(config) = config_response {
        let config = config.into_inner();
        if let Some(default_network) = config.default_network {
            // Validate the configured default is actually a public network
            if let Some(net) = networks.iter().find(|n| n.id == default_network)
                && net.public
            {
                return Ok(default_network);
            }
            // Configured default is not public or not found - ignore and fall back
        }
    }

    // Fall back to first public network
    for network in networks {
        if network.public {
            return Ok(network.id);
        }
    }

    anyhow::bail!(
        "No public network found. Please set a default network with 'triton network set-default'"
    )
}

/// Resolve image name or UUID to a UUID
///
/// Accepts either:
/// - Full UUID (48324407-fc8a-11ef-be31-db3d2e6f73b1)
/// - Short UUID (48324407)
/// - Image name (talos-1.12-nocloud)
pub async fn resolve_image_id(name_or_uuid: &str, client: &TypedClient) -> Result<Uuid> {
    // Try parsing as full UUID first
    if let Ok(uuid) = Uuid::parse_str(name_or_uuid) {
        return Ok(uuid);
    }

    let account = client.effective_account();

    // List all images and search
    let response = client
        .inner()
        .list_images()
        .account(account)
        .send()
        .await
        .context("Failed to list images")?;

    let images = response.into_inner();

    // Check if it's a short UUID (8 hex chars)
    if name_or_uuid.len() == 8 && name_or_uuid.chars().all(|c| c.is_ascii_hexdigit()) {
        let short_lower = name_or_uuid.to_lowercase();
        let matches: Vec<_> = images
            .into_iter()
            .filter(|img| img.id.to_string().to_lowercase().starts_with(&short_lower))
            .collect();

        match matches.len() {
            0 => bail!("Image not found: {}", name_or_uuid),
            1 => return Ok(matches[0].id),
            _ => {
                let list = matches
                    .iter()
                    .map(|img| format!("  - {} ({}...)", img.name, &img.id.to_string()[..8]))
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "Ambiguous short UUID '{}' matches multiple images:\n{}\n\
                    Use full UUID or image name.",
                    name_or_uuid,
                    list
                )
            }
        }
    }

    // Try matching by name
    let matches: Vec<_> = images
        .into_iter()
        .filter(|img| img.name == name_or_uuid)
        .collect();

    match matches.len() {
        0 => bail!("Image not found: {}", name_or_uuid),
        1 => Ok(matches[0].id),
        _ => {
            let list = matches
                .iter()
                .map(|img| format!("  - {} ({})", img.name, &img.id.to_string()[..8]))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "Multiple images named '{}':\n{}\n\
                Use UUID to specify which image.",
                name_or_uuid,
                list
            )
        }
    }
}

/// Resolve package name or UUID to a UUID
///
/// Accepts either:
/// - Full UUID (a6342267-49ac-4904-bb5e-fe1cdc5f14a7)
/// - Short UUID (a6342267)
/// - Package name (sample-2G)
pub async fn resolve_package_id(name_or_uuid: &str, client: &TypedClient) -> Result<Uuid> {
    // Try parsing as full UUID first
    if let Ok(uuid) = Uuid::parse_str(name_or_uuid) {
        return Ok(uuid);
    }

    let account = client.effective_account();

    // List all packages
    let response = client
        .inner()
        .list_packages()
        .account(account)
        .send()
        .await
        .context("Failed to list packages")?;

    let packages = response.into_inner();

    // Check if it's a short UUID (8 hex chars)
    if name_or_uuid.len() == 8 && name_or_uuid.chars().all(|c| c.is_ascii_hexdigit()) {
        let short_lower = name_or_uuid.to_lowercase();
        let matches: Vec<_> = packages
            .into_iter()
            .filter(|pkg| pkg.id.to_string().to_lowercase().starts_with(&short_lower))
            .collect();

        match matches.len() {
            0 => bail!("Package not found: {}", name_or_uuid),
            1 => return Ok(matches[0].id),
            _ => {
                let list = matches
                    .iter()
                    .map(|pkg| format!("  - {} ({}...)", pkg.name, &pkg.id.to_string()[..8]))
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "Ambiguous short UUID '{}' matches multiple packages:\n{}\n\
                    Use full UUID or package name.",
                    name_or_uuid,
                    list
                )
            }
        }
    }

    // Try matching by name
    let matches: Vec<_> = packages
        .into_iter()
        .filter(|pkg| pkg.name == name_or_uuid)
        .collect();

    match matches.len() {
        0 => bail!("Package not found: {}", name_or_uuid),
        1 => Ok(matches[0].id),
        _ => {
            let list = matches
                .iter()
                .map(|pkg| format!("  - {} ({})", pkg.name, &pkg.id.to_string()[..8]))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "Multiple packages named '{}':\n{}\n\
                Use UUID to specify which package.",
                name_or_uuid,
                list
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nic_config_sorting() {
        let mut nics = vec![
            NicConfig {
                mac: "90:b8:d0:00:00:01".to_string(),
                ip: "192.168.1.2".to_string(),
                netmask: "255.255.255.0".to_string(),
                gateway: None,
                network_id: Uuid::new_v4(),
                primary: false,
            },
            NicConfig {
                mac: "90:b8:d0:00:00:02".to_string(),
                ip: "10.0.0.5".to_string(),
                netmask: "255.255.255.0".to_string(),
                gateway: Some("10.0.0.1".to_string()),
                network_id: Uuid::new_v4(),
                primary: true,
            },
        ];

        nics.sort_by(|a, b| {
            if a.primary != b.primary {
                b.primary.cmp(&a.primary)
            } else {
                a.ip.cmp(&b.ip)
            }
        });

        assert!(nics[0].primary);
        assert_eq!(nics[0].ip, "10.0.0.5");
    }

    #[test]
    fn test_preallocate_fabric_ips_basic() {
        let nodes = preallocate_fabric_ips("10.0.0.0/24", 3, 2, "test-cluster")
            .expect("Failed to allocate IPs");

        assert_eq!(nodes.len(), 5);

        // Control plane nodes come first
        assert_eq!(nodes[0].name, "test-cluster-ctrl-0");
        assert_eq!(nodes[0].role, NodeRole::Control);
        assert_eq!(nodes[0].fabric_ip.to_string(), "10.0.0.10");

        assert_eq!(nodes[1].name, "test-cluster-ctrl-1");
        assert_eq!(nodes[1].fabric_ip.to_string(), "10.0.0.11");

        assert_eq!(nodes[2].name, "test-cluster-ctrl-2");
        assert_eq!(nodes[2].fabric_ip.to_string(), "10.0.0.12");

        // Worker nodes follow
        assert_eq!(nodes[3].name, "test-cluster-worker-0");
        assert_eq!(nodes[3].role, NodeRole::Worker);
        assert_eq!(nodes[3].fabric_ip.to_string(), "10.0.0.13");

        assert_eq!(nodes[4].name, "test-cluster-worker-1");
        assert_eq!(nodes[4].fabric_ip.to_string(), "10.0.0.14");
    }

    #[test]
    fn test_preallocate_fabric_ips_small_subnet() {
        // /30 subnet has only 4 total addresses (network, gateway, 2 hosts)
        // Starting at .10 would exceed the subnet
        let result = preallocate_fabric_ips("10.0.0.0/30", 1, 1, "test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not enough IPs"));
    }

    #[test]
    fn test_preallocate_fabric_ips_invalid_subnet() {
        // Invalid CIDR format
        let result = preallocate_fabric_ips("10.0.0.0", 1, 1, "test");
        assert!(result.is_err());

        // Invalid IP address
        let result = preallocate_fabric_ips("999.0.0.0/24", 1, 1, "test");
        assert!(result.is_err());

        // Invalid prefix length
        let result = preallocate_fabric_ips("10.0.0.0/33", 1, 1, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_preallocate_fabric_ips_different_subnet() {
        let nodes = preallocate_fabric_ips("192.168.100.0/24", 1, 1, "prod")
            .expect("Failed to allocate IPs");

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].fabric_ip.to_string(), "192.168.100.10");
        assert_eq!(nodes[1].fabric_ip.to_string(), "192.168.100.11");
    }

    #[test]
    fn test_preallocate_fabric_ips_zero_nodes() {
        let nodes =
            preallocate_fabric_ips("10.0.0.0/24", 0, 0, "empty").expect("Failed to allocate IPs");
        assert_eq!(nodes.len(), 0);
    }
}
