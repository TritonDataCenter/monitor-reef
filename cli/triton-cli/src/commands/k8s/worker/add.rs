// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::AddWorkersRequest;

use crate::commands::k8s::resolve_cluster;
use crate::output::json;

#[derive(Args, Clone)]
pub struct WorkerAddArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Number of worker nodes to provision
    #[arg(long, short = 'n', default_value = "1")]
    pub count: u32,

    /// Triton package for the worker VMs
    #[arg(long, short = 'p')]
    pub package: String,
}

pub async fn run(args: WorkerAddArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let cluster = resolve_cluster(&args.cluster, client).await?;

    let result = client
        .inner()
        .k8s_cluster_workers_add()
        .cluster(cluster.id)
        .body(AddWorkersRequest {
            count: args.count,
            package: args.package,
        })
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to add workers: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&result)?;
    } else {
        println!(
            "Provisioning {} worker(s) for cluster '{}'. Nodes will join after reboot.",
            args.count, result.name
        );
    }

    Ok(())
}
