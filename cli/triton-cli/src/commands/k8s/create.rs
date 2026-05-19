// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Create a Kubernetes cluster record

use anyhow::Result;
use clap::Args;
use triton_gateway_client::TypedClient;
use triton_gateway_client::types::CreateClusterRequest;

use super::get::print_cluster_kv;
use crate::output::json;

#[derive(Args, Clone)]
pub struct CreateArgs {
    /// Cluster name
    #[arg(long)]
    pub name: String,

    /// Optional description
    #[arg(long)]
    pub description: Option<String>,

    /// Triton fabric network UUID
    #[arg(long)]
    pub fabric: Option<uuid::Uuid>,
}

pub async fn run(args: CreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let body = CreateClusterRequest {
        name: args.name,
        description: args.description,
        fabric_network_id: args.fabric,
    };

    let cluster = client
        .inner()
        .k8s_clusters_create()
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to create cluster: {}", e))?
        .into_inner();

    if use_json {
        json::print_json(&cluster)?;
    } else {
        print_cluster_kv(&cluster);
    }

    Ok(())
}
