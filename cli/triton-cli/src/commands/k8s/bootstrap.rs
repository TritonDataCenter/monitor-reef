// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Bootstrap a Kubernetes cluster

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::{BootstrapClusterRequest, NodeBootstrapRole, NodeBootstrapSpec};

use crate::output::json;

#[derive(Args, Clone)]
pub struct BootstrapArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Fabric IP of a control-plane node (repeat for multiple)
    #[arg(long = "control-plane", value_name = "IP")]
    pub control_planes: Vec<String>,

    /// Fabric IP of a worker node (repeat for multiple)
    #[arg(long = "worker", value_name = "IP")]
    pub workers: Vec<String>,

    /// Talos installer image tag, e.g. "v1.12.7"
    #[arg(long)]
    pub talos_version: Option<String>,

    /// Disk to install Talos on, e.g. "/dev/sda"
    #[arg(long)]
    pub install_disk: Option<String>,
}

pub async fn run(args: BootstrapArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    if args.control_planes.is_empty() {
        anyhow::bail!("at least one --control-plane <ip> is required");
    }

    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    let mut nodes: Vec<NodeBootstrapSpec> = args
        .control_planes
        .iter()
        .map(|ip| NodeBootstrapSpec {
            fabric_ip: ip.clone(),
            role: NodeBootstrapRole::ControlPlane,
        })
        .collect();
    nodes.extend(args.workers.iter().map(|ip| NodeBootstrapSpec {
        fabric_ip: ip.clone(),
        role: NodeBootstrapRole::Worker,
    }));

    let body = BootstrapClusterRequest {
        nodes,
        talos_version: args.talos_version,
        install_disk: args.install_disk,
    };

    let result = client
        .inner()
        .k8s_cluster_bootstrap()
        .cluster(cluster.id)
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to bootstrap cluster: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!(
            "Bootstrap started. Poll `triton k8s get {}` until state is `running`.",
            &cluster.id.to_string()[..8]
        );
    }

    Ok(())
}
