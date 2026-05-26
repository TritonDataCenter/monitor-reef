// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;

use crate::commands::k8s::resolve_cluster;
use crate::output::json;

#[derive(Args, Clone)]
pub struct StatusArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,
}

pub async fn run(args: StatusArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = resolve_cluster(&args.cluster, client).await?;

    let status = client
        .inner()
        .k8s_cluster_lb_status()
        .cluster(cluster.id)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("lb status failed: {e}"))?
        .into_inner();

    if use_json {
        json::print_json(&status)?;
    } else {
        println!("LB Controller Status — cluster '{}'", cluster.name);
        println!(
            "  Installed: {}",
            if status.installed { "yes" } else { "no" }
        );
        println!("  Ready:     {}", if status.ready { "yes" } else { "no" });
        if let Some(r) = status.replicas {
            println!(
                "  Replicas:  {}/{}",
                status.available_replicas.unwrap_or(0),
                r
            );
        }
        if !status.installed {
            println!();
            println!("Install with: triton k8s lb install {}", cluster.name);
        }
    }

    Ok(())
}
