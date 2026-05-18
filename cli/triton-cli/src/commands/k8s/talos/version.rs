// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Query Talos node version via gRPC

use anyhow::{Context, Result};

use super::client::{self, NodeTargetInterceptor};
use super::proto::machine;

/// Version information returned by a Talos node
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct VersionInfo {
    /// Version tag (e.g. "v1.9.0")
    pub tag: String,

    /// Git SHA
    pub sha: String,

    /// Build timestamp
    pub built: String,

    /// Go version
    pub go_version: String,

    /// OS (e.g. "linux")
    pub os: String,

    /// Architecture (e.g. "amd64")
    pub arch: String,
}

/// Query the Talos version running on a node
#[allow(dead_code)]
pub async fn get_version(
    endpoint: &str,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<VersionInfo> {
    get_version_via(endpoint, None, talosconfig, verbose).await
}

/// Query the Talos version running on a target node, optionally routed through a proxy.
///
/// # Arguments
///
/// * `endpoint` - The Talos API endpoint to connect to (control plane IP)
/// * `target_node` - Optional target node IP to route the request to via the endpoint
/// * `talosconfig` - Optional path to talosconfig file
/// * `verbose` - Print debug output
pub async fn get_version_via(
    endpoint: &str,
    target_node: Option<&str>,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<VersionInfo> {
    let channel = client::connect(endpoint, talosconfig, verbose).await?;

    let response = if let Some(target) = target_node {
        // Route through the endpoint to the target node
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel,
            interceptor,
        );
        client
            .version(())
            .await
            .context("failed to query Talos version via proxy")?
            .into_inner()
    } else {
        // Direct connection to the endpoint
        let mut client = machine::machine_service_client::MachineServiceClient::new(channel);
        client
            .version(())
            .await
            .context("failed to query Talos version")?
            .into_inner()
    };

    // The response contains a list of messages (one per node in cluster mode)
    // For single node queries, we expect exactly one message
    let version_msg = response
        .messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no version response from node"))?;

    // Check for errors in metadata
    if let Some(ref meta) = version_msg.metadata
        && !meta.error.is_empty()
    {
        anyhow::bail!("version query error: {}", meta.error);
    }

    let version_info = version_msg
        .version
        .ok_or_else(|| anyhow::anyhow!("no version info in response"))?;

    Ok(VersionInfo {
        tag: version_info.tag,
        sha: version_info.sha,
        built: version_info.built,
        go_version: version_info.go_version,
        os: version_info.os,
        arch: version_info.arch,
    })
}
