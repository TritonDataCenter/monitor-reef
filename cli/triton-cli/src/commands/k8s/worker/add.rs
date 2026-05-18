// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Add worker nodes to an existing cluster

use anyhow::{Context, Result, bail};
use clap::Args;
use cloudapi_api::types::network::Nic;
use cloudapi_client::TypedClient;
use std::net::IpAddr;
use uuid::Uuid;

use crate::commands::k8s::network::generate_network_patch;
use crate::commands::k8s::provisioning::{
    discover_fabric_network, get_default_external_network, provision_additional_workers,
    query_all_instance_nics, resolve_package_id, validate_ip_in_subnet, wait_for_all_running,
};
use crate::commands::k8s::state::{ClusterState, NodeInfo, NodeRole};
use crate::commands::k8s::talos;

#[derive(Args, Clone)]
pub struct AddArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Number of workers to add
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Package for worker nodes (defaults to cluster's worker package)
    #[arg(long)]
    pub package: Option<String>,

    /// Additional network IDs for workers
    #[arg(long)]
    pub network: Vec<String>,

    /// Explicit fabric IP addresses for new worker nodes (comma-separated).
    ///
    /// Must provide exactly --count IPs. When specified, these IPs
    /// are requested from Triton instead of relying on DHCP assignment.
    /// IPs must be within the fabric subnet range.
    #[arg(long, value_delimiter = ',')]
    pub fabric_ip: Option<Vec<IpAddr>>,
}

pub async fn run(args: AddArgs, client: &TypedClient, _use_json: bool) -> Result<()> {
    eprintln!("==> Loading cluster state");

    // 1. Load cluster state
    let mut state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    eprintln!("    Cluster: {} ({})", state.name, state.uuid);

    // 2. Validate cluster has been bootstrapped
    let worker_config = state
        .workers
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cluster has not been bootstrapped yet"))?;

    // 3. Load existing worker.yaml from cluster directory
    let cluster_dir = state.cluster_dir()?;
    let worker_yaml_path = cluster_dir.join("worker.yaml");

    if !worker_yaml_path.exists() {
        anyhow::bail!(
            "Worker config not found at {}. Has the cluster been bootstrapped?",
            worker_yaml_path.display()
        );
    }

    let worker_config_yaml = tokio::fs::read_to_string(&worker_yaml_path)
        .await
        .context("Failed to read worker.yaml")?;

    eprintln!(
        "    Loaded worker config from: {}",
        worker_yaml_path.display()
    );

    // 4. Determine next worker number by scanning existing nodes
    let existing_worker_indices: Vec<u32> = state
        .nodes
        .keys()
        .filter_map(|name| {
            // Parse names like "cluster-worker-0", "cluster-worker-1"
            let prefix = format!("{}-worker-", state.name);
            if name.starts_with(&prefix) {
                name[prefix.len()..].parse::<u32>().ok()
            } else {
                None
            }
        })
        .collect();

    let next_worker_index = existing_worker_indices
        .iter()
        .max()
        .map_or(0, |&max| max + 1);
    eprintln!(
        "    Existing workers: {}, next index: {}",
        existing_worker_indices.len(),
        next_worker_index
    );

    // 4b. Validate explicit fabric IPs if provided
    if let Some(ref ips) = args.fabric_ip {
        if state.fabric_network_id.is_none() {
            bail!("Cannot specify --fabric-ip without a fabric network configured");
        }
        if ips.len() as u32 != args.count {
            bail!(
                "--fabric-ip: expected {} IPs (matching --count), got {}",
                args.count,
                ips.len()
            );
        }
        // Validate IPs are within the fabric subnet
        if let Some(fabric_id) = state.fabric_network_id {
            let fabric = discover_fabric_network(fabric_id, client)
                .await
                .context("Failed to discover fabric network for IP validation")?;
            for ip in ips {
                validate_ip_in_subnet(ip, &fabric.subnet)?;
            }
        }
    }

    // 5. Resolve package (use stored or override)
    let package_id = if let Some(ref pkg_override) = args.package {
        eprintln!("==> Resolving package override '{}'", pkg_override);
        resolve_package_id(pkg_override, client)
            .await
            .context("Failed to resolve package")?
    } else if let Some(stored_id) = worker_config.package_id {
        eprintln!(
            "    Using stored worker package: {}",
            &stored_id.to_string()[..8]
        );
        stored_id
    } else {
        // Fall back to resolving the package name from config
        eprintln!("==> Resolving worker package '{}'", worker_config.package);
        resolve_package_id(&worker_config.package, client)
            .await
            .context("Failed to resolve worker package")?
    };

    // 6. Get image from stored state
    let image_id = worker_config.image_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Worker image ID not stored in cluster state. \
                 This cluster may have been created with an older version. \
                 Please re-bootstrap the cluster or manually specify the image."
        )
    })?;

    eprintln!("    Using image: {}", &image_id.to_string()[..8]);

    // 7. Get default external network
    eprintln!("==> Finding default external network");
    let external_network_id = get_default_external_network(client)
        .await
        .context("Failed to get default external network")?;

    // 8. Parse additional networks
    let additional_networks: Result<Vec<Uuid>> = args
        .network
        .iter()
        .map(|s| {
            s.parse::<Uuid>()
                .with_context(|| format!("Invalid network UUID: {}", s))
        })
        .collect();
    let additional_networks = additional_networks?;

    if !additional_networks.is_empty() {
        eprintln!(
            "    Additional networks: {}",
            additional_networks
                .iter()
                .map(|id| id.to_string()[..8].to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // 9. Provision new worker instance(s)
    eprintln!("==> Provisioning {} worker node(s)", args.count);

    let mut worker_instances = provision_additional_workers(
        args.count,
        next_worker_index,
        &image_id.to_string(),
        &package_id.to_string(),
        state.fabric_network_id,
        &additional_networks,
        state.uuid,
        &state.name,
        external_network_id,
        Some(&worker_config_yaml),
        args.fabric_ip.as_deref(),
        client,
    )
    .await
    .context("Failed to provision workers")?;

    // 10. Wait for instances to be running
    eprintln!("==> Waiting for workers to be running");
    wait_for_all_running(&worker_instances, 300, client)
        .await
        .context("Failed waiting for workers to be running")?;

    // 11. Query NIC data
    eprintln!("==> Querying worker NIC data");
    query_all_instance_nics(&mut worker_instances, client)
        .await
        .context("Failed to query worker NICs")?;

    // 11b. Generate and apply network patches to persist networking across upgrades.
    // Workers are fabric-only, so we route through the control plane using the Talos
    // proxy mechanism (nodes header). Without this, workers lose networking after
    // Talos upgrades because the STATE partition has no network config.
    let talosconfig_path = cluster_dir.join("talosconfig");
    let talosconfig_str = talosconfig_path.to_string_lossy().to_string();

    // Find control plane endpoint for proxying
    let control_plane_endpoint = state
        .nodes
        .iter()
        .find(|(_, i)| i.role == NodeRole::Control)
        .and_then(|(_, i)| i.primary_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("No control plane node found for proxying"))?;

    // Get nameservers from fabric network resolvers, fall back to Google DNS
    let nameservers: Vec<String> = if let Some(fabric_id) = state.fabric_network_id {
        match discover_fabric_network(fabric_id, client).await {
            Ok(info) if !info.resolvers.is_empty() => info.resolvers,
            _ => vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
        }
    } else {
        vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    };

    eprintln!(
        "==> Generating and applying network patches (via {})",
        control_plane_endpoint
    );

    for inst in &worker_instances {
        // Convert NicConfig to cloudapi Nic type for network module
        let nics: Vec<Nic> = inst
            .nics
            .iter()
            .map(|n| Nic {
                mac: n.mac.clone(),
                primary: n.primary,
                ip: n.ip.clone(),
                netmask: n.netmask.clone(),
                gateway: n.gateway.clone(),
                network: n.network_id,
                state: None,
            })
            .collect();

        let patch_yaml = generate_network_patch(
            &nics,
            &nameservers,
            false, // is_control_plane
            state.fabric_network_id,
            Some(&inst.name),
        )
        .with_context(|| format!("Failed to generate network patch for {}", inst.name))?;

        let patch_path = cluster_dir.join(format!("{}-network-patch.yaml", inst.name));
        tokio::fs::write(&patch_path, &patch_yaml)
            .await
            .with_context(|| format!("Failed to write patch to {}", patch_path.display()))?;

        // Get the worker's fabric IP (the target node for routing)
        let worker_fabric_ip = inst
            .primary_ip
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Worker {} has no primary IP", inst.name))?;

        eprintln!(
            "    Applying config to {} ({} via {})",
            inst.name, worker_fabric_ip, control_plane_endpoint
        );

        if let Err(e) = talos::apply_config::run_via(
            &control_plane_endpoint,
            Some(worker_fabric_ip),
            &worker_yaml_path,
            &[patch_path.as_ref()],
            Some(&talosconfig_str),
            true,  // do_retry
            false, // verbose
        )
        .await
        {
            // Don't fail the add for worker config errors - they can be
            // fixed later with manual config application
            eprintln!(
                "    WARNING: Failed to apply config to {} ({}): {}",
                inst.name, worker_fabric_ip, e
            );
            eprintln!("    Workers may lose networking after Talos upgrades.");
            continue;
        }

        eprintln!(
            "    Applied config to {} ({} via {})",
            inst.name, worker_fabric_ip, control_plane_endpoint
        );
    }

    // 12. Update cluster state
    eprintln!("==> Updating cluster state");

    // Add new nodes to the nodes map
    for inst in &worker_instances {
        // Workers may have the fabric NIC as their primary (fabric-only workers),
        // so also check the primary NIC against the fabric network ID.
        let fabric_ip = inst
            .nics
            .iter()
            .find(|n| {
                !n.primary
                    || state
                        .fabric_network_id
                        .is_some_and(|fid| fid == n.network_id)
            })
            .map(|n| n.ip.clone());

        state.nodes.insert(
            inst.name.clone(),
            NodeInfo {
                instance_id: inst.instance_id,
                primary_ip: inst.primary_ip.clone(),
                fabric_ip,
                role: NodeRole::Worker,
            },
        );
    }

    // Update last_fabric_ip_offset if we're using fabric networking with
    // auto-allocation. Explicit IPs may be non-sequential, so skip the update.
    if state.fabric_network_id.is_some() && args.fabric_ip.is_none() {
        let current_offset = state.last_fabric_ip_offset.unwrap_or(10);
        state.last_fabric_ip_offset = Some(current_offset + args.count);
    }

    // 13. Save cluster state
    state.save().await.context("Failed to save cluster state")?;

    // Print summary
    eprintln!();
    eprintln!(
        "==> Successfully added {} worker(s) to cluster {}",
        args.count, state.name
    );
    eprintln!();

    for inst in &worker_instances {
        let fabric_ip = inst
            .nics
            .iter()
            .find(|n| {
                !n.primary
                    || state
                        .fabric_network_id
                        .is_some_and(|fid| fid == n.network_id)
            })
            .map(|n| n.ip.as_str())
            .unwrap_or("(none)");

        eprintln!(
            "    {} ({}) - fabric IP: {}",
            inst.name,
            &inst.instance_id.to_string()[..8],
            fabric_ip
        );
    }

    eprintln!();
    eprintln!("Workers will automatically join the cluster via Talos.");
    eprintln!("Check cluster status with: kubectl get nodes");

    Ok(())
}
