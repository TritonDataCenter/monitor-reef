// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Delete a Kubernetes cluster record

use anyhow::Result;
use clap::Args;
use dialoguer::Confirm;
use triton_gateway_client::TypedClient;

#[derive(Args, Clone)]
pub struct DeleteArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,
}

pub async fn run(args: DeleteArgs, client: &TypedClient) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    // Confirm before deleting.
    let confirmed = Confirm::new()
        .with_prompt(format!(
            "Delete cluster {} ({})? This cannot be undone.",
            cluster.name,
            &cluster.id.to_string()[..8]
        ))
        .default(false)
        .interact()
        .map_err(|e| anyhow::anyhow!("failed to read confirmation: {}", e))?;

    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    println!(
        "Deleting cluster {} ({})...",
        cluster.name,
        &cluster.id.to_string()[..8]
    );

    client
        .inner()
        .k8s_clusters_delete()
        .cluster(cluster.id)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete cluster: {}", e))?;

    Ok(())
}
