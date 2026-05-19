// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Add nodes to a running Kubernetes cluster

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::AddNodesRequest;

use crate::output::json;

#[derive(Args, Clone)]
pub struct AddNodesArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Path to a JSON file containing AddNodesRequest (nodes array)
    #[arg(long)]
    pub config: PathBuf,
}

pub async fn run(args: AddNodesArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    // Read and deserialize the add-nodes request body.
    let raw = std::fs::read_to_string(&args.config)
        .with_context(|| format!("failed to read config file {}", args.config.display()))?;
    let body: AddNodesRequest = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse config file {} as AddNodesRequest",
            args.config.display()
        )
    })?;

    let result = client
        .inner()
        .k8s_cluster_nodes_add()
        .cluster(cluster.id)
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to add nodes: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!("Node configs submitted.");
    }

    Ok(())
}
