// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Show status of the Triton LoadBalancer controller

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;
use std::process::Command;

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

    // Check if deployment exists
    let deployment_output = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(&kubeconfig_path)
        .arg("get")
        .arg("deployment")
        .arg("triton-lb-controller")
        .arg("-n")
        .arg("kube-system")
        .arg("-o")
        .arg("json")
        .output()
        .context("Failed to run kubectl")?;

    if !deployment_output.status.success() {
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

    // Parse deployment status
    let deployment: serde_json::Value = serde_json::from_slice(&deployment_output.stdout)
        .context("Failed to parse deployment JSON")?;

    let replicas = deployment["spec"]["replicas"].as_i64().map(|v| v as i32);
    let available_replicas = deployment["status"]["availableReplicas"]
        .as_i64()
        .map(|v| v as i32);

    let ready = available_replicas.unwrap_or(0) >= replicas.unwrap_or(1);

    // Get pod status
    let pod_output = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(&kubeconfig_path)
        .arg("get")
        .arg("pods")
        .arg("-n")
        .arg("kube-system")
        .arg("-l")
        .arg("app=triton-lb-controller")
        .arg("-o")
        .arg("jsonpath={.items[0].status.phase}")
        .output()
        .context("Failed to get pod status")?;

    let pod_status = if pod_output.status.success() {
        let status = String::from_utf8_lossy(&pod_output.stdout)
            .trim()
            .to_string();
        if status.is_empty() {
            None
        } else {
            Some(status)
        }
    } else {
        None
    };

    let status = ControllerStatus {
        installed: true,
        ready,
        replicas,
        available_replicas,
        pod_status,
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
