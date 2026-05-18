// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Roll the three control-plane static pods (kube-apiserver,
//! kube-controller-manager, kube-scheduler) one node at a time.
//!
//! For each service, in talosctl-compatible order:
//!   1. Patch the local `controlplane.yaml` so the image tag matches the
//!      target. This is the source of truth for future `control add` calls.
//!   2. For each control plane node, sequentially:
//!      a. `ApplyConfiguration(NoReboot)` so Talos re-renders the static
//!         pod manifest with the new image.
//!      b. Watch the pod in kube-system with `k8s-app=<service>` running on
//!         this node until it is `Ready` *and* every container is running
//!         the target image.
//!      c. Bound by `--per-node-timeout`.

use anyhow::{Context, Result, bail};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ListParams};
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::state::ClusterState;
use super::super::talos::apply_config;
use super::ResolvedImages;
use super::discovery::Plan;
use super::machine_config::{self, ImageField};

/// Inner loop for one control-plane service across every control plane node.
async fn upgrade_service(
    plan: &Plan,
    service: &str,
    label: &str,
    field: ImageField,
    new_image: &str,
    controlplane_yaml: &Path,
    talosconfig: &Path,
    kubeconfig: &Path,
    per_node_timeout: Duration,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("==> Upgrading {} to {}", service, plan.to_version);
    }

    // Step 1: write the new image into the local controlplane.yaml.
    let mut doc = machine_config::load_yaml(controlplane_yaml).await?;
    let prev = machine_config::set_image(&mut doc, field, new_image)?;
    machine_config::save_yaml(controlplane_yaml, &doc).await?;
    if !json {
        eprintln!(
            "    Local config: {} {} -> {}",
            label,
            prev.as_deref().unwrap_or("<unset>"),
            new_image
        );
    }

    // Step 2: roll each control plane node, one at a time.
    let kube_client = super::super::kube_client::client_from_kubeconfig(kubeconfig).await?;
    let talosconfig_str = talosconfig.to_string_lossy().to_string();
    let cluster_dir = controlplane_yaml
        .parent()
        .ok_or_else(|| anyhow::anyhow!("controlplane.yaml has no parent directory"))?;

    for node in &plan.control_plane {
        if !json {
            eprintln!("    -> {}", node.name);
        }

        // Always include the per-node network patch so hostname / IP
        // settings configured at bootstrap are preserved on re-apply.
        let patch = machine_config::network_patch_path(cluster_dir, &node.name).await;
        let patch_refs: Vec<&Path> = patch.iter().map(|p| p.as_path()).collect();

        // ApplyConfiguration with the patched YAML — NoReboot is what
        // apply_config::run uses internally.
        apply_config::run(
            &node.talos_endpoint,
            controlplane_yaml,
            &patch_refs,
            Some(&talosconfig_str),
            false,
            false,
        )
        .await
        .with_context(|| format!("ApplyConfiguration on {}", node.name))?;

        wait_for_pod_running_image(
            &kube_client,
            &node.name,
            service,
            new_image,
            per_node_timeout,
            json,
        )
        .await
        .with_context(|| {
            format!(
                "waiting for {} on {} to reach {}",
                service, node.name, new_image
            )
        })?;

        if !json {
            eprintln!("    -> {}: {} updated", node.name, service);
        }
    }

    if !json {
        eprintln!();
    }
    Ok(())
}

pub async fn run(
    plan: &Plan,
    state: &ClusterState,
    controlplane_yaml: &Path,
    talosconfig: &Path,
    kubeconfig: &Path,
    images: &ResolvedImages,
    per_node_timeout: Duration,
    json: bool,
) -> Result<()> {
    let _ = state; // currently unused; kept for signature symmetry with siblings

    for (service, label, field, image) in [
        (
            "kube-apiserver",
            "apiserver",
            ImageField::ApiServer,
            images.apiserver.as_str(),
        ),
        (
            "kube-controller-manager",
            "controller-manager",
            ImageField::ControllerManager,
            images.controller_manager.as_str(),
        ),
        (
            "kube-scheduler",
            "scheduler",
            ImageField::Scheduler,
            images.scheduler.as_str(),
        ),
    ] {
        upgrade_service(
            plan,
            service,
            label,
            field,
            image,
            controlplane_yaml,
            talosconfig,
            kubeconfig,
            per_node_timeout,
            json,
        )
        .await?;
    }

    Ok(())
}

/// Poll until a pod matching `k8s-app=<service>` on `node_name` is `Ready`
/// and every container is running `target_image`.
async fn wait_for_pod_running_image(
    client: &kube::Client,
    node_name: &str,
    service: &str,
    target_image: &str,
    timeout: Duration,
    json: bool,
) -> Result<()> {
    let api: Api<Pod> = Api::namespaced(client.clone(), "kube-system");
    let selector = format!("k8s-app={}", service);
    let lp = ListParams::default().labels(&selector);
    let start = Instant::now();
    let poll = Duration::from_secs(3);
    let mut last_log = Instant::now() - poll;

    loop {
        if start.elapsed() > timeout {
            bail!(
                "timeout after {:?} waiting for {} on {} to run image {}",
                timeout,
                service,
                node_name,
                target_image
            );
        }

        // The Kubernetes API may briefly go away while a control-plane
        // static pod restarts on the same node our kubeconfig points at.
        // Treat list errors as transient during the poll window — the
        // outer timeout still bounds the wait.
        let pod_list = match api.list(&lp).await {
            Ok(l) => l,
            Err(e) => {
                if !json && last_log.elapsed() >= Duration::from_secs(10) {
                    eprintln!("       waiting: kube API not reachable yet ({})", e);
                    last_log = Instant::now();
                }
                tokio::time::sleep(poll).await;
                continue;
            }
        };
        let candidate = pod_list
            .items
            .into_iter()
            .find(|p| p.spec.as_ref().and_then(|s| s.node_name.clone()) == Some(node_name.into()));

        if let Some(pod) = candidate {
            let ready = pod
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .map(|conds| {
                    conds
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                })
                .unwrap_or(false);

            let all_target = pod
                .status
                .as_ref()
                .and_then(|s| s.container_statuses.as_ref())
                .map(|cs| cs.iter().all(|c| c.image.contains(target_image)))
                .unwrap_or(false);

            if ready && all_target {
                return Ok(());
            }

            if !json && last_log.elapsed() >= Duration::from_secs(10) {
                let image_str = pod
                    .status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .and_then(|cs| cs.first())
                    .map(|c| c.image.clone())
                    .unwrap_or_else(|| "<unknown>".into());
                eprintln!(
                    "       waiting: ready={} image={} target={}",
                    ready, image_str, target_image
                );
                last_log = Instant::now();
            }
        } else if !json && last_log.elapsed() >= Duration::from_secs(10) {
            eprintln!(
                "       waiting: no pod for {} on {} yet",
                service, node_name
            );
            last_log = Instant::now();
        }

        tokio::time::sleep(poll).await;
    }
}
