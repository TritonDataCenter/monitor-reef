// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Remove the Triton LoadBalancer controller from a Kubernetes cluster

use anyhow::{Context, Result, bail};
use clap::Args;

use super::super::kube_client::{
    self, K8sClusterRole, K8sClusterRoleBinding, K8sConfigMap, K8sDeployment, K8sSecret,
    K8sServiceAccount,
};
use crate::commands::k8s::state::ClusterState;

#[derive(Args, Clone)]
pub struct RemoveArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub async fn run(args: RemoveArgs, _json: bool) -> Result<()> {
    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    let kubeconfig_path = cluster.cluster_dir()?.join("kubeconfig");
    if !kubeconfig_path.exists() {
        bail!(
            "Kubeconfig not found at {}. Has the cluster been bootstrapped?",
            kubeconfig_path.display()
        );
    }

    if !args.yes {
        eprintln!(
            "This will remove the Triton LoadBalancer controller from cluster '{}'.",
            cluster.name
        );
        eprintln!("Any existing LoadBalancer services will stop working.");
        eprintln!();
        eprint!("Continue? [y/N] ");

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read input")?;

        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    eprintln!("==> Removing LoadBalancer controller");

    // Create Kubernetes client
    let k8s_client = kube_client::client_from_kubeconfig(&kubeconfig_path).await?;

    // Delete deployment
    eprintln!("    Deleting deployment...");
    kube_client::delete_namespaced::<K8sDeployment>(
        &k8s_client,
        "triton-lb-controller",
        "kube-system",
    )
    .await?;

    // Delete configmap
    eprintln!("    Deleting configmap...");
    kube_client::delete_namespaced::<K8sConfigMap>(
        &k8s_client,
        "triton-lb-controller-config",
        "kube-system",
    )
    .await?;

    // Delete secret
    eprintln!("    Deleting credentials secret...");
    kube_client::delete_namespaced::<K8sSecret>(&k8s_client, "triton-credentials", "kube-system")
        .await?;

    // Delete RBAC resources
    eprintln!("    Deleting RBAC resources...");
    kube_client::delete_cluster_scoped::<K8sClusterRoleBinding>(
        &k8s_client,
        "triton-lb-controller",
    )
    .await?;
    kube_client::delete_cluster_scoped::<K8sClusterRole>(&k8s_client, "triton-lb-controller")
        .await?;
    kube_client::delete_namespaced::<K8sServiceAccount>(
        &k8s_client,
        "triton-lb-controller",
        "kube-system",
    )
    .await?;

    eprintln!();
    eprintln!("==> LoadBalancer controller removed successfully");

    Ok(())
}
