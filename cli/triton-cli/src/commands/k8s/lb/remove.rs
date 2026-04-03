// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Remove the Triton LoadBalancer controller from a Kubernetes cluster

use anyhow::{Context, Result, bail};
use clap::Args;
use std::process::Command;

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

    // Delete deployment
    eprintln!("    Deleting deployment...");
    let _ = kubectl_delete(
        &kubeconfig_path,
        "deployment",
        "triton-lb-controller",
        "kube-system",
    );

    // Delete configmap
    eprintln!("    Deleting configmap...");
    let _ = kubectl_delete(
        &kubeconfig_path,
        "configmap",
        "triton-lb-controller-config",
        "kube-system",
    );

    // Delete secret
    eprintln!("    Deleting credentials secret...");
    let _ = kubectl_delete(
        &kubeconfig_path,
        "secret",
        "triton-credentials",
        "kube-system",
    );

    // Delete RBAC resources
    eprintln!("    Deleting RBAC resources...");
    let _ = kubectl_delete(
        &kubeconfig_path,
        "clusterrolebinding",
        "triton-lb-controller",
        "",
    );
    let _ = kubectl_delete(&kubeconfig_path, "clusterrole", "triton-lb-controller", "");
    let _ = kubectl_delete(
        &kubeconfig_path,
        "serviceaccount",
        "triton-lb-controller",
        "kube-system",
    );

    eprintln!();
    eprintln!("==> LoadBalancer controller removed successfully");

    Ok(())
}

/// Delete a Kubernetes resource
fn kubectl_delete(
    kubeconfig: &std::path::Path,
    kind: &str,
    name: &str,
    namespace: &str,
) -> Result<()> {
    let mut cmd = Command::new("kubectl");
    cmd.arg("--kubeconfig")
        .arg(kubeconfig)
        .arg("delete")
        .arg(kind)
        .arg(name)
        .arg("--ignore-not-found");

    if !namespace.is_empty() {
        cmd.arg("-n").arg(namespace);
    }

    let output = cmd.output().context("Failed to run kubectl delete")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("kubectl delete {} {} failed: {}", kind, name, stderr);
    }

    Ok(())
}
