// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Add worker nodes to an existing cluster

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_client::TypedClient;
use uuid::Uuid;

use crate::commands::k8s::provisioning::{
    get_default_external_network, provision_additional_workers, query_all_instance_nics,
    resolve_package_id, wait_for_all_running,
};
use crate::commands::k8s::state::{ClusterState, NodeInfo, NodeRole};

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

    // 12. Update cluster state
    eprintln!("==> Updating cluster state");

    // Add new nodes to the nodes map
    for inst in &worker_instances {
        let fabric_ip = inst.nics.iter().find(|n| !n.primary).map(|n| n.ip.clone());

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

    // Update last_fabric_ip_offset if we're using fabric networking
    if state.fabric_network_id.is_some() {
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
            .find(|n| !n.primary)
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
