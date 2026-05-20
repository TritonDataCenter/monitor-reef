// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Retrieve the kubeconfig for a Kubernetes cluster

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use triton_gateway_client::TypedClient;

#[derive(Args, Clone)]
pub struct KubeconfigArgs {
    /// Cluster UUID, short ID prefix, or name
    pub cluster: String,

    /// Write kubeconfig to this file instead of ~/.kube/config
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,

    /// Print kubeconfig to stdout instead of writing to a file
    #[arg(long)]
    pub stdout: bool,
}

pub async fn run(args: KubeconfigArgs, client: &TypedClient) -> Result<()> {
    let cluster = super::resolve_cluster(&args.cluster, client).await?;

    let response = client
        .inner()
        .k8s_cluster_kubeconfig()
        .cluster(cluster.id)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to get kubeconfig: {}", e))?
        .into_inner();

    if args.stdout {
        print!("{}", response.kubeconfig);
        return Ok(());
    }

    // Determine the output path: explicit --output or default ~/.kube/config.
    let output_path = if let Some(path) = args.output {
        path
    } else {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        let kube_dir = home.join(".kube");
        std::fs::create_dir_all(&kube_dir)
            .with_context(|| format!("failed to create {}", kube_dir.display()))?;
        kube_dir.join("config")
    };

    std::fs::write(&output_path, response.kubeconfig.as_bytes())
        .with_context(|| format!("failed to write kubeconfig to {}", output_path.display()))?;

    // Set permissions to 0600 on Unix systems.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", output_path.display()))?;
    }

    eprintln!("Written to {}", output_path.display());
    Ok(())
}
