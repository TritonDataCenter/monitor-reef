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
    /// Network UUID for tracking connections.
    network_id: Uuid,

    #[allow(dead_code)]
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

/// Format a CN UUID for display (full UUID).
fn format_cn_uuid(uuid: &Uuid) -> String {
    uuid.to_string()
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
                eprintln!("    Warning: Failed to query NICs for {}: {}", node_name, e);
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
                // SAFETY: We just inserted this key, so it must exist
                #[allow(clippy::expect_used)]
                all_networks.get(&nic.network_id).expect("just inserted")
            };

            networks.push(NetworkDisplay {
                network_id: nic.network_id,
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

/// Tracks which column position each instance's connector lines occupy.
struct InstancePosition {
    /// The node name for reference.
    name: String,

    /// Column positions for each network connection line (one per network).
    /// These are absolute character positions in the output.
    connector_columns: Vec<usize>,

    /// Network IDs this instance connects to, in order matching connector_columns.
    network_ids: Vec<Uuid>,
}

/// Render the ASCII diagram for the cluster.
fn render_ascii_diagram(info: &ClusterDisplay) {
    // Width of each instance box (interior content width + borders)
    const BOX_WIDTH: usize = 31;
    // Total width of CN outer container (interior)
    const OUTER_WIDTH: usize = 77;
    // Space between instance boxes when side by side
    const BOX_SPACING: usize = 6;
    // Left margin inside CN box before first instance box
    const LEFT_MARGIN: usize = 3;

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

    // Build sorted network list (fabric networks first, then public, then by name)
    let mut network_list: Vec<_> = info.all_networks.iter().collect();
    network_list.sort_by(|a, b| {
        if a.1.is_public != b.1.is_public {
            // Fabric (non-public) first, then public
            a.1.is_public.cmp(&b.1.is_public)
        } else {
            a.1.name.cmp(&b.1.name)
        }
    });

    // Phase 1: Calculate positions for all instances and their connector lines.
    // Connector columns are relative to the start of the full output line (col 0 = first char).
    // The CN box border is at col 0, content starts at col 2 (after "│ ").
    let mut all_positions: Vec<InstancePosition> = Vec::new();

    for cn in &cn_keys {
        let Some(nodes) = by_cn.get(cn) else {
            continue;
        };
        for chunk in nodes.chunks(2) {
            for (box_idx, node) in chunk.iter().enumerate() {
                // Position of instance box left edge relative to full line
                // "│ " prefix = 2 chars, then LEFT_MARGIN, then boxes
                let box_start = 2 + LEFT_MARGIN + box_idx * (BOX_WIDTH + BOX_SPACING);
                let box_center = box_start + BOX_WIDTH / 2;

                // Each network connection gets its own vertical line
                let mut connector_columns = Vec::new();
                let mut network_ids = Vec::new();

                let net_count = node.networks.len();
                for (net_idx, net) in node.networks.iter().enumerate() {
                    // Spread connector lines evenly across the box bottom
                    let offset = if net_count == 1 {
                        0isize
                    } else {
                        // For 2 networks: offsets are -3, +3
                        // For 3 networks: offsets are -4, 0, +4
                        let half = (net_count - 1) as isize;
                        let pos = net_idx as isize * 2 - half;
                        pos * 3
                    };
                    let col = (box_center as isize + offset) as usize;
                    connector_columns.push(col);
                    network_ids.push(net.network_id);
                }

                all_positions.push(InstancePosition {
                    name: node.name.clone(),
                    connector_columns,
                    network_ids,
                });
            }
        }
    }

    // Collect all connector columns that need to pass through CN boundaries
    let all_connector_cols: Vec<usize> = all_positions
        .iter()
        .flat_map(|p| p.connector_columns.iter().copied())
        .collect();

    // Total output line width including CN box borders
    let total_line_width = OUTER_WIDTH + 2;

    // Phase 2: Render CN boxes with simplified instance boxes
    println!("┌{}┐", "─".repeat(OUTER_WIDTH));

    let mut position_idx = 0;
    for (cn_idx, cn) in cn_keys.iter().enumerate() {
        let cn_label = match cn {
            Some(uuid) => format!("CN: {}", format_cn_uuid(uuid)),
            None => "CN: (unknown)".to_string(),
        };

        // Connectors from previous CNs that need to pass through this CN
        let prev_cn_cols: Vec<usize> = all_positions[..position_idx]
            .iter()
            .flat_map(|p| p.connector_columns.iter().copied())
            .collect();

        // CN header - show pass-through lines from previous CNs
        let mut header_row: Vec<char> = vec![' '; total_line_width];
        header_row[0] = '│';
        header_row[total_line_width - 1] = '│';
        // Add pass-through lines
        for &col in &prev_cn_cols {
            if col > 0 && col < total_line_width - 1 {
                header_row[col] = '│';
            }
        }
        // Write CN label, but preserve pass-through lines that fall outside the text
        let label_start = 2; // After "│ "
        for (i, ch) in cn_label.chars().enumerate() {
            let col = label_start + i;
            if col < total_line_width - 1 {
                header_row[col] = ch;
            }
        }
        println!("{}", header_row.iter().collect::<String>());

        // CN header separator with pass-through crossings
        let mut header_sep: Vec<char> = vec!['─'; total_line_width];
        header_sep[0] = '├';
        header_sep[total_line_width - 1] = '┤';
        for &col in &prev_cn_cols {
            if col > 0 && col < total_line_width - 1 {
                header_sep[col] = '┼';
            }
        }
        println!("{}", header_sep.iter().collect::<String>());

        let Some(nodes) = by_cn.get(cn) else {
            continue;
        };

        // Render nodes in pairs (2 per row)
        for chunk in nodes.chunks(2) {
            // Collect connector columns from previous CNs that need to pass through
            let prev_cn_cols: Vec<usize> = all_positions[..position_idx]
                .iter()
                .flat_map(|p| p.connector_columns.iter().copied())
                .collect();

            // Empty line before boxes - show vertical lines from previous CNs
            let mut empty_line: Vec<char> = vec![' '; total_line_width];
            empty_line[0] = '│';
            empty_line[total_line_width - 1] = '│';
            for &col in &prev_cn_cols {
                if col > 0 && col < total_line_width - 1 {
                    empty_line[col] = '│';
                }
            }
            println!("{}", empty_line.iter().collect::<String>());

            // Build box lines for each node in the chunk
            let mut box_lines: Vec<Vec<String>> = Vec::new();
            let mut chunk_positions: Vec<&InstancePosition> = Vec::new();

            for node in chunk {
                chunk_positions.push(&all_positions[position_idx]);
                position_idx += 1;

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

                box_lines.push(lines);
            }

            // Determine which pass-through lines fall within each box's boundaries
            // These lines will TERMINATE at the box top (using ┴) rather than pass through
            let mut box_ranges: Vec<(usize, usize)> = Vec::new();
            for box_idx in 0..box_lines.len() {
                let box_start = 2 + LEFT_MARGIN + box_idx * (BOX_WIDTH + BOX_SPACING);
                let box_end = box_start + BOX_WIDTH;
                box_ranges.push((box_start, box_end));
            }

            // Render the boxes side by side (top and content lines)
            // Line 0 is top border, line 1 is content
            let total_lines = box_lines[0].len();
            for line_idx in 0..total_lines {
                let mut row: Vec<char> = vec![' '; total_line_width];
                row[0] = '│';
                row[total_line_width - 1] = '│';

                // Add vertical connectors from previous CNs ONLY outside box boundaries
                for &col in &prev_cn_cols {
                    if col > 0 && col < total_line_width - 1 {
                        let inside_any_box = box_ranges
                            .iter()
                            .any(|&(start, end)| col >= start && col < end);
                        if !inside_any_box {
                            row[col] = '│';
                        }
                    }
                }

                // Place the box content, handling intersections with pass-through lines
                for (box_idx, box_content) in box_lines.iter().enumerate() {
                    let box_start = 2 + LEFT_MARGIN + box_idx * (BOX_WIDTH + BOX_SPACING);

                    for (i, ch) in box_content[line_idx].chars().enumerate() {
                        let col = box_start + i;
                        if col < total_line_width - 1 {
                            let is_passthrough = prev_cn_cols.contains(&col);

                            if line_idx == 0 {
                                // Top border line - pass-through lines TERMINATE here with ┴
                                if is_passthrough {
                                    if ch == '─' {
                                        row[col] = '┴';
                                    } else if ch == '┌' {
                                        // Corner with pass-through terminating
                                        row[col] = '├';
                                    } else if ch == '┐' {
                                        row[col] = '┤';
                                    } else {
                                        row[col] = ch;
                                    }
                                } else {
                                    row[col] = ch;
                                }
                            } else {
                                // Content line - box content takes priority, no pass-through
                                row[col] = ch;
                            }
                        }
                    }
                }

                println!("{}", row.iter().collect::<String>());
            }

            // Render bottom borders with connector stubs
            // Pass-through lines from prev CNs only show OUTSIDE box boundaries
            let mut bottom_row: Vec<char> = vec![' '; total_line_width];
            bottom_row[0] = '│';
            bottom_row[total_line_width - 1] = '│';

            // Add vertical connectors from previous CNs ONLY outside box boundaries
            for &col in &prev_cn_cols {
                if col > 0 && col < total_line_width - 1 {
                    let inside_any_box = box_ranges
                        .iter()
                        .any(|&(start, end)| col >= start && col < end);
                    if !inside_any_box {
                        bottom_row[col] = '│';
                    }
                }
            }

            for (box_idx, pos) in chunk_positions.iter().enumerate() {
                let box_start = 2 + LEFT_MARGIN + box_idx * (BOX_WIDTH + BOX_SPACING);
                let box_end = box_start + BOX_WIDTH;

                // Build the bottom border
                // Own connectors + consumed pass-throughs that fall within this box
                // (pass-through lines that terminated at the TOP of this box re-emerge here)
                for i in 0..BOX_WIDTH {
                    let col = box_start + i;
                    if col >= total_line_width - 1 {
                        continue;
                    }

                    let is_own_connector = pos.connector_columns.contains(&col);
                    let is_consumed_passthrough =
                        prev_cn_cols.contains(&col) && col >= box_start && col < box_end;

                    let ch = if i == 0 {
                        '└'
                    } else if i == BOX_WIDTH - 1 {
                        '┘'
                    } else if is_own_connector || is_consumed_passthrough {
                        '┬'
                    } else {
                        '─'
                    };

                    bottom_row[col] = ch;
                }
            }
            println!("{}", bottom_row.iter().collect::<String>());

            // Render connector lines dropping down from boxes
            // This includes:
            // - This chunk's own connectors
            // - Prev CN connectors that are OUTSIDE box boundaries (pass-through)
            // - Prev CN connectors that were INSIDE boxes (re-emerged from box bottom)
            let chunk_cols: Vec<usize> = chunk_positions
                .iter()
                .flat_map(|p| p.connector_columns.iter().copied())
                .collect();

            let mut connector_row: Vec<char> = vec![' '; total_line_width];
            connector_row[0] = '│';
            connector_row[total_line_width - 1] = '│';
            // All prev CN connectors now show (both pass-through and re-emerged)
            for &col in &prev_cn_cols {
                if col > 0 && col < total_line_width - 1 {
                    connector_row[col] = '│';
                }
            }
            // This chunk's own connectors
            for &col in &chunk_cols {
                if col > 0 && col < total_line_width - 1 {
                    connector_row[col] = '│';
                }
            }
            println!("{}", connector_row.iter().collect::<String>());
        }

        // Render CN bottom border with connector lines passing through
        if cn_idx < cn_keys.len() - 1 {
            // Collect connector columns up to this point (all instances rendered so far)
            let rendered_cols: Vec<usize> = all_positions[..position_idx]
                .iter()
                .flat_map(|p| p.connector_columns.iter().copied())
                .collect();

            // Build line with connectors passing through (use ┼ since lines continue)
            let mut border_line: Vec<char> = vec!['─'; total_line_width];
            border_line[0] = '└';
            border_line[total_line_width - 1] = '┘';

            for &col in &rendered_cols {
                if col > 0 && col < total_line_width - 1 {
                    border_line[col] = '┼';
                }
            }
            println!("{}", border_line.iter().collect::<String>());

            // Gap line showing vertical connectors between CN boxes
            let mut gap_line: Vec<char> = vec![' '; total_line_width];
            for &col in &rendered_cols {
                if col > 0 && col < total_line_width - 1 {
                    gap_line[col] = '│';
                }
            }
            println!("{}", gap_line.iter().collect::<String>());

            // Top border of next CN box (use ┼ since lines continue through)
            let mut top_border: Vec<char> = vec!['─'; total_line_width];
            top_border[0] = '┌';
            top_border[total_line_width - 1] = '┐';
            for &col in &rendered_cols {
                if col > 0 && col < total_line_width - 1 {
                    top_border[col] = '┼';
                }
            }
            println!("{}", top_border.iter().collect::<String>());
        }
    }

    // Bottom border of last CN box with connectors passing through
    // ALL connector columns need to exit the CN area (even ones that were "consumed"
    // by instance boxes - those network connections still need to reach the network boxes)
    // Use ┼ since lines continue through to network boxes below
    let mut border_line: Vec<char> = vec!['─'; total_line_width];
    border_line[0] = '└';
    border_line[total_line_width - 1] = '┘';

    for &col in &all_connector_cols {
        if col > 0 && col < total_line_width - 1 {
            border_line[col] = '┼';
        }
    }
    println!("{}", border_line.iter().collect::<String>());

    // Phase 3: Draw vertical connector lines in the gap between CN boxes and network boxes
    let mut connector_line: Vec<char> = vec![' '; total_line_width];
    for &col in &all_connector_cols {
        if col > 0 && col < total_line_width - 1 {
            connector_line[col] = '│';
        }
    }
    println!("{}", connector_line.iter().collect::<String>());

    // Phase 4: Render network boxes at the bottom
    for (net_id, net_info) in &network_list {
        // Find all instances that connect to this network and their IPs
        let mut connected_instances: Vec<(&str, &str, usize)> = Vec::new();
        for pos in &all_positions {
            for (idx, &nid) in pos.network_ids.iter().enumerate() {
                if &nid == *net_id {
                    // Find the IP for this connection
                    for node in &info.nodes {
                        if node.name == pos.name {
                            for net in &node.networks {
                                if net.network_id == nid {
                                    connected_instances.push((
                                        &pos.name,
                                        &net.ip,
                                        pos.connector_columns[idx],
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        if connected_instances.is_empty() {
            continue;
        }

        // Get connector columns for this network
        let net_connector_cols: Vec<usize> =
            connected_instances.iter().map(|(_, _, col)| *col).collect();

        // Find remaining networks and their connectors (for lines that pass through)
        let remaining_cols: Vec<usize> = all_positions
            .iter()
            .flat_map(|p| {
                p.network_ids
                    .iter()
                    .zip(p.connector_columns.iter())
                    .filter(|(nid, _)| {
                        // Check if this network comes after the current one
                        let current_idx = network_list
                            .iter()
                            .position(|(id, _)| id == net_id)
                            .unwrap_or(0);
                        network_list
                            .iter()
                            .position(|(id, _)| *id == *nid)
                            .map(|idx| idx > current_idx)
                            .unwrap_or(false)
                    })
                    .map(|(_, col)| *col)
            })
            .collect();

        // Draw top border connecting to this network box
        let mut top_border: Vec<char> = vec!['─'; total_line_width];
        top_border[0] = ' ';
        top_border[1] = '┌';
        top_border[total_line_width - 2] = '┐';
        top_border[total_line_width - 1] = ' ';

        for &col in &net_connector_cols {
            if col > 1 && col < total_line_width - 2 {
                top_border[col] = '┴';
            }
        }
        // Remaining connectors pass through
        for &col in &remaining_cols {
            if col > 1 && col < total_line_width - 2 {
                top_border[col] = '┼';
            }
        }
        println!("{}", top_border.iter().collect::<String>());

        // Network name and type line
        let net_type = if net_info.is_public {
            "public"
        } else {
            "fabric"
        };
        let subnet_str = net_info
            .subnet
            .as_ref()
            .map(|s| format!(" {}", s))
            .unwrap_or_default();
        let net_label = format!("{} ({}){}", net_info.name, net_type, subnet_str);

        let mut name_line: Vec<char> = vec![' '; total_line_width];
        name_line[1] = '│';
        name_line[total_line_width - 2] = '│';
        // Remaining connectors pass through - place BEFORE text so text takes priority
        for &col in &remaining_cols {
            if col > 1 && col < total_line_width - 2 {
                name_line[col] = '│';
            }
        }
        // Insert label starting at position 3 - text overwrites any pass-through lines
        for (i, ch) in net_label.chars().enumerate() {
            if 3 + i < total_line_width - 3 {
                name_line[3 + i] = ch;
            }
        }
        println!("{}", name_line.iter().collect::<String>());

        // IP addresses line
        let ips: Vec<&str> = connected_instances.iter().map(|(_, ip, _)| *ip).collect();
        let ip_text = format!("  {}", ips.join(", "));

        let mut ip_line: Vec<char> = vec![' '; total_line_width];
        ip_line[1] = '│';
        ip_line[total_line_width - 2] = '│';
        // Remaining connectors pass through - place BEFORE text so text takes priority
        for &col in &remaining_cols {
            if col > 1 && col < total_line_width - 2 {
                ip_line[col] = '│';
            }
        }
        // Insert IP text - text overwrites any pass-through lines
        for (i, ch) in ip_text.chars().enumerate() {
            if 3 + i < total_line_width - 3 {
                ip_line[3 + i] = ch;
            }
        }
        println!("{}", ip_line.iter().collect::<String>());

        // Bottom border
        let mut bottom_border: Vec<char> = vec!['─'; total_line_width];
        bottom_border[0] = ' ';
        bottom_border[1] = '└';
        bottom_border[total_line_width - 2] = '┘';
        bottom_border[total_line_width - 1] = ' ';
        // Remaining connectors pass through
        for &col in &remaining_cols {
            if col > 1 && col < total_line_width - 2 {
                bottom_border[col] = '┴';
            }
        }
        println!("{}", bottom_border.iter().collect::<String>());

        // Gap line showing remaining connectors
        if !remaining_cols.is_empty() {
            let mut gap_line: Vec<char> = vec![' '; total_line_width];
            for &col in &remaining_cols {
                if col > 0 && col < total_line_width - 1 {
                    gap_line[col] = '│';
                }
            }
            println!("{}", gap_line.iter().collect::<String>());
        }
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
        let json =
            serde_json::to_string_pretty(&state).context("Failed to serialize cluster state")?;
        println!("{}", json);
    }

    Ok(())
}
