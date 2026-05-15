// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pre-pull container images on relevant nodes via Talos `ImagePull` RPC.
//!
//! Control plane images (apiserver, controller-manager, scheduler, proxy)
//! pull into the CRI namespace on each control plane node. Kubelet images
//! pull into the SYSTEM namespace on every node.

use anyhow::{Context, Result};
use tonic::Code;

use super::super::state::ClusterState;
use super::super::talos::client::{self, NodeTargetInterceptor};
use super::super::talos::proto::{common, machine};
use super::ResolvedImages;
use super::discovery::Plan;

pub async fn run(
    plan: &Plan,
    _state: &ClusterState,
    talosconfig: &std::path::Path,
    images: &ResolvedImages,
    upgrade_kubelet: bool,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("==> Pre-pulling images");
    }

    let talosconfig = talosconfig.to_string_lossy().to_string();

    // Control plane component images on every control plane node, into NS_CRI.
    let cp_images = [
        ("apiserver", &images.apiserver),
        ("controller-manager", &images.controller_manager),
        ("scheduler", &images.scheduler),
        ("proxy", &images.proxy),
    ];

    for node in &plan.control_plane {
        for (label, image) in cp_images.iter() {
            pull_one(
                &node.name,
                &node.talos_endpoint,
                node.fabric_target.as_deref(),
                image,
                common::ContainerdNamespace::NsCri,
                &talosconfig,
                json,
                label,
            )
            .await?;
        }
    }

    // Kubelet image on every node, into NS_SYSTEM.
    if upgrade_kubelet {
        let all_nodes: Vec<&super::discovery::PlanNode> = plan
            .control_plane
            .iter()
            .chain(plan.workers.iter())
            .collect();
        for node in all_nodes {
            pull_one(
                &node.name,
                &node.talos_endpoint,
                node.fabric_target.as_deref(),
                &images.kubelet,
                common::ContainerdNamespace::NsSystem,
                &talosconfig,
                json,
                "kubelet",
            )
            .await?;
        }
    }

    if !json {
        eprintln!();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn pull_one(
    node_name: &str,
    endpoint: &str,
    fabric_target: Option<&str>,
    image: &str,
    namespace: common::ContainerdNamespace,
    talosconfig: &str,
    json: bool,
    label: &str,
) -> Result<()> {
    let channel = client::connect(endpoint, Some(talosconfig), false).await?;
    let req = machine::ImagePullRequest {
        namespace: namespace as i32,
        reference: image.to_string(),
    };

    let result = if let Some(target) = fabric_target {
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel,
            interceptor,
        );
        client.image_pull(req).await
    } else {
        let mut client = machine::machine_service_client::MachineServiceClient::new(channel);
        client.image_pull(req).await
    };

    match result {
        Ok(resp) => {
            let inner = resp.into_inner();
            for m in &inner.messages {
                if let Some(ref meta) = m.metadata
                    && !meta.error.is_empty()
                {
                    if !json {
                        eprintln!("    {} {}: pull error: {}", node_name, label, meta.error);
                    }
                    anyhow::bail!("image pull failed on {}: {}", node_name, meta.error);
                }
            }
            if !json {
                eprintln!("    {} {}: pulled {}", node_name, label, image);
            }
        }
        Err(status) if status.code() == Code::Unimplemented => {
            if !json {
                eprintln!(
                    "    {} {}: ImagePull not implemented on this Talos version, skipping",
                    node_name, label
                );
            }
        }
        Err(status) => {
            return Err(anyhow::Error::from(status))
                .with_context(|| format!("ImagePull on {} for {}", node_name, image));
        }
    }
    Ok(())
}
