// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Output kubeconfig for a cluster

use anyhow::Result;
use clap::Args;

use super::state::ClusterState;

#[derive(Args, Clone)]
pub struct KubeconfigArgs {
    /// Cluster name or UUID
    pub cluster: String,
}

pub async fn run(args: KubeconfigArgs) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster).await?;
    let kubeconfig_path = cluster.cluster_dir()?.join("kubeconfig");

    if !kubeconfig_path.exists() {
        anyhow::bail!("Kubeconfig not found for cluster {}", cluster.name);
    }

    let kubeconfig = tokio::fs::read_to_string(&kubeconfig_path).await?;
    print!("{}", kubeconfig);

    Ok(())
}
