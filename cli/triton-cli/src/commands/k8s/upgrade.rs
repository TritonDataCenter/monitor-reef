// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Upgrade Talos on all nodes in a Kubernetes cluster

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::UpgradeClusterRequest;

use crate::output::json;

#[derive(Args, Clone)]
pub struct UpgradeArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Talos image reference (e.g. ghcr.io/siderolabs/talos:v1.8.0)
    #[arg(long)]
    pub image: String,

    /// Preserve data on upgrade (default: false)
    #[arg(long, default_value_t = false)]
    pub preserve: bool,
}

pub async fn run(args: UpgradeArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    let body = UpgradeClusterRequest {
        talos_image: args.image,
        preserve: args.preserve,
    };

    let result = client
        .inner()
        .k8s_cluster_upgrade()
        .cluster(cluster.id)
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to upgrade cluster: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!("Upgrade started. Nodes will reboot sequentially.");
    }

    Ok(())
}
