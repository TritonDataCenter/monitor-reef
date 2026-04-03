// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Bootstrap cluster nodes

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_api::types::network::Nic;
use cloudapi_client::TypedClient;
use std::collections::HashMap;
use uuid::Uuid;

use super::logging::LogWriter;
use super::network::generate_network_patch;
use super::provisioning::{
    create_firewall_rules, detect_user_ip, discover_fabric_network, find_external_ip,
    get_default_external_network, preallocate_fabric_ips, provision_control_plane,
    provision_workers, query_all_instance_nics, resolve_image_id, resolve_package_id,
    wait_for_all_running,
};
use super::state::{ClusterState, ControlPlaneConfig, NodeInfo, NodeRole, WorkerConfig};
use super::talos;
use super::talos::config::{SecretsBundle, generate_machine_configs};

#[derive(Args, Clone)]
pub struct BootstrapArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Number of control plane nodes
    #[arg(long, default_value = "1")]
    pub control_nodes: u32,

    /// Package for control plane nodes
    #[arg(long)]
    pub control_package: String,

    /// Number of worker nodes
    #[arg(long, default_value = "2")]
    pub worker_nodes: u32,

    /// Package for worker nodes
    #[arg(long)]
    pub worker_package: String,

    /// Additional worker network IDs
    #[arg(long)]
    pub worker_network: Vec<String>,

    /// Talos image name
    #[arg(long, default_value = "talos-1.12-nocloud")]
    pub image: String,

    /// Talos version for config generation
    #[arg(long, default_value = "1.6.0")]
    pub talos_version: String,

    /// IP addresses to allow through firewall for management access.
    ///
    /// Use "auto" to detect your external IP automatically, or provide
    /// a comma-separated list of IPs (e.g. "192.168.1.100" or
    /// "192.168.1.100,10.0.0.50"). Defaults to "auto".
    #[arg(long, default_value = "auto")]
    pub firewall_allow: String,
}

pub async fn run(args: BootstrapArgs, client: &TypedClient, _use_json: bool) -> Result<()> {
    eprintln!("==> Loading cluster state");

    // 1. Load cluster state (by name or UUID)
    let mut state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    eprintln!("    Cluster: {} ({})", state.name, state.uuid);

    let cluster_dir = state.cluster_dir()?;

    // Initialize logging for this bootstrap operation
    let logger = LogWriter::new(state.uuid, "bootstrap")
        .await
        .context("Failed to initialize logging")?;

    logger.info(format!(
        "Starting bootstrap for cluster {} ({})",
        state.name, state.uuid
    ));
    logger.info(format!(
        "Control nodes: {}, Worker nodes: {}",
        args.control_nodes, args.worker_nodes
    ));
    logger.info(format!(
        "Control package: {}, Worker package: {}",
        args.control_package, args.worker_package
    ));
    logger.info(format!(
        "Image: {}, Talos version: {}",
        args.image, args.talos_version
    ));
    logger.flush().await?;

    // 2. Discover/validate fabric network (if configured)
    // This must happen BEFORE config generation so we can pre-allocate fabric IPs
    // and use the control plane's fabric IP as the Kubernetes API endpoint.
    let fabric_info = if let Some(fabric_id) = state.fabric_network_id {
        eprintln!("==> Discovering fabric network");
        logger.info(format!("Discovering fabric network {}", fabric_id));
        let info = discover_fabric_network(fabric_id, client)
            .await
            .context("Failed to discover fabric network")?;
        eprintln!(
            "    Network: {} ({})",
            info.name,
            &fabric_id.to_string()[..8]
        );
        eprintln!("    Subnet:  {}", info.subnet);
        eprintln!(
            "    Gateway: {}",
            info.gateway.as_deref().unwrap_or("(none)")
        );
        eprintln!(
            "    DNS:     {}",
            if info.resolvers.is_empty() {
                "(none)".to_string()
            } else {
                info.resolvers.join(", ")
            }
        );
        logger.info(format!(
            "Fabric network: {} ({}), subnet: {}, gateway: {}, DNS: {}",
            info.name,
            &fabric_id.to_string()[..8],
            info.subnet,
            info.gateway.as_deref().unwrap_or("(none)"),
            if info.resolvers.is_empty() {
                "(none)".to_string()
            } else {
                info.resolvers.join(", ")
            }
        ));
        Some(info)
    } else {
        eprintln!("==> No fabric network configured (external-only mode)");
        logger.info("No fabric network configured (external-only mode)");
        None
    };

    // 3. Pre-allocate fabric IPs for all nodes
    // The first control plane's fabric IP will be used as the Kubernetes API endpoint.
    // This IP is reachable by all nodes on the fabric network.
    let preallocated_nodes = if let Some(ref fabric) = fabric_info {
        eprintln!("==> Pre-allocating fabric IPs");
        let nodes = preallocate_fabric_ips(
            &fabric.subnet,
            args.control_nodes,
            args.worker_nodes,
            &state.name,
        )
        .context("Failed to pre-allocate fabric IPs")?;

        for node in &nodes {
            eprintln!("    {}: {}", node.name, node.fabric_ip);
        }
        logger.info(format!(
            "Pre-allocated {} fabric IPs starting from {}",
            nodes.len(),
            nodes
                .first()
                .map(|n| n.fabric_ip.to_string())
                .unwrap_or_default()
        ));
        nodes
    } else {
        Vec::new()
    };

    // Determine the control plane endpoint IP.
    // When using a fabric network, use the first control plane's fabric IP.
    // This is the IP that workers will use to reach the Kubernetes API.
    let control_endpoint_ip = preallocated_nodes
        .iter()
        .find(|n| n.role == NodeRole::Control)
        .map(|n| n.fabric_ip.to_string());

    // 4. Generate Talos secrets
    eprintln!("==> Generating Talos secrets");
    logger.info("Generating Talos secrets");
    let secrets =
        talos::config::SecretsBundle::generate().context("Failed to generate Talos secrets")?;

    let secrets_path = cluster_dir.join("secrets.yaml");
    secrets
        .save(&secrets_path)
        .await
        .context("Failed to save secrets")?;
    eprintln!("    Saved to: {}", secrets_path.display());
    logger.info(format!("Saved secrets to: {}", secrets_path.display()));

    // 5. Generate base control plane and worker configs using talosctl
    // The endpoint IP is the control plane's fabric IP (or a placeholder if no fabric).
    // Workers will connect to the Kubernetes API via this fabric IP.
    eprintln!("==> Generating base Talos configs");
    logger.info("Generating base Talos configs");

    // Use fabric IP as endpoint if available, otherwise use placeholder
    // (external-only mode would need a different approach)
    let endpoint_ip = control_endpoint_ip.as_deref().unwrap_or("192.0.2.1"); // RFC 5737 TEST-NET-1 fallback

    eprintln!("    Control plane endpoint: {}", endpoint_ip);
    logger.info(format!("Control plane endpoint: {}", endpoint_ip));

    let controlplane_yaml = cluster_dir.join("controlplane.yaml");
    let worker_yaml = cluster_dir.join("worker.yaml");
    let talosconfig_path = cluster_dir.join("talosconfig");

    generate_talos_configs(
        &state.name,
        endpoint_ip,
        &secrets_path,
        &cluster_dir,
        &logger,
    )
    .await
    .context("Failed to generate Talos configs")?;

    eprintln!(
        "    Generated: {}, {}, {}",
        controlplane_yaml.display(),
        worker_yaml.display(),
        talosconfig_path.display()
    );
    logger.info(format!(
        "Generated configs: {}, {}, {}",
        controlplane_yaml.display(),
        worker_yaml.display(),
        talosconfig_path.display()
    ));

    // Read the generated configs for cloud-init user-data
    // These will be passed to instances at boot time so Talos has its configuration
    let controlplane_config = tokio::fs::read_to_string(&controlplane_yaml)
        .await
        .context("Failed to read controlplane.yaml")?;
    let worker_config = tokio::fs::read_to_string(&worker_yaml)
        .await
        .context("Failed to read worker.yaml")?;

    // 6. Get default external network
    eprintln!("==> Finding default external network");
    let external_network_id = get_default_external_network(client)
        .await
        .context("Failed to get default external network")?;
    eprintln!(
        "    External network: {}",
        &external_network_id.to_string()[..8]
    );

    // Parse additional worker networks
    let worker_networks: Result<Vec<Uuid>> = args
        .worker_network
        .iter()
        .map(|s| {
            s.parse::<Uuid>()
                .with_context(|| format!("Invalid worker network UUID: {}", s))
        })
        .collect();
    let worker_networks = worker_networks?;

    // Resolve image name to UUID
    eprintln!("==> Resolving image '{}'", args.image);
    let image_id = resolve_image_id(&args.image, client)
        .await
        .context("Failed to resolve image")?;
    eprintln!("    Image: {} ({})", args.image, &image_id.to_string()[..8]);

    // Resolve control package name to UUID
    eprintln!(
        "==> Resolving control plane package '{}'",
        args.control_package
    );
    let control_package_id = resolve_package_id(&args.control_package, client)
        .await
        .context("Failed to resolve control package")?;
    eprintln!(
        "    Package: {} ({})",
        args.control_package,
        &control_package_id.to_string()[..8]
    );

    // Resolve worker package name to UUID
    eprintln!("==> Resolving worker package '{}'", args.worker_package);
    let worker_package_id = resolve_package_id(&args.worker_package, client)
        .await
        .context("Failed to resolve worker package")?;
    eprintln!(
        "    Package: {} ({})",
        args.worker_package,
        &worker_package_id.to_string()[..8]
    );

    // 6. Provision control plane instance(s) FIRST
    // We need the control plane running to get its actual fabric IP before provisioning workers.
    // Pass the controlplane config as cloud-init user-data so Talos boots with its config.
    eprintln!(
        "==> Provisioning {} control plane node(s)",
        args.control_nodes
    );
    let mut control_instances = provision_control_plane(
        args.control_nodes,
        &image_id.to_string(),
        &control_package_id.to_string(),
        external_network_id,
        state.fabric_network_id,
        state.uuid,
        &state.name,
        Some(&controlplane_config),
        client,
    )
    .await
    .context("Failed to provision control plane")?;

    // 7. Wait for control plane to be running and query its NIC data
    // We need the actual fabric IP before provisioning workers.
    eprintln!("==> Waiting for control plane to be running");
    wait_for_all_running(&control_instances, 300, client)
        .await
        .context("Failed waiting for control plane to be running")?;

    eprintln!("==> Querying control plane NIC data");
    query_all_instance_nics(&mut control_instances, client)
        .await
        .context("Failed to query control plane NICs")?;

    // 8. Get the actual control plane fabric IP and update worker config
    // The worker.yaml was generated with a pre-allocated IP that may not match
    // the actual IP assigned by Triton's DHCP. We need to fix this.
    let actual_control_fabric_ip = if state.fabric_network_id.is_some() {
        control_instances[0]
            .nics
            .iter()
            .find(|n| !n.primary) // Fabric NIC is not primary (external is primary)
            .map(|n| n.ip.clone())
    } else {
        None
    };

    // Update worker config with actual fabric IP if different from pre-allocated
    let worker_config = if let (Some(actual_ip), Some(preallocated_ip)) =
        (&actual_control_fabric_ip, &control_endpoint_ip)
    {
        if actual_ip != preallocated_ip {
            eprintln!(
                "==> Updating worker config endpoint: {} -> {}",
                preallocated_ip, actual_ip
            );
            logger.info(format!(
                "Updated worker config endpoint from {} to {}",
                preallocated_ip, actual_ip
            ));
            // Replace the pre-allocated endpoint with the actual fabric IP
            worker_config.replace(
                &format!("endpoint: https://{}:6443", preallocated_ip),
                &format!("endpoint: https://{}:6443", actual_ip),
            )
        } else {
            worker_config
        }
    } else {
        worker_config
    };

    // 9. Provision worker instances with corrected config
    // Pass the worker config as cloud-init user-data so Talos boots with its config
    eprintln!("==> Provisioning {} worker node(s)", args.worker_nodes);
    let mut worker_instances = provision_workers(
        args.worker_nodes,
        &image_id.to_string(),
        &worker_package_id.to_string(),
        state.fabric_network_id,
        &worker_networks,
        state.uuid,
        &state.name,
        external_network_id,
        Some(&worker_config),
        client,
    )
    .await
    .context("Failed to provision workers")?;

    // 10. Wait for workers to be running
    eprintln!("==> Waiting for workers to be running");
    wait_for_all_running(&worker_instances, 300, client)
        .await
        .context("Failed waiting for workers to be running")?;

    // 11. Query NIC data for workers
    eprintln!("==> Querying worker NIC data");
    query_all_instance_nics(&mut worker_instances, client)
        .await
        .context("Failed to query worker NICs")?;

    // Create all_instances list with both control and worker nodes
    let mut all_instances = Vec::new();
    all_instances.extend(control_instances.iter().cloned());
    all_instances.extend(worker_instances.iter().cloned());

    // Determine control plane endpoint (primary IP of first control node)
    let control_endpoint = control_instances[0]
        .primary_ip
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Control plane node has no primary IP"))?;

    eprintln!("    Control plane endpoint: {}", control_endpoint);

    // 10. Generate per-node network patches
    eprintln!("==> Generating network patches for all nodes");

    let nameservers = fabric_info
        .as_ref()
        .map(|f| f.resolvers.clone())
        .unwrap_or_default();

    for inst in &all_instances {
        let is_control = control_instances
            .iter()
            .any(|ci| ci.instance_id == inst.instance_id);

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

        let patch_yaml = generate_network_patch(&nics, &nameservers, is_control)
            .with_context(|| format!("Failed to generate network patch for {}", inst.name))?;

        let patch_path = cluster_dir.join(format!("{}-network-patch.yaml", inst.name));
        tokio::fs::write(&patch_path, patch_yaml)
            .await
            .with_context(|| format!("Failed to write patch to {}", patch_path.display()))?;

        eprintln!("    Generated: {}", patch_path.display());
    }

    // 11. Create firewall rules (must be done before applying configs)
    eprintln!("==> Creating firewall rules");

    // Parse --firewall-allow: "auto" means detect, otherwise comma-separated IPs
    let user_ips: Vec<String> = if args.firewall_allow.eq_ignore_ascii_case("auto") {
        match detect_user_ip().await {
            Some(ip) => {
                eprintln!("    Detected user IP: {}", ip);
                vec![ip]
            }
            None => {
                eprintln!("    WARNING: Could not detect user IP, skipping user access rules");
                vec![]
            }
        }
    } else {
        let ips: Vec<String> = args
            .firewall_allow
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for ip in &ips {
            eprintln!("    User IP: {}", ip);
        }
        ips
    };

    let _rule_ids = create_firewall_rules(
        state.uuid,
        "control",
        state.fabric_network_id,
        &user_ips,
        client,
    )
    .await
    .context("Failed to create firewall rules")?;

    // Allow firewall rules to propagate to compute nodes
    eprintln!("    Waiting for firewall rules to propagate...");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // 12. Create endpoint patch if actual IP differs from pre-allocated
    // This patches the cluster.controlPlane.endpoint to use the actual fabric IP.
    let endpoint_patch_path = if let (Some(actual_ip), Some(preallocated_ip)) =
        (&actual_control_fabric_ip, &control_endpoint_ip)
    {
        if actual_ip != preallocated_ip {
            let endpoint_patch = format!(
                "cluster:\n  controlPlane:\n    endpoint: https://{}:6443\n",
                actual_ip
            );
            let path = cluster_dir.join("endpoint-patch.yaml");
            tokio::fs::write(&path, endpoint_patch)
                .await
                .context("Failed to write endpoint patch")?;
            eprintln!(
                "==> Generated endpoint patch: {} -> {}",
                preallocated_ip, actual_ip
            );
            logger.info(format!(
                "Generated endpoint patch: {} -> {}",
                preallocated_ip, actual_ip
            ));
            Some(path)
        } else {
            None
        }
    } else {
        None
    };

    // 13. Apply network persistence patches to control plane nodes only
    // Workers are fabric-only (no external NIC) and cannot be reached from outside Triton.
    // They already have complete network config from cloud-init, so no post-boot patching needed.
    eprintln!("==> Applying network patches to control plane nodes");
    eprintln!("    (Workers have complete config from cloud-init, skipping)");
    logger.info("Applying network patches to control plane nodes only");
    logger.info("Workers skipped - they have complete config from cloud-init");
    logger.flush().await?;

    let talosconfig_str = talosconfig_path.to_string_lossy().to_string();

    for inst in &control_instances {
        let patch_path = cluster_dir.join(format!("{}-network-patch.yaml", inst.name));

        // Get the node's IP to apply the config to
        // Use external (public) IP so we can reach the node from outside Triton
        let target_ip = match find_external_ip(&inst.nics, client).await? {
            Some(ip) => ip,
            None => {
                // Fall back to primary IP if no external IP found
                if let Some(primary_ip) = &inst.primary_ip {
                    primary_ip.clone()
                } else {
                    logger.error(format!("Instance {} has no accessible IP", inst.name));
                    logger.flush().await?;
                    anyhow::bail!("Instance {} has no accessible IP", inst.name);
                }
            }
        };

        logger.info(format!("Applying config to {} ({})", inst.name, target_ip));

        // Build list of patch files
        let mut patches: Vec<&std::path::Path> = vec![patch_path.as_ref()];
        if let Some(ref ep_patch) = endpoint_patch_path {
            patches.push(ep_patch.as_ref());
        }

        // Use native gRPC apply config instead of shelling out to talosctl
        if let Err(e) = talos::apply_config::run(
            &target_ip,
            &controlplane_yaml,
            &patches,
            Some(&talosconfig_str),
            true,  // do_retry
            false, // verbose
        )
        .await
        {
            logger.error(format!("Failed to apply config to {}: {}", inst.name, e));
            logger.flush().await?;
            logger.create_latest_symlink().await?;

            // Print log file location for debugging
            if let Some(log_path) = logger.log_file_path() {
                eprintln!();
                eprintln!("    Logs saved to: {}", log_path.display());
            }

            return Err(e).with_context(|| format!("Failed to apply config to {}", inst.name));
        }

        logger.info(format!(
            "Successfully applied config to {} ({})",
            inst.name, target_ip
        ));
        eprintln!("    Applied config to {} ({})", inst.name, target_ip);
    }

    // 13. Bootstrap etcd on first control node
    eprintln!("==> Bootstrapping etcd on control plane");

    // Use external IP for talosctl commands (reachable from outside Triton)
    let control_endpoint_for_bootstrap = find_external_ip(&control_instances[0].nics, client)
        .await?
        .unwrap_or_else(|| control_endpoint.clone());

    talos::bootstrap::run(
        &control_endpoint_for_bootstrap,
        true, // do_retry
        Some(&talosconfig_str),
        false, // verbose
    )
    .await
    .context("Failed to bootstrap etcd")?;

    eprintln!("    etcd bootstrapped successfully");

    // 14. Health check cluster
    // Note: The health check may fail when running from outside the cluster because
    // it tries to validate k8s API connectivity via the fabric IP (which is only
    // reachable from within Triton). We continue anyway since etcd bootstrapped.
    eprintln!("==> Checking cluster health");

    match talos::health::run(
        &control_endpoint_for_bootstrap,
        "5m", // wait_timeout (reduced since it may fail from outside)
        Some(&talosconfig_str),
        false, // verbose
    )
    .await
    {
        Ok(()) => eprintln!("    Cluster is healthy!"),
        Err(e) => {
            eprintln!(
                "    WARNING: Health check failed (may be expected from outside Triton): {}",
                e
            );
            eprintln!("    Continuing with kubeconfig retrieval...");
            logger.warn(format!("Health check failed: {}", e));
        }
    }

    // 15. Retrieve and store kubeconfig
    eprintln!("==> Retrieving kubeconfig");

    let kubeconfig_path = cluster_dir.join("kubeconfig");
    talos::kubeconfig::run(
        &control_endpoint_for_bootstrap,
        &kubeconfig_path.to_string_lossy(),
        Some(&talosconfig_str),
        false, // verbose
    )
    .await
    .context("Failed to retrieve kubeconfig")?;

    // Post-process kubeconfig to use external IP instead of fabric IP
    // The kubeconfig from Talos uses the cluster endpoint (the actual fabric IP after
    // the endpoint patch is applied). We need to replace it with the external IP so we
    // can access the K8s API from outside Triton.
    if let Some(ref fabric_ip) = actual_control_fabric_ip {
        let kubeconfig_content = tokio::fs::read_to_string(&kubeconfig_path)
            .await
            .context("Failed to read kubeconfig")?;

        let updated_content = kubeconfig_content.replace(
            &format!("server: https://{}:6443", fabric_ip),
            &format!("server: https://{}:6443", control_endpoint_for_bootstrap),
        );

        tokio::fs::write(&kubeconfig_path, updated_content)
            .await
            .context("Failed to write updated kubeconfig")?;

        logger.info(format!(
            "Updated kubeconfig server URL from {} to {}",
            fabric_ip, control_endpoint_for_bootstrap
        ));
    }

    eprintln!("    Saved to: {}", kubeconfig_path.display());

    // 16. Update cluster state with node info and save
    eprintln!("==> Updating cluster state");

    state.control_plane = Some(ControlPlaneConfig {
        endpoint: Some(control_endpoint.clone()),
        package: args.control_package.clone(),
        image: args.image.clone(),
        talos_version: args.talos_version.clone(),
    });

    state.workers = Some(WorkerConfig {
        package: args.worker_package.clone(),
        image: args.image.clone(),
        package_id: Some(worker_package_id),
        image_id: Some(image_id),
    });

    // Calculate and store the last fabric IP offset for adding workers later.
    // IPs are allocated starting at offset 10, with control nodes first, then workers.
    // The offset for the last worker is: 10 + control_count + worker_count - 1
    // We store the next available offset (one past the last allocated).
    if state.fabric_network_id.is_some() {
        let total_nodes = args.control_nodes + args.worker_nodes;
        // Starting offset is 10, so next available is 10 + total_nodes
        state.last_fabric_ip_offset = Some(10 + total_nodes);
    }

    // Build nodes map
    let mut nodes = HashMap::new();

    for inst in &control_instances {
        let fabric_ip = inst.nics.iter().find(|n| !n.primary).map(|n| n.ip.clone());

        nodes.insert(
            inst.name.clone(),
            NodeInfo {
                instance_id: inst.instance_id,
                primary_ip: inst.primary_ip.clone(),
                fabric_ip,
                role: NodeRole::Control,
            },
        );
    }

    for inst in &worker_instances {
        let fabric_ip = inst.nics.iter().find(|n| !n.primary).map(|n| n.ip.clone());

        nodes.insert(
            inst.name.clone(),
            NodeInfo {
                instance_id: inst.instance_id,
                primary_ip: inst.primary_ip.clone(),
                fabric_ip,
                role: NodeRole::Worker,
            },
        );
    }

    state.nodes = nodes;

    state.save().await.context("Failed to save cluster state")?;

    // Finalize logging
    logger.info("Bootstrap completed successfully");
    logger.flush().await?;
    logger.create_latest_symlink().await?;

    eprintln!();
    eprintln!("==> Bootstrap complete!");
    eprintln!();
    eprintln!("Cluster: {}", state.name);
    eprintln!("UUID:    {}", state.uuid);
    eprintln!(
        "Nodes:   {} control, {} worker",
        control_instances.len(),
        worker_instances.len()
    );
    eprintln!();
    eprintln!("Access your cluster:");
    eprintln!("  export KUBECONFIG={}", kubeconfig_path.to_string_lossy());
    eprintln!("  kubectl get nodes");
    eprintln!();
    eprintln!("Talos access:");
    eprintln!(
        "  talosctl --talosconfig {} --nodes {} dashboard",
        talosconfig_path.to_string_lossy(),
        control_endpoint_for_bootstrap
    );

    if let Some(log_path) = logger.log_file_path() {
        eprintln!();
        eprintln!("Logs: {}", log_path.display());
    }

    Ok(())
}

/// Generate Talos base configs using native implementation
///
/// The endpoint IP is added to the certificate's Subject Alternative Names (SANs)
/// so that TLS connections to the Kubernetes API are trusted.
async fn generate_talos_configs(
    cluster_name: &str,
    endpoint: &str,
    secrets_path: &std::path::Path,
    output_dir: &std::path::Path,
    logger: &LogWriter,
) -> Result<()> {
    // Note: --install-disk /dev/vda is required for Triton bhyve VMs which use
    // VirtIO disks. Without this, talosctl defaults to /dev/sda which doesn't exist.
    //
    // Additional SANs include the endpoint IP so that TLS connections to the
    // Kubernetes API server are trusted.
    logger.info("Generating Talos machine configs (native)");

    // Load secrets bundle
    let secrets = SecretsBundle::load(secrets_path)
        .await
        .context("Failed to load secrets bundle")?;

    // Generate machine configs with the endpoint IP
    // The endpoint parameter is just an IP address, not a URL
    let additional_sans = vec![endpoint.to_string()];

    let configs = generate_machine_configs(
        &secrets,
        cluster_name,
        endpoint,
        "/dev/vda",
        &additional_sans,
    )
    .context("Failed to generate machine configs")?;

    // Write output files
    let controlplane_path = output_dir.join("controlplane.yaml");
    let worker_path = output_dir.join("worker.yaml");
    let talosconfig_path = output_dir.join("talosconfig");

    tokio::fs::write(&controlplane_path, &configs.controlplane_yaml)
        .await
        .with_context(|| format!("Failed to write {}", controlplane_path.display()))?;

    tokio::fs::write(&worker_path, &configs.worker_yaml)
        .await
        .with_context(|| format!("Failed to write {}", worker_path.display()))?;

    tokio::fs::write(&talosconfig_path, &configs.talosconfig_yaml)
        .await
        .with_context(|| format!("Failed to write {}", talosconfig_path.display()))?;

    logger.info("Talos machine configs generated successfully");
    Ok(())
}
