// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::InstallLbRequest;

use crate::commands::k8s::resolve_cluster;
use crate::output::json;

#[derive(Args, Clone)]
pub struct InstallArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Triton package for LoadBalancer VMs
    #[arg(long, default_value = "sample-1G")]
    pub package: String,

    /// Image name or UUID for LB VMs (default: newest "cloud-load-balancer")
    #[arg(long)]
    pub image: Option<String>,

    /// Override external CNS suffix (auto-discovered if absent)
    #[arg(long)]
    pub external_cns_suffix: Option<String>,

    /// Controller container image
    #[arg(long, default_value = "travispaul/triton-lb-controller:latest")]
    pub controller_image: String,
}

pub async fn run(args: InstallArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = resolve_cluster(&args.cluster, client).await?;

    let result = client
        .inner()
        .k8s_cluster_lb_install()
        .cluster(cluster.id)
        .body(InstallLbRequest {
            package: args.package,
            image: args.image,
            external_cns_suffix: args.external_cns_suffix,
            controller_image: args.controller_image,
        })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("lb install failed: {e}"))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!(
            "Installing LB controller for cluster '{}'. Check status with: triton k8s lb status {}",
            result.name, result.name
        );
    }

    Ok(())
}
