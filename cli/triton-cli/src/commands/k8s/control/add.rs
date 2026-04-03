// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Add control plane nodes to an existing cluster for HA

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_api::types::network::Nic;
use cloudapi_client::TypedClient;

use crate::commands::k8s::network::generate_network_patch;
use crate::commands::k8s::provisioning::{
    find_external_ip, get_default_external_network, provision_additional_control_plane,
    query_all_instance_nics, resolve_package_id, wait_for_all_running,
};
use crate::commands::k8s::state::{ClusterState, NodeInfo, NodeRole};
use crate::commands::k8s::talos;

#[derive(Args, Clone)]
pub struct AddArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Number of control plane nodes to add
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Package for control plane nodes (defaults to cluster's control package)
    #[arg(long)]
    pub package: Option<String>,
}

pub async fn run(args: AddArgs, client: &TypedClient, _use_json: bool) -> Result<()> {
    eprintln!("==> Loading cluster state");

    // 1. Load cluster state
    let mut state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    eprintln!("    Cluster: {} ({})", state.name, state.uuid);

    // 2. Validate cluster has been bootstrapped
    let control_config = state
        .control_plane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cluster has not been bootstrapped yet"))?;

    // 3. Load existing controlplane.yaml from cluster directory
    let cluster_dir = state.cluster_dir()?;
    let controlplane_yaml_path = cluster_dir.join("controlplane.yaml");
    let talosconfig_path = cluster_dir.join("talosconfig");

    if !controlplane_yaml_path.exists() {
        anyhow::bail!(
            "Control plane config not found at {}. Has the cluster been bootstrapped?",
            controlplane_yaml_path.display()
        );
    }

    let controlplane_config_yaml = tokio::fs::read_to_string(&controlplane_yaml_path)
        .await
        .context("Failed to read controlplane.yaml")?;

    eprintln!(
        "    Loaded control plane config from: {}",
        controlplane_yaml_path.display()
    );

    // 4. Determine next control plane node number by scanning existing nodes
    let existing_ctrl_indices: Vec<u32> = state
        .nodes
        .keys()
        .filter_map(|name| {
            // Parse names like "cluster-ctrl-0", "cluster-ctrl-1"
            let prefix = format!("{}-ctrl-", state.name);
            if name.starts_with(&prefix) {
                name[prefix.len()..].parse::<u32>().ok()
            } else {
                None
            }
        })
        .collect();

    let next_ctrl_index = existing_ctrl_indices.iter().max().map_or(0, |&max| max + 1);
    eprintln!(
        "    Existing control nodes: {}, next index: {}",
        existing_ctrl_indices.len(),
        next_ctrl_index
    );

    // 5. Resolve package (use stored or override)
    let package_id = if let Some(ref pkg_override) = args.package {
        eprintln!("==> Resolving package override '{}'", pkg_override);
        resolve_package_id(pkg_override, client)
            .await
            .context("Failed to resolve package")?
    } else if let Some(stored_id) = control_config.package_id {
        eprintln!(
            "    Using stored control package: {}",
            &stored_id.to_string()[..8]
        );
        stored_id
    } else {
        // Fall back to resolving the package name from config
        eprintln!("==> Resolving control package '{}'", control_config.package);
        resolve_package_id(&control_config.package, client)
            .await
            .context("Failed to resolve control package")?
    };

    // 6. Get image from stored state
    let image_id = control_config.image_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Control plane image ID not stored in cluster state. \
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

    // 8. Provision new control plane instance(s)
    eprintln!("==> Provisioning {} control plane node(s)", args.count);

    let mut control_instances = provision_additional_control_plane(
        args.count,
        next_ctrl_index,
        &image_id.to_string(),
        &package_id.to_string(),
        external_network_id,
        state.fabric_network_id,
        state.uuid,
        &state.name,
        Some(&controlplane_config_yaml),
        client,
    )
    .await
    .context("Failed to provision control plane nodes")?;

    // 9. Wait for instances to be running
    eprintln!("==> Waiting for control plane nodes to be running");
    wait_for_all_running(&control_instances, 300, client)
        .await
        .context("Failed waiting for control plane nodes to be running")?;

    // 10. Query NIC data
    eprintln!("==> Querying control plane NIC data");
    query_all_instance_nics(&mut control_instances, client)
        .await
        .context("Failed to query control plane NICs")?;

    // 11. Generate network patches for each new control plane node
    eprintln!("==> Generating network patches");

    // Get nameservers from existing control plane nodes or use defaults
    let nameservers: Vec<String> = if let Some(fabric_id) = state.fabric_network_id {
        // Query fabric network for resolvers
        use crate::commands::k8s::provisioning::discover_fabric_network;
        match discover_fabric_network(fabric_id, client).await {
            Ok(info) => info.resolvers,
            Err(_) => vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
        }
    } else {
        vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    };

    for inst in &control_instances {
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

        let patch_yaml = generate_network_patch(&nics, &nameservers, true)
            .with_context(|| format!("Failed to generate network patch for {}", inst.name))?;

        let patch_path = cluster_dir.join(format!("{}-network-patch.yaml", inst.name));
        tokio::fs::write(&patch_path, patch_yaml)
            .await
            .with_context(|| format!("Failed to write patch to {}", patch_path.display()))?;

        eprintln!("    Generated: {}", patch_path.display());
    }

    // 12. Apply network patches to the new control plane nodes
    eprintln!("==> Applying network patches to new control plane nodes");

    let talosconfig_str = talosconfig_path.to_string_lossy().to_string();

    for inst in &control_instances {
        let patch_path = cluster_dir.join(format!("{}-network-patch.yaml", inst.name));

        // Get the node's external IP
        let target_ip = match find_external_ip(&inst.nics, client).await? {
            Some(ip) => ip,
            None => {
                if let Some(primary_ip) = &inst.primary_ip {
                    primary_ip.clone()
                } else {
                    anyhow::bail!("Instance {} has no accessible IP", inst.name);
                }
            }
        };

        eprintln!("    Applying config to {} ({})", inst.name, target_ip);

        // Apply configuration using native gRPC
        talos::apply_config::run(
            &target_ip,
            &controlplane_yaml_path,
            &[patch_path.as_ref()],
            Some(&talosconfig_str),
            true,  // do_retry
            false, // verbose
        )
        .await
        .with_context(|| format!("Failed to apply config to {}", inst.name))?;

        eprintln!("    Applied config to {} ({})", inst.name, target_ip);
    }

    // 13. Update cluster state
    eprintln!("==> Updating cluster state");

    // Add new nodes to the nodes map
    for inst in &control_instances {
        let fabric_ip = inst.nics.iter().find(|n| !n.primary).map(|n| n.ip.clone());

        state.nodes.insert(
            inst.name.clone(),
            NodeInfo {
                instance_id: inst.instance_id,
                primary_ip: inst.primary_ip.clone(),
                fabric_ip,
                role: NodeRole::Control,
            },
        );
    }

    // 14. Save cluster state
    state.save().await.context("Failed to save cluster state")?;

    // Print summary
    eprintln!();
    eprintln!(
        "==> Successfully added {} control plane node(s) to cluster {}",
        args.count, state.name
    );
    eprintln!();

    for inst in &control_instances {
        let external_ip = inst.primary_ip.as_deref().unwrap_or("(none)");
        let fabric_ip = inst
            .nics
            .iter()
            .find(|n| !n.primary)
            .map(|n| n.ip.as_str())
            .unwrap_or("(none)");

        eprintln!(
            "    {} ({}) - external: {}, fabric: {}",
            inst.name,
            &inst.instance_id.to_string()[..8],
            external_ip,
            fabric_ip
        );
    }

    eprintln!();
    eprintln!("New control plane nodes will automatically join the etcd cluster.");
    eprintln!("Check cluster status with:");
    eprintln!(
        "  talosctl --talosconfig {} etcd members",
        talosconfig_path.display()
    );
    eprintln!("  kubectl get nodes");

    Ok(())
}
