// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Bootstrap a Kubernetes cluster

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::BootstrapClusterRequest;

use crate::output::json;

#[derive(Args, Clone)]
pub struct BootstrapArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Number of control-plane nodes to provision
    #[arg(long, default_value = "1")]
    pub control_plane_count: u32,

    /// Number of worker nodes to provision
    #[arg(long, default_value = "0")]
    pub worker_count: u32,

    /// CloudAPI package name or UUID for each node VM
    #[arg(long, default_value = "sample-2G")]
    pub package: String,

    /// CloudAPI image name or UUID (must be a Talos nocloud image)
    #[arg(long, default_value = "talos-1.12-nocloud")]
    pub image: String,

    /// Talos installer image tag, e.g. "v1.12.7"
    #[arg(long)]
    pub talos_version: Option<String>,

    /// Disk to install Talos on, e.g. "/dev/sda"
    #[arg(long)]
    pub install_disk: Option<String>,
}

pub async fn run(args: BootstrapArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    if args.control_plane_count == 0 {
        anyhow::bail!("--control-plane-count must be at least 1");
    }

    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    let body = BootstrapClusterRequest {
        control_plane_count: args.control_plane_count,
        worker_count: args.worker_count,
        package: args.package,
        image: args.image,
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
