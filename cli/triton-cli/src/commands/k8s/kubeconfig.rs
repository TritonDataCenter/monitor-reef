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

    /// Override the server URL in the kubeconfig (defaults to the relay bridge
    /// address https://127.0.0.1:6443)
    #[arg(long, default_value = "https://127.0.0.1:6443")]
    pub server: String,
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

    let kubeconfig = patch_server_url(&response.kubeconfig, &args.server);

    if args.stdout {
        print!("{}", kubeconfig);
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

    std::fs::write(&output_path, kubeconfig.as_bytes())
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

/// Replace every `server:` value in the kubeconfig YAML with `server_url`.
fn patch_server_url(kubeconfig: &str, server_url: &str) -> String {
    kubeconfig
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("server:") {
                let indent = &line[..line.len() - trimmed.len()];
                format!("{}server: {}", indent, server_url)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
