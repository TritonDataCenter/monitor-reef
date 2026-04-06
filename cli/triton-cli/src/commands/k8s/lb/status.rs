// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Show status of the Triton LoadBalancer controller

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use super::super::kube_client;
use crate::commands::k8s::state::ClusterState;
use crate::output::json;

#[derive(Args, Clone)]
pub struct StatusArgs {
    /// Cluster name or UUID
    pub cluster: String,
}

#[derive(Serialize)]
struct ControllerStatus {
    installed: bool,
    ready: bool,
    replicas: Option<i32>,
    available_replicas: Option<i32>,
    pod_status: Option<String>,
}

pub async fn run(args: StatusArgs, use_json: bool) -> Result<()> {
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

    // Create Kubernetes client
    let k8s_client = kube_client::client_from_kubeconfig(&kubeconfig_path).await?;

    // Check if deployment exists
    let deployment =
        kube_client::get_deployment(&k8s_client, "triton-lb-controller", "kube-system")
            .await
            .context("Failed to get deployment")?;

    let status = match deployment {
        None => {
            let status = ControllerStatus {
                installed: false,
                ready: false,
                replicas: None,
                available_replicas: None,
                pod_status: None,
            };

            if use_json {
                json::print_json(&status)?;
            } else {
                eprintln!(
                    "LoadBalancer controller is not installed in cluster '{}'",
                    cluster.name
                );
                eprintln!();
                eprintln!("Install it with:");
                eprintln!("  triton k8s lb install {}", cluster.name);
            }
            return Ok(());
        }
        Some(dep) => {
            let replicas = dep.spec.as_ref().and_then(|s| s.replicas);
            let available_replicas = dep.status.as_ref().and_then(|s| s.available_replicas);
            let ready = available_replicas.unwrap_or(0) >= replicas.unwrap_or(1);

            // Get pod status
            let pod_status = kube_client::get_pod_status_by_label(
                &k8s_client,
                "kube-system",
                "app=triton-lb-controller",
            )
            .await
            .context("Failed to get pod status")?;

            ControllerStatus {
                installed: true,
                ready,
                replicas,
                available_replicas,
                pod_status,
            }
        }
    };

    if use_json {
        json::print_json(&status)?;
    } else {
        eprintln!("LoadBalancer Controller Status");
        eprintln!("==============================");
        eprintln!(
            "Cluster:    {} ({})",
            cluster.name,
            &cluster.uuid.to_string()[..8]
        );
        eprintln!(
            "Installed:  {}",
            if status.installed { "yes" } else { "no" }
        );
        eprintln!("Ready:      {}", if status.ready { "yes" } else { "no" });
        if let Some(replicas) = status.replicas {
            eprintln!(
                "Replicas:   {}/{}",
                status.available_replicas.unwrap_or(0),
                replicas
            );
        }
        if let Some(ref pod_status) = status.pod_status {
            eprintln!("Pod Status: {}", pod_status);
        }

        if status.installed && status.ready {
            eprintln!();
            eprintln!("Controller is running. View logs with:");
            eprintln!(
                "  kubectl --kubeconfig {} logs -n kube-system deploy/triton-lb-controller -f",
                kubeconfig_path.display()
            );
        }
    }

    Ok(())
}
