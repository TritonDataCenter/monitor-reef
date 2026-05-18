// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Trigger Talos upgrade via gRPC

use anyhow::{Context, Result};

use super::client::{self, NodeTargetInterceptor};
use super::proto::machine;

/// Result of an upgrade request
#[derive(Debug, Clone)]
pub struct UpgradeResult {
    /// Acknowledgment message
    pub ack: String,

    /// Actor ID for tracking the upgrade operation
    pub actor_id: String,
}

/// Trigger a Talos upgrade on a node
///
/// # Arguments
///
/// * `endpoint` - Node IP address or hostname
/// * `image` - Installer image URL (e.g. ghcr.io/siderolabs/installer:v1.9.0)
/// * `preserve` - Preserve data during upgrade
/// * `stage` - Stage the upgrade (download but don't apply until reboot)
/// * `force` - Force the upgrade even if the node thinks it's not safe
/// * `talosconfig` - Optional path to talosconfig file
/// * `verbose` - Print debug output
#[allow(dead_code)]
pub async fn upgrade_node(
    endpoint: &str,
    image: &str,
    preserve: bool,
    stage: bool,
    force: bool,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<UpgradeResult> {
    upgrade_node_via(
        endpoint,
        None,
        image,
        preserve,
        stage,
        force,
        talosconfig,
        verbose,
    )
    .await
}

/// Trigger a Talos upgrade on a target node, optionally routed through a proxy.
///
/// # Arguments
///
/// * `endpoint` - The Talos API endpoint to connect to (control plane IP)
/// * `target_node` - Optional target node IP to route the request to via the endpoint
/// * `image` - Installer image URL (e.g. ghcr.io/siderolabs/installer:v1.9.0)
/// * `preserve` - Preserve data during upgrade
/// * `stage` - Stage the upgrade (download but don't apply until reboot)
/// * `force` - Force the upgrade even if the node thinks it's not safe
/// * `talosconfig` - Optional path to talosconfig file
/// * `verbose` - Print debug output
#[allow(clippy::too_many_arguments)]
pub async fn upgrade_node_via(
    endpoint: &str,
    target_node: Option<&str>,
    image: &str,
    preserve: bool,
    stage: bool,
    force: bool,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<UpgradeResult> {
    let channel = client::connect(endpoint, talosconfig, verbose).await?;

    let request = machine::UpgradeRequest {
        image: image.to_string(),
        preserve,
        stage,
        force,
        reboot_mode: machine::upgrade_request::RebootMode::Default as i32,
    };

    if verbose {
        eprintln!(
            "sending upgrade request: image={}, preserve={}, stage={}, force={}, target={:?}",
            image, preserve, stage, force, target_node
        );
    }

    let response = if let Some(target) = target_node {
        // Route through the endpoint to the target node
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel,
            interceptor,
        );
        client
            .upgrade(request)
            .await
            .context("failed to send upgrade request via proxy")?
            .into_inner()
    } else {
        // Direct connection to the endpoint
        let mut client = machine::machine_service_client::MachineServiceClient::new(channel);
        client
            .upgrade(request)
            .await
            .context("failed to send upgrade request")?
            .into_inner()
    };

    // The response contains a list of messages (one per node in cluster mode)
    let upgrade_msg = response
        .messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no upgrade response from node"))?;

    // Check for errors in metadata
    if let Some(ref meta) = upgrade_msg.metadata
        && !meta.error.is_empty()
    {
        anyhow::bail!("upgrade error: {}", meta.error);
    }

    Ok(UpgradeResult {
        ack: upgrade_msg.ack,
        actor_id: upgrade_msg.actor_id,
    })
}
