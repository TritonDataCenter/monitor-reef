// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Delete cluster

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;
use tokio::time::sleep;
use uuid::Uuid;

use super::provisioning::discover_fabric_network;
use super::state::ClusterState;

#[derive(Args, Clone)]
pub struct DeleteArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Force deletion without confirmation
    #[arg(long, short)]
    pub force: bool,

    /// Skip fabric network deletion (useful for shared fabric networks)
    #[arg(long)]
    pub skip_fabric: bool,
}

pub async fn run(args: DeleteArgs, client: &TypedClient) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster).await?;

    let account = client.effective_account();
    let cluster_id = cluster.uuid.to_string();

    // 1. Discover cluster instances
    println!("Discovering cluster instances...");
    let machines = client
        .inner()
        .list_machines()
        .account(account)
        .send()
        .await?;

    // Filter instances by cluster tag
    let cluster_instances: Vec<_> = machines
        .into_inner()
        .into_iter()
        .filter(|m| {
            m.tags
                .get("k8s.cluster")
                .and_then(|v| v.as_str())
                .map(|v| v == cluster_id)
                .unwrap_or(false)
        })
        .collect();

    // 2. Display instance information and get confirmation
    if !cluster_instances.is_empty() {
        println!(
            "\nFound {} cluster instance{}:",
            cluster_instances.len(),
            if cluster_instances.len() == 1 {
                ""
            } else {
                "s"
            }
        );

        for machine in &cluster_instances {
            let id_str = machine.id.to_string();
            let short_id = &id_str[..8];
            let role = machine
                .tags
                .get("k8s.role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // Get IP info - show primary IP if available, otherwise note fabric-only
            let ip_display = if let Some(ref primary_ip) = machine.primary_ip {
                primary_ip.clone()
            } else {
                "fabric only".to_string()
            };

            println!(
                "  - {} ({}) ({}, {})",
                machine.name, short_id, role, ip_display
            );
        }

        // Confirm unless forced
        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!(
                    "\nDelete these {} instance{}?",
                    cluster_instances.len(),
                    if cluster_instances.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ))
                .default(false)
                .interact()?
            {
                println!("Deletion cancelled");
                return Ok(());
            }
        }

        // 3. Delete instances
        println!("\nDeleting instances...");
        let mut errors = Vec::new();
        let mut deleted_ids = Vec::new();

        for machine in cluster_instances {
            let id_str = machine.id.to_string();
            let short_id = &id_str[..8];

            print!("  Deleting {} ({})...", machine.name, short_id);

            match client
                .inner()
                .delete_machine()
                .account(account)
                .machine(machine.id)
                .send()
                .await
            {
                Ok(_) => {
                    println!(" done");
                    deleted_ids.push(machine.id);
                }
                Err(e) => {
                    println!(" failed: {}", e);
                    errors.push(format!("{}: {}", short_id, e));
                }
            }
        }

        if !errors.is_empty() {
            eprintln!(
                "\nWarning: {} instance{} failed to delete",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            );
        }

        // Wait for instances to be fully deleted before checking fabric
        // Only wait for network cleanup if we're not skipping fabric deletion
        if let (false, false, Some(fabric_id)) =
            (deleted_ids.is_empty(), args.skip_fabric, cluster.fabric_network_id)
        {
            println!();
            println!("Waiting for instances to be fully deleted...");
            let _ = std::io::stdout().flush();
            match wait_for_instances_deleted(&deleted_ids, 120, client).await {
                Ok(_) => println!("  All instances deleted"),
                Err(e) => {
                    eprintln!("  Warning: {}", e);
                }
            }

            // Also wait for network IPs to be released
            println!("Waiting for network IPs to be released...");
            let _ = std::io::stdout().flush();
            match wait_for_network_empty(fabric_id, 60, client).await {
                Ok(_) => println!("  Network IPs released"),
                Err(e) => {
                    eprintln!("  Warning: {}", e);
                }
            }

            // Additional delay to allow NAPI to fully release MAC addresses
            println!("Waiting for NAPI cleanup...");
            sleep(Duration::from_secs(5)).await;
            println!("  Done");
        }
    } else {
        println!("No instances found for cluster");
    }

    // 4. Handle fabric network cleanup (if cluster has one)
    if args.skip_fabric {
        if cluster.fabric_network_id.is_some() {
            println!("\nSkipping fabric network cleanup (--skip-fabric)");
        }
    } else if let Some(fabric_network_id) = cluster.fabric_network_id {
        println!("\nChecking fabric network...");

        // Try to get network info
        match discover_fabric_network(fabric_network_id, client).await {
            Ok(network_info) => {
                let network_id_str = fabric_network_id.to_string();
                let short_id = &network_id_str[..8];

                // Check if network has any instances still using it
                match client
                    .inner()
                    .list_network_ips()
                    .account(account)
                    .network(network_id_str.clone())
                    .send()
                    .await
                {
                    Ok(response) => {
                        let ips = response.into_inner();

                        // Count IPs that belong to instances (not just reserved)
                        let instance_ips: Vec<_> = ips
                            .iter()
                            .filter(|ip| ip.belongs_to_uuid.is_some())
                            .collect();

                        if !instance_ips.is_empty() {
                            println!(
                                "  Network '{}' ({}) has {} instance{} using it",
                                network_info.name,
                                short_id,
                                instance_ips.len(),
                                if instance_ips.len() == 1 { "" } else { "s" }
                            );
                            println!("  Skipping network deletion (network is not empty)");
                        } else {
                            println!("  Network '{}' ({}) is empty", network_info.name, short_id);

                            // Confirm deletion unless forced
                            let should_delete = if args.force {
                                true
                            } else {
                                use dialoguer::Confirm;
                                Confirm::new()
                                    .with_prompt(format!(
                                        "Delete fabric network '{}'?",
                                        network_info.name
                                    ))
                                    .default(false)
                                    .interact()?
                            };

                            if should_delete {
                                print!("  Deleting network...");
                                match client
                                    .inner()
                                    .delete_fabric_network()
                                    .account(account)
                                    .vlan_id(network_info.vlan_id)
                                    .id(fabric_network_id)
                                    .send()
                                    .await
                                {
                                    Ok(_) => println!(" done"),
                                    Err(e) => {
                                        println!(" failed: {}", e);
                                        eprintln!(
                                            "  Warning: Failed to delete fabric network: {}",
                                            e
                                        );
                                    }
                                }
                            } else {
                                println!("  Skipping network deletion");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "  Warning: Could not list network IPs: {}. Skipping network deletion.",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                let network_id_str = fabric_network_id.to_string();
                let short_id = &network_id_str[..8];
                println!(
                    "  Fabric network {} not found or already deleted: {}",
                    short_id, e
                );
            }
        }
    }

    // 5. Delete cluster state directory
    println!("\nDeleting cluster state...");
    cluster.delete().await?;
    println!("  Deleted cluster {} ({})", cluster.name, cluster.uuid);

    Ok(())
}

/// Wait for network to have no instance IPs
///
/// Polls until the network has no IPs belonging to instances
async fn wait_for_network_empty(
    network_id: Uuid,
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    let account = client.effective_account();
    let network_id_str = network_id.to_string();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let ips = client
            .inner()
            .list_network_ips()
            .account(account)
            .network(network_id_str.clone())
            .send()
            .await?
            .into_inner();

        // Count IPs that belong to instances (not just reserved)
        let instance_ips: Vec<_> = ips
            .iter()
            .filter(|ip| ip.belongs_to_uuid.is_some())
            .collect();

        if instance_ips.is_empty() {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for {} network IP{} to be released",
                instance_ips.len(),
                if instance_ips.len() == 1 { "" } else { "s" }
            ));
        }

        print!(".");
        let _ = std::io::stdout().flush();
        sleep(Duration::from_secs(2)).await;
    }
}

/// Wait for all instances to be fully deleted
///
/// Polls until instances no longer appear in list_machines or enter "deleted" state
async fn wait_for_instances_deleted(
    instance_ids: &[Uuid],
    timeout_secs: u64,
    client: &TypedClient,
) -> Result<()> {
    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        // Get all machines and check if any of our instances are still present and not deleted
        let machines = client
            .inner()
            .list_machines()
            .account(account)
            .send()
            .await?
            .into_inner();

        let still_present: Vec<_> = machines
            .iter()
            .filter(|m| {
                instance_ids.contains(&m.id)
                    && m.state != cloudapi_client::types::MachineState::Deleted
            })
            .collect();

        if still_present.is_empty() {
            return Ok(());
        }

        if start.elapsed() > timeout {
            let remaining: Vec<String> = still_present
                .iter()
                .map(|m| format!("{} ({})", m.name, &m.id.to_string()[..8]))
                .collect();
            return Err(anyhow::anyhow!(
                "Timeout waiting for instances to be deleted: {}",
                remaining.join(", ")
            ));
        }

        print!(".");
        let _ = std::io::stdout().flush();
        sleep(Duration::from_secs(2)).await;
    }
}
