// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Get a Kubernetes cluster record

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::{Cluster, ClusterState};

use crate::output::{enum_to_display, json};

#[derive(Args, Clone)]
pub struct GetArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Poll until the cluster reaches a terminal state (running or degraded)
    #[arg(long, short = 'w')]
    pub watch: bool,
}

pub async fn run(args: GetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    if !args.watch {
        if use_json {
            json::print_json(&cluster)?;
        } else {
            print_cluster_kv(&cluster);
        }
        return Ok(());
    }

    // Watch mode: poll every 10s until terminal state.
    let mut current = cluster;
    loop {
        if use_json {
            json::print_json(&current)?;
        } else {
            print_cluster_kv(&current);
            println!();
        }

        match current.state {
            ClusterState::Running | ClusterState::Degraded => break,
            _ => {}
        }

        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        current = super::resolve_cluster(&args.cluster, client).await?;
    }

    Ok(())
}

/// Print cluster fields as key-value pairs (matches `triton instance get` style).
pub fn print_cluster_kv(c: &Cluster) {
    println!("id:                   {}", c.id);
    println!("name:                 {}", c.name);
    println!("state:                {}", enum_to_display(&c.state));
    println!(
        "description:          {}",
        c.description.as_deref().unwrap_or("-")
    );
    println!("control_plane_count:  {}", c.control_plane_count);
    println!("worker_count:         {}", c.worker_count);
    println!(
        "endpoint:             {}",
        c.endpoint.as_deref().unwrap_or("-")
    );
    println!(
        "talos_version:        {}",
        c.talos_version.as_deref().unwrap_or("-")
    );
    println!(
        "kubernetes_version:   {}",
        c.kubernetes_version.as_deref().unwrap_or("-")
    );
    println!(
        "fabric_network_id:    {}",
        c.fabric_network_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("created_at:           {}", c.created_at.to_rfc3339());
}
