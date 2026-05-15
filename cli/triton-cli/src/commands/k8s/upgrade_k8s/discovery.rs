// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Cluster discovery and upgrade plan construction.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

use super::super::kube_client;
use super::super::state::{ClusterState, NodeInfo, NodeRole};
use super::ResolvedImages;

/// Single node to upgrade.
#[derive(Debug, Clone, Serialize)]
pub struct PlanNode {
    pub name: String,

    pub role: NodeRole,

    /// Talos API endpoint (control plane primary IP for routing, or this
    /// node's own primary IP if it's a control plane node).
    pub talos_endpoint: String,

    /// Fabric IP used as the `nodes` header to proxy through the control
    /// plane endpoint. `None` for control plane nodes (no proxy needed).
    pub fabric_target: Option<String>,
}

/// The plan computed up-front from discovery.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub cluster_name: String,

    pub from_version: String,

    pub to_version: String,

    pub control_plane: Vec<PlanNode>,

    pub workers: Vec<PlanNode>,
}

impl Plan {
    pub async fn build(
        state: &ClusterState,
        target_version: &str,
        from_override: Option<&str>,
        kubeconfig: &Path,
    ) -> Result<Self> {
        let cp_endpoint = state
            .nodes
            .iter()
            .find(|(_, info)| info.role == NodeRole::Control)
            .and_then(|(_, info)| info.primary_ip.clone())
            .context("no control plane node has a primary IP")?;

        let control_plane = collect_role(state, NodeRole::Control, &cp_endpoint)?;
        let workers = collect_role(state, NodeRole::Worker, &cp_endpoint)?;

        let from_version = match from_override {
            Some(v) => super::super::images::normalize_version(v),
            None => detect_k8s_version(kubeconfig)
                .await
                .context("could not auto-detect current Kubernetes version")?,
        };

        Ok(Plan {
            cluster_name: state.name.clone(),
            from_version,
            to_version: target_version.to_string(),
            control_plane,
            workers,
        })
    }

    pub fn control_plane_node_names(&self) -> Vec<String> {
        self.control_plane.iter().map(|n| n.name.clone()).collect()
    }

    pub fn worker_node_names(&self) -> Vec<String> {
        self.workers.iter().map(|n| n.name.clone()).collect()
    }

    pub fn print_header(&self, images: &ResolvedImages) {
        eprintln!(
            "==> Kubernetes upgrade plan for cluster '{}'",
            self.cluster_name
        );
        eprintln!("    From: {}", self.from_version);
        eprintln!("    To:   {}", self.to_version);
        eprintln!("    Images:");
        eprintln!("      apiserver:          {}", images.apiserver);
        eprintln!("      controller-manager: {}", images.controller_manager);
        eprintln!("      scheduler:          {}", images.scheduler);
        eprintln!("      proxy:              {}", images.proxy);
        eprintln!("      kubelet:            {}", images.kubelet);
        eprintln!(
            "    Nodes: {} control, {} worker",
            self.control_plane.len(),
            self.workers.len()
        );
        for node in &self.control_plane {
            eprintln!("      [ctrl] {} ({})", node.name, node.talos_endpoint);
        }
        for node in &self.workers {
            let via = node
                .fabric_target
                .as_deref()
                .map(|t| format!(" via {} (fabric {})", node.talos_endpoint, t))
                .unwrap_or_default();
            eprintln!("      [work] {}{}", node.name, via);
        }
        eprintln!();
    }
}

fn collect_role(state: &ClusterState, role: NodeRole, cp_endpoint: &str) -> Result<Vec<PlanNode>> {
    let mut out = Vec::new();
    for (name, info) in &state.nodes {
        if info.role != role {
            continue;
        }
        out.push(node_to_plan(name, info, cp_endpoint)?);
    }
    // Stable order by name so output is reproducible.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn node_to_plan(name: &str, info: &NodeInfo, cp_endpoint: &str) -> Result<PlanNode> {
    let (talos_endpoint, fabric_target) = match info.role {
        NodeRole::Control => {
            let ep = info
                .primary_ip
                .clone()
                .with_context(|| format!("control node {} has no primary IP", name))?;
            (ep, None)
        }
        NodeRole::Worker => {
            let fabric = info
                .fabric_ip
                .clone()
                .or_else(|| info.primary_ip.clone())
                .with_context(|| format!("worker node {} has no IP address", name))?;
            (cp_endpoint.to_string(), Some(fabric))
        }
    };
    Ok(PlanNode {
        name: name.to_string(),
        role: info.role,
        talos_endpoint,
        fabric_target,
    })
}

/// Query the live Kubernetes API for its server version.
///
/// Returns a normalized form like `"v1.35.0"`. The API exposes minor as a
/// string ("35" or "35+") and gitVersion as a full tag; we prefer gitVersion
/// when it parses, falling back to `vMAJOR.MINOR.0`.
pub async fn detect_k8s_version(kubeconfig: &Path) -> Result<String> {
    let client = kube_client::client_from_kubeconfig(kubeconfig).await?;
    let info = client
        .apiserver_version()
        .await
        .context("apiserver version query")?;
    if info.git_version.starts_with('v') {
        return Ok(info.git_version);
    }
    // Strip any non-numeric suffix from minor ("35+", "35-rc.1").
    let minor: String = info
        .minor
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let minor = if minor.is_empty() { "0".into() } else { minor };
    Ok(format!("v{}.{}.0", info.major, minor))
}
