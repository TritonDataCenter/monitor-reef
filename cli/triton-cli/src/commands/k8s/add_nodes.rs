// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Add nodes to a running Kubernetes cluster

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::{AddNodesRequest, NodeBootstrapRole, NodeBootstrapSpec};

use crate::output::json;

#[derive(Args, Clone)]
pub struct AddNodesArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Fabric IP of a control-plane node to add (repeat for multiple)
    #[arg(long = "control-plane", value_name = "IP")]
    pub control_planes: Vec<String>,

    /// Fabric IP of a worker node to add (repeat for multiple)
    #[arg(long = "worker", value_name = "IP")]
    pub workers: Vec<String>,
}

pub async fn run(args: AddNodesArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    if args.control_planes.is_empty() && args.workers.is_empty() {
        anyhow::bail!("at least one --control-plane <ip> or --worker <ip> is required");
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

    let result = client
        .inner()
        .k8s_cluster_nodes_add()
        .cluster(cluster.id)
        .body(AddNodesRequest { nodes })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to add nodes: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!("Node configs submitted. Nodes will join the cluster after reboot.");
    }

    Ok(())
}
