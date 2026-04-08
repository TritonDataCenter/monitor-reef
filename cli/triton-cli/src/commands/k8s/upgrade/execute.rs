// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Execute Talos upgrade operations

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::time::Duration;

use super::UpgradeArgs;
use crate::commands::k8s::state::{ClusterState, NodeInfo, NodeRole};
use crate::commands::k8s::talos;

/// Node upgrade status for JSON output
#[derive(Serialize)]
struct NodeUpgradeStatus {
    name: String,
    role: String,
    current_version: Option<String>,
    target_version: String,
    status: String,
    /// The endpoint used to reach this node (may be proxied through control plane)
    endpoint: String,
    /// If set, this is the fabric IP used to route through the control plane
    #[serde(skip_serializing_if = "Option::is_none")]
    fabric_ip: Option<String>,
}

/// Upgrade result for JSON output
#[derive(Serialize)]
struct UpgradeOutput {
    cluster: String,
    target_version: String,
    dry_run: bool,
    nodes: Vec<NodeUpgradeStatus>,
}

pub async fn run(args: UpgradeArgs, json: bool) -> Result<()> {
    // Load cluster state
    let state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    let cluster_dir = state.cluster_dir()?;
    let talosconfig_path = cluster_dir.join("talosconfig");

    if !tokio::fs::try_exists(&talosconfig_path)
        .await
        .unwrap_or(false)
    {
        anyhow::bail!(
            "Cluster {} has no talosconfig - was it bootstrapped?",
            state.name
        );
    }

    let talosconfig = talosconfig_path.to_string_lossy().to_string();

    // Determine target version/image
    let installer_image = args.installer_image()?;
    let target_version = args.target_version()?;

    // Collect nodes to upgrade based on flags
    let mut nodes_to_upgrade: Vec<(&String, &NodeInfo)> = Vec::new();

    // Determine which nodes to upgrade
    let upgrade_control = !args.workers; // upgrade control unless --workers specified
    let upgrade_workers = !args.control_plane; // upgrade workers unless --control-plane specified

    // Control plane nodes first (for proper upgrade order)
    if upgrade_control {
        let control_nodes: Vec<_> = state
            .nodes
            .iter()
            .filter(|(_, info)| info.role == NodeRole::Control)
            .collect();
        nodes_to_upgrade.extend(control_nodes);
    }

    // Then worker nodes
    if upgrade_workers {
        let worker_nodes: Vec<_> = state
            .nodes
            .iter()
            .filter(|(_, info)| info.role == NodeRole::Worker)
            .collect();
        nodes_to_upgrade.extend(worker_nodes);
    }

    if nodes_to_upgrade.is_empty() {
        if json {
            let output = UpgradeOutput {
                cluster: state.name.clone(),
                target_version: target_version.clone(),
                dry_run: args.dry_run,
                nodes: vec![],
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!("No nodes to upgrade");
        }
        return Ok(());
    }

    if !json {
        eprintln!("==> Upgrade plan for cluster '{}'", state.name);
        eprintln!("    Target version: {}", target_version);
        eprintln!("    Installer image: {}", installer_image);
        eprintln!(
            "    Nodes to upgrade: {} ({} control, {} worker)",
            nodes_to_upgrade.len(),
            nodes_to_upgrade
                .iter()
                .filter(|(_, i)| i.role == NodeRole::Control)
                .count(),
            nodes_to_upgrade
                .iter()
                .filter(|(_, i)| i.role == NodeRole::Worker)
                .count()
        );
        eprintln!();
    }

    // Find the control plane endpoint for proxying worker requests
    let control_plane_endpoint = state
        .nodes
        .iter()
        .find(|(_, i)| i.role == NodeRole::Control)
        .and_then(|(_, i)| i.primary_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("No control plane node found"))?;

    // Query current versions and build status
    let mut node_statuses: Vec<NodeUpgradeStatus> = Vec::new();

    for (name, info) in &nodes_to_upgrade {
        // For control plane nodes, use their primary IP directly
        // For worker nodes, route through the control plane using their fabric IP
        let (endpoint, fabric_ip) = match info.role {
            NodeRole::Control => {
                let ep = info
                    .primary_ip
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Control node {} has no primary IP", name))?;
                (ep.clone(), None)
            }
            NodeRole::Worker => {
                // Workers are only reachable via fabric IP, routed through control plane
                let fabric = info
                    .fabric_ip
                    .as_ref()
                    .or(info.primary_ip.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("Worker node {} has no IP address", name))?;
                (control_plane_endpoint.clone(), Some(fabric.clone()))
            }
        };

        // Query current version (route through proxy if needed)
        let current_version = match talos::version::get_version_via(
            &endpoint,
            fabric_ip.as_deref(),
            Some(&talosconfig),
            false,
        )
        .await
        {
            Ok(v) => Some(v.tag),
            Err(e) => {
                if !json {
                    eprintln!("    WARNING: Could not query version for {}: {}", name, e);
                }
                None
            }
        };

        let already_at_target = current_version
            .as_ref()
            .is_some_and(|v| v == &target_version);

        let status = if already_at_target {
            "already_at_target".to_string()
        } else {
            "pending".to_string()
        };

        let role = match info.role {
            NodeRole::Control => "control",
            NodeRole::Worker => "worker",
        };

        if !json {
            let current = current_version.as_deref().unwrap_or("unknown");
            let via_info = if fabric_ip.is_some() {
                format!(" (via {})", control_plane_endpoint)
            } else {
                String::new()
            };
            if already_at_target {
                eprintln!(
                    "    {} ({}): {} -> SKIP (already at target){}",
                    name, role, current, via_info
                );
            } else {
                eprintln!(
                    "    {} ({}): {} -> {}{}",
                    name, role, current, target_version, via_info
                );
            }
        }

        node_statuses.push(NodeUpgradeStatus {
            name: (*name).clone(),
            role: role.to_string(),
            current_version,
            target_version: target_version.clone(),
            status,
            endpoint: endpoint.clone(),
            fabric_ip,
        });
    }

    // If dry-run, stop here
    if args.dry_run {
        if json {
            let output = UpgradeOutput {
                cluster: state.name.clone(),
                target_version,
                dry_run: true,
                nodes: node_statuses,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!();
            eprintln!("==> Dry-run complete. No changes made.");
            eprintln!("    Run without --dry-run to apply the upgrade.");
        }
        return Ok(());
    }

    // Pre-flight checks (unless --force)
    if !args.force {
        if !json {
            eprintln!();
            eprintln!("==> Running pre-flight checks");
        }

        // Check cluster health via first control plane node
        let control_endpoint = state
            .nodes
            .iter()
            .find(|(_, i)| i.role == NodeRole::Control)
            .and_then(|(_, i)| i.primary_ip.as_ref().or(i.fabric_ip.as_ref()));

        if let Some(endpoint) = control_endpoint {
            if !json {
                eprintln!(
                    "    Checking cluster health (timeout: {})...",
                    args.health_timeout
                );
            }
            match talos::health::run(
                endpoint,
                &args.health_timeout,
                Some(&talosconfig),
                None,
                false,
            )
            .await
            {
                Ok(()) => {
                    if !json {
                        eprintln!("    Cluster health: OK");
                    }
                }
                Err(e) => {
                    if !json {
                        eprintln!("    Cluster health: FAILED - {}", e);
                        eprintln!();
                        eprintln!("    Use --force to skip pre-flight checks, or");
                        eprintln!(
                            "    use --health-timeout to increase the timeout (current: {})",
                            args.health_timeout
                        );
                    }
                    anyhow::bail!("Pre-flight health check failed: {}", e);
                }
            }
        }
    }

    // Execute upgrades one node at a time
    if !json {
        eprintln!();
        eprintln!("==> Starting rolling upgrade");
    }

    for status in &mut node_statuses {
        // Skip nodes already at target version
        if status.status == "already_at_target" {
            continue;
        }

        if !json {
            eprintln!();
            eprintln!(
                "==> Upgrading {} ({}) to {}",
                status.name, status.role, target_version
            );
        }

        status.status = "upgrading".to_string();

        // Send upgrade request (route through proxy if needed)
        match talos::upgrade::upgrade_node_via(
            &status.endpoint,
            status.fabric_ip.as_deref(),
            &installer_image,
            args.preserve,
            args.stage,
            args.force,
            Some(&talosconfig),
            false,
        )
        .await
        {
            Ok(result) => {
                if !json {
                    eprintln!("    Upgrade initiated: {}", result.ack);
                    eprintln!("    Actor ID: {}", result.actor_id);
                }
            }
            Err(e) => {
                status.status = format!("failed: {}", e);
                if !json {
                    eprintln!("    FAILED: {}", e);
                }
                anyhow::bail!("Failed to upgrade {}: {}", status.name, e);
            }
        }

        // Wait for node to go down and come back up
        if !json {
            eprintln!(
                "    Waiting for node to reboot (timeout: {})...",
                args.reboot_timeout
            );
        }

        // Wait for node to become unreachable (rebooting)
        tokio::time::sleep(Duration::from_secs(10)).await;

        // Wait for node to come back and verify new version
        let max_wait = parse_duration(&args.reboot_timeout)?;
        let poll_interval = Duration::from_secs(10);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > max_wait {
                status.status = "timeout".to_string();
                if !json {
                    eprintln!(
                        "    TIMEOUT: Node did not come back after {}",
                        args.reboot_timeout
                    );
                }
                anyhow::bail!(
                    "Timeout waiting for {} to come back after upgrade",
                    status.name
                );
            }

            // Try to query version (route through proxy if needed)
            match talos::version::get_version_via(
                &status.endpoint,
                status.fabric_ip.as_deref(),
                Some(&talosconfig),
                false,
            )
            .await
            {
                Ok(v) => {
                    if v.tag == target_version {
                        status.status = "completed".to_string();
                        status.current_version = Some(v.tag.clone());
                        if !json {
                            eprintln!("    Node upgraded successfully to {}", v.tag);
                        }
                        break;
                    } else {
                        // Node is back but not at target version yet (might still be upgrading)
                        if !json {
                            eprintln!(
                                "    Node is back at version {}, waiting for upgrade to complete...",
                                v.tag
                            );
                        }
                    }
                }
                Err(_) => {
                    // Node not reachable yet, keep waiting
                }
            }

            tokio::time::sleep(poll_interval).await;
        }

        // Brief health check after node upgrade
        if !json {
            eprintln!("    Verifying node health...");
        }

        // Give the node a moment to stabilize
        tokio::time::sleep(Duration::from_secs(5)).await;

        // For control plane nodes, do a health check before proceeding
        if status.role == "control" {
            match talos::health::run(&status.endpoint, "60s", Some(&talosconfig), None, false).await
            {
                Ok(()) => {
                    if !json {
                        eprintln!("    Node health: OK");
                    }
                }
                Err(e) => {
                    if !json {
                        eprintln!("    WARNING: Health check failed: {}", e);
                        eprintln!("    Continuing with upgrade...");
                    }
                }
            }
        }
    }

    // Final output
    if json {
        let output = UpgradeOutput {
            cluster: state.name.clone(),
            target_version,
            dry_run: false,
            nodes: node_statuses,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        eprintln!();
        eprintln!("==> Upgrade complete!");
        eprintln!("    All nodes upgraded to {}", args.target_version()?);
    }

    Ok(())
}

/// Parse a human-readable duration string like "10m", "1h", "30s", "1h30m".
fn parse_duration(s: &str) -> Result<Duration> {
    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            let n: u64 = current_num
                .parse()
                .with_context(|| format!("invalid duration '{}'", s))?;
            current_num.clear();

            match ch {
                'h' => total_secs += n * 3600,
                'm' => total_secs += n * 60,
                's' => total_secs += n,
                _ => bail!("unknown duration unit '{}' in '{}'", ch, s),
            }
        }
    }

    // Handle bare number (assume seconds).
    if !current_num.is_empty() {
        let n: u64 = current_num.parse()?;
        total_secs += n;
    }

    if total_secs == 0 {
        bail!("duration '{}' resolves to zero", s);
    }

    Ok(Duration::from_secs(total_secs))
}
