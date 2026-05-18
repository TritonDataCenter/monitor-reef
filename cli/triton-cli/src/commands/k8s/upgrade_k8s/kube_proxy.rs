// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Roll kube-proxy by patching `cluster.proxy.image` on each control plane
//! node, then waiting for the kube-proxy DaemonSet to finish its rollout.

use anyhow::{Context, Result, bail};
use k8s_openapi::api::apps::v1::DaemonSet;
use kube::api::Api;
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::state::ClusterState;
use super::super::talos::apply_config;
use super::ResolvedImages;
use super::discovery::Plan;
use super::machine_config::{self, ImageField};

pub async fn run(
    plan: &Plan,
    _state: &ClusterState,
    controlplane_yaml: &Path,
    talosconfig: &Path,
    kubeconfig: &Path,
    images: &ResolvedImages,
    per_node_timeout: Duration,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("==> Upgrading kube-proxy to {}", plan.to_version);
    }

    let mut doc = machine_config::load_yaml(controlplane_yaml).await?;
    let prev = machine_config::set_image(&mut doc, ImageField::Proxy, &images.proxy)?;
    machine_config::save_yaml(controlplane_yaml, &doc).await?;
    if !json {
        eprintln!(
            "    Local config: proxy {} -> {}",
            prev.as_deref().unwrap_or("<unset>"),
            images.proxy
        );
    }

    let talosconfig_str = talosconfig.to_string_lossy().to_string();
    let cluster_dir = controlplane_yaml
        .parent()
        .ok_or_else(|| anyhow::anyhow!("controlplane.yaml has no parent directory"))?;
    for node in &plan.control_plane {
        if !json {
            eprintln!("    -> {}", node.name);
        }
        let patch = machine_config::network_patch_path(cluster_dir, &node.name).await;
        let patch_refs: Vec<&Path> = patch.iter().map(|p| p.as_path()).collect();
        apply_config::run(
            &node.talos_endpoint,
            controlplane_yaml,
            &patch_refs,
            Some(&talosconfig_str),
            false,
            false,
        )
        .await
        .with_context(|| format!("ApplyConfiguration on {} for kube-proxy", node.name))?;
    }

    // Wait for the DaemonSet rollout to complete.
    let kube_client = super::super::kube_client::client_from_kubeconfig(kubeconfig).await?;
    wait_for_daemonset_updated(&kube_client, "kube-proxy", per_node_timeout, json).await?;

    if !json {
        eprintln!("    kube-proxy upgraded.");
        eprintln!();
    }

    Ok(())
}

async fn wait_for_daemonset_updated(
    client: &kube::Client,
    name: &str,
    timeout: Duration,
    json: bool,
) -> Result<()> {
    let api: Api<DaemonSet> = Api::namespaced(client.clone(), "kube-system");
    let start = Instant::now();
    let poll = Duration::from_secs(3);
    let mut last_log = Instant::now() - poll;

    loop {
        if start.elapsed() > timeout {
            bail!(
                "timeout after {:?} waiting for DaemonSet kube-system/{} to finish rollout",
                timeout,
                name
            );
        }

        let ds = match api.get_opt(name).await {
            Ok(d) => d,
            Err(e) => {
                if !json && last_log.elapsed() >= Duration::from_secs(10) {
                    eprintln!("       waiting: kube API not reachable yet ({})", e);
                    last_log = Instant::now();
                }
                tokio::time::sleep(poll).await;
                continue;
            }
        };

        if let Some(ds) = ds {
            let status = ds.status.as_ref();
            let desired = status.map(|s| s.desired_number_scheduled).unwrap_or(0);
            let updated = status.and_then(|s| s.updated_number_scheduled).unwrap_or(0);
            let ready = status.map(|s| s.number_ready).unwrap_or(0);

            if updated >= desired && ready >= desired && desired > 0 {
                return Ok(());
            }

            if !json && last_log.elapsed() >= Duration::from_secs(10) {
                eprintln!(
                    "       waiting: updated={}/{} ready={}/{}",
                    updated, desired, ready, desired
                );
                last_log = Instant::now();
            }
        }

        tokio::time::sleep(poll).await;
    }
}
