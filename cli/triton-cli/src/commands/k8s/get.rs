// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Get cluster details with optional ASCII diagram visualization.

use std::collections::HashMap;

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_client::TypedClient;
use uuid::Uuid;

use super::provisioning::query_instance_nics;
use super::state::{ClusterState, NodeRole};

/// Arguments for the `k8s get` command.
#[derive(Args, Clone)]
pub struct GetArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Display cluster as ASCII diagram showing nodes, networks, and compute nodes
    #[arg(long)]
    pub ascii: bool,
}

/// Information about a network connection for display purposes.
struct NetworkDisplay {
    name: String,

    ip: String,

    is_public: bool,
}

/// Information about a node for display purposes.
struct NodeDisplay {
    name: String,

    #[allow(dead_code)]
    instance_id: Uuid,

    role: NodeRole,

    compute_node: Option<Uuid>,

    networks: Vec<NetworkDisplay>,
}

/// Collected cluster information for rendering.
struct ClusterDisplay {
    name: String,

    uuid: Uuid,

    endpoint: Option<String>,

    created_at: String,

    nodes: Vec<NodeDisplay>,

    all_networks: HashMap<Uuid, NetworkInfo>,
}

/// Cached network information.
struct NetworkInfo {
    name: String,

    is_public: bool,

    subnet: Option<String>,
}

/// Truncate a UUID to its first 8 characters for display.
fn truncate_uuid(uuid: &Uuid) -> String {
    uuid.to_string()[..8].to_string()
}

/// Collect all cluster information needed for display.
async fn collect_cluster_info(
    state: &ClusterState,
    client: &TypedClient,
) -> Result<ClusterDisplay> {
    let account = client.effective_account();
    let mut nodes = Vec::new();
    let mut all_networks: HashMap<Uuid, NetworkInfo> = HashMap::new();

    for (node_name, node_info) in &state.nodes {
        // Query machine details to get compute_node
        let compute_node = match client.get_machine(account, &node_info.instance_id).await {
            Ok(machine) => machine.compute_node,
            Err(e) => {
                eprintln!(
                    "    Warning: Failed to query instance {}: {}",
                    truncate_uuid(&node_info.instance_id),
                    e
                );
                None
            }
        };

        // Query NICs for this instance
        let nics = match query_instance_nics(node_info.instance_id, client).await {
            Ok(nics) => nics,
            Err(e) => {
                eprintln!(
                    "    Warning: Failed to query NICs for {}: {}",
                    node_name, e
                );
                Vec::new()
            }
        };

        // Build network display info for each NIC
        let mut networks = Vec::new();
        for nic in &nics {
            // Query network info if not already cached
            let net_info = if let Some(info) = all_networks.get(&nic.network_id) {
                info
            } else {
                let info = match client
                    .inner()
                    .get_network()
                    .account(account)
                    .network(nic.network_id.to_string())
                    .send()
                    .await
                {
                    Ok(response) => {
                        let network = response.into_inner();
                        NetworkInfo {
                            name: network.name,
                            is_public: network.public,
                            subnet: network.subnet,
                        }
                    }
                    Err(_) => NetworkInfo {
                        name: format!("network-{}", truncate_uuid(&nic.network_id)),
                        is_public: false,
                        subnet: None,
                    },
                };
                all_networks.insert(nic.network_id, info);
                all_networks.get(&nic.network_id).expect("just inserted")
            };

            networks.push(NetworkDisplay {
                name: net_info.name.clone(),
                ip: nic.ip.clone(),
                is_public: net_info.is_public,
            });
        }

        // Sort networks: public first, then by name
        networks.sort_by(|a, b| {
            if a.is_public != b.is_public {
                b.is_public.cmp(&a.is_public)
            } else {
                a.name.cmp(&b.name)
            }
        });

        nodes.push(NodeDisplay {
            name: node_name.clone(),
            instance_id: node_info.instance_id,
            role: node_info.role,
            compute_node,
            networks,
        });
    }

    // Sort nodes: control plane first, then by name
    nodes.sort_by(|a, b| {
        if a.role != b.role {
            match (&a.role, &b.role) {
                (NodeRole::Control, NodeRole::Worker) => std::cmp::Ordering::Less,
                (NodeRole::Worker, NodeRole::Control) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        } else {
            a.name.cmp(&b.name)
        }
    });

    let endpoint = state
        .control_plane
        .as_ref()
        .and_then(|cp| cp.endpoint.clone());

    Ok(ClusterDisplay {
        name: state.name.clone(),
        uuid: state.uuid,
        endpoint,
        created_at: state.created_at.to_rfc3339(),
        nodes,
        all_networks,
    })
}

/// Render the ASCII diagram for the cluster.
fn render_ascii_diagram(info: &ClusterDisplay) {
    const BOX_WIDTH: usize = 35;
    const OUTER_WIDTH: usize = 77;

    // Header
    println!("Cluster: {} ({})", info.name, info.uuid);
    if let Some(endpoint) = &info.endpoint {
        println!("Endpoint: {}", endpoint);
    }
    println!("Created: {}", info.created_at);
    println!();

    // Handle empty cluster
    if info.nodes.is_empty() {
        println!("(no nodes)");
        return;
    }

    // Group nodes by compute node
    let mut by_cn: HashMap<Option<Uuid>, Vec<&NodeDisplay>> = HashMap::new();
    for node in &info.nodes {
        by_cn.entry(node.compute_node).or_default().push(node);
    }

    // Sort CN groups: known CNs first (sorted), then unknown
    let mut cn_keys: Vec<_> = by_cn.keys().cloned().collect();
    cn_keys.sort_by(|a, b| match (a, b) {
        (Some(a_uuid), Some(b_uuid)) => a_uuid.cmp(b_uuid),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    // Top border
    println!("┌{}┐", "─".repeat(OUTER_WIDTH));

    for (cn_idx, cn) in cn_keys.iter().enumerate() {
        let cn_label = match cn {
            Some(uuid) => format!("CN: {}...", truncate_uuid(uuid)),
            None => "CN: (unknown)".to_string(),
        };

        // CN header
        println!("│ {:<width$} │", cn_label, width = OUTER_WIDTH - 2);
        println!("├{}┤", "─".repeat(OUTER_WIDTH));

        let nodes = by_cn.get(cn).expect("key exists");

        // Render nodes in pairs (2 per row)
        for chunk in nodes.chunks(2) {
            // Determine max height needed for this row
            let max_networks = chunk.iter().map(|n| n.networks.len()).max().unwrap_or(0);
            let box_height = 2 + max_networks; // name line + network lines

            // Build box lines for each node in the chunk
            let mut box_lines: Vec<Vec<String>> = Vec::new();
            for node in chunk {
                let mut lines = Vec::new();

                // Top border of node box
                lines.push(format!("┌{}┐", "─".repeat(BOX_WIDTH - 2)));

                // Node name with role marker
                let role_marker = if node.role == NodeRole::Control {
                    " [C]"
                } else {
                    ""
                };
                let name_line = format!("{}{}", node.name, role_marker);
                lines.push(format!("│ {:<width$} │", name_line, width = BOX_WIDTH - 4));

                // Network lines
                for net in &node.networks {
                    let net_type = if net.is_public { "external" } else { "fabric" };
                    let net_line = format!("{} ({})", net.ip, net_type);
                    lines.push(format!("│ {:<width$} │", net_line, width = BOX_WIDTH - 4));
                }

                // Pad to max height
                while lines.len() < box_height + 1 {
                    lines.push(format!("│ {:<width$} │", "", width = BOX_WIDTH - 4));
                }

                // Bottom border of node box
                lines.push(format!("└{}┘", "─".repeat(BOX_WIDTH - 2)));

                box_lines.push(lines);
            }

            // Render the boxes side by side
            let total_lines = box_lines[0].len();
            for line_idx in 0..total_lines {
                let mut row = String::from("│ ");
                for (box_idx, box_content) in box_lines.iter().enumerate() {
                    if box_idx > 0 {
                        row.push_str("  ");
                    }
                    row.push_str(&box_content[line_idx]);
                }
                // Pad to outer width
                let current_len = row.chars().count();
                let padding = OUTER_WIDTH - current_len;
                row.push_str(&" ".repeat(padding));
                row.push_str(" │");
                println!("{}", row);
            }
        }

        // Separator between CN groups (but not after last)
        if cn_idx < cn_keys.len() - 1 {
            println!("├{}┤", "─".repeat(OUTER_WIDTH));
        }
    }

    // Bottom border
    println!("└{}┘", "─".repeat(OUTER_WIDTH));

    // Network summary
    println!();
    println!("Networks:");
    let mut network_list: Vec<_> = info.all_networks.iter().collect();
    network_list.sort_by(|a, b| {
        if a.1.is_public != b.1.is_public {
            b.1.is_public.cmp(&a.1.is_public)
        } else {
            a.1.name.cmp(&b.1.name)
        }
    });

    for (_, net_info) in network_list {
        let net_type = if net_info.is_public {
            "(public)"
        } else {
            "(fabric)"
        };
        let subnet_str = net_info
            .subnet
            .as_ref()
            .map(|s| format!(" {}", s))
            .unwrap_or_default();
        println!("  {}: {}{}", net_info.name, net_type, subnet_str);
    }
}

/// Run the `k8s get` command.
pub async fn run(args: GetArgs, client: &TypedClient, _use_json: bool) -> Result<()> {
    let state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .with_context(|| format!("Failed to load cluster '{}'", args.cluster))?;

    if args.ascii {
        eprintln!("==> Collecting cluster information");
        let info = collect_cluster_info(&state, client).await?;
        println!();
        render_ascii_diagram(&info);
    } else {
        // Default: pretty-printed JSON
        let json = serde_json::to_string_pretty(&state)
            .context("Failed to serialize cluster state")?;
        println!("{}", json);
    }

    Ok(())
}
