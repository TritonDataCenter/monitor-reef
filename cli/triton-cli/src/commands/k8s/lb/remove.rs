// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;

use crate::commands::k8s::resolve_cluster;

#[derive(Args, Clone)]
pub struct RemoveArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,
}

pub async fn run(args: RemoveArgs, client: &TypedClient, _json: bool) -> Result<()> {
    let cluster = resolve_cluster(&args.cluster, client).await?;

    client
        .inner()
        .k8s_cluster_lb_remove()
        .cluster(cluster.id)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("lb remove failed: {e}"))?;

    println!("LB controller removed from cluster '{}'.", cluster.name);

    Ok(())
}
