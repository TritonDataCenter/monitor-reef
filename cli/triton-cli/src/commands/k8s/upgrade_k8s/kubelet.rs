// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Roll kubelet across every node, control plane first then workers.
//!
//! For each node: cordon → drain (PDB-respecting eviction) → patch
//! `machine.kubelet.image` in the local YAML and `ApplyConfiguration(NoReboot)`
//! → wait for the node's `nodeInfo.kubeletVersion` to match the target and
//! the node to report `Ready=True` → uncordon.

use anyhow::{Context, Result, bail};
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::api::{Api, ListParams, Patch, PatchParams};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::state::ClusterState;
use super::super::talos::apply_config;
use super::ResolvedImages;
use super::discovery::{Plan, PlanNode};
use super::machine_config::{self, ImageField};

pub async fn run(
    plan: &Plan,
    _state: &ClusterState,
    controlplane_yaml: &Path,
    worker_yaml: &Path,
    talosconfig: &Path,
    kubeconfig: &Path,
    images: &ResolvedImages,
    per_node_timeout: Duration,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("==> Rolling kubelet to {}", images.kubelet);
    }

    // Patch both local config files once. Each node gets its corresponding
    // file pushed, so they have to be ready before any roll starts.
    let mut cp_doc = machine_config::load_yaml(controlplane_yaml).await?;
    let cp_prev = machine_config::set_image(&mut cp_doc, ImageField::Kubelet, &images.kubelet)?;
    machine_config::save_yaml(controlplane_yaml, &cp_doc).await?;

    let mut wk_doc = machine_config::load_yaml(worker_yaml).await?;
    let wk_prev = machine_config::set_image(&mut wk_doc, ImageField::Kubelet, &images.kubelet)?;
    machine_config::save_yaml(worker_yaml, &wk_doc).await?;

    if !json {
        eprintln!(
            "    Local config: kubelet (cp) {} -> {}",
            cp_prev.as_deref().unwrap_or("<unset>"),
            images.kubelet
        );
        eprintln!(
            "    Local config: kubelet (wk) {} -> {}",
            wk_prev.as_deref().unwrap_or("<unset>"),
            images.kubelet
        );
    }

    let kube_client = super::super::kube_client::client_from_kubeconfig(kubeconfig).await?;
    let talosconfig_str = talosconfig.to_string_lossy().to_string();

    let target_kubelet = &images.kubelet;
    let expected_version = extract_version_from_image(target_kubelet);

    // Control plane first.
    for node in &plan.control_plane {
        roll_one(
            &kube_client,
            node,
            controlplane_yaml,
            &talosconfig_str,
            &expected_version,
            per_node_timeout,
            json,
        )
        .await?;
    }
    // Then workers.
    for node in &plan.workers {
        roll_one(
            &kube_client,
            node,
            worker_yaml,
            &talosconfig_str,
            &expected_version,
            per_node_timeout,
            json,
        )
        .await?;
    }

    if !json {
        eprintln!("    kubelet roll complete.");
        eprintln!();
    }
    Ok(())
}

async fn roll_one(
    client: &kube::Client,
    node: &PlanNode,
    base_yaml: &Path,
    talosconfig: &str,
    expected_version: &str,
    timeout: Duration,
    json: bool,
) -> Result<()> {
    if !json {
        eprintln!("    -> {} ({})", node.name, expected_version);
    }
    cordon(client, &node.name, true).await?;
    if !json {
        eprintln!("       cordoned");
    }

    let evicted = drain(client, &node.name, timeout, json).await?;
    if !json {
        eprintln!("       drained ({} evictable pods removed)", evicted);
    }

    let cluster_dir = base_yaml
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", base_yaml.display()))?;
    let patch = machine_config::network_patch_path(cluster_dir, &node.name).await;
    let patch_refs: Vec<&Path> = patch.iter().map(|p| p.as_path()).collect();

    // Apply config to the right node. Control plane connects directly;
    // workers proxy through the control plane endpoint.
    apply_config::run_via(
        &node.talos_endpoint,
        node.fabric_target.as_deref(),
        base_yaml,
        &patch_refs,
        Some(talosconfig),
        false,
        false,
    )
    .await
    .with_context(|| format!("ApplyConfiguration on {} for kubelet", node.name))?;
    if !json {
        eprintln!("       config applied");
    }

    wait_for_kubelet_version(client, &node.name, expected_version, timeout, json).await?;
    if !json {
        eprintln!("       kubelet upgraded");
    }

    cordon(client, &node.name, false).await?;
    if !json {
        eprintln!("       uncordoned");
    }
    Ok(())
}

async fn cordon(client: &kube::Client, node_name: &str, on: bool) -> Result<()> {
    let nodes: Api<Node> = Api::all(client.clone());
    let patch = json!({ "spec": { "unschedulable": on } });
    // Use a strategic-merge patch rather than server-side apply: SSA
    // requires the patch object to include apiVersion+kind+metadata.name
    // (since 1.27 strict checks), and we just want to flip a single bool.
    let pp = PatchParams::default();

    // Retry on transient connect/TLS errors. The kube apiserver our
    // kubeconfig points at can drop briefly when a same-node component
    // (apid, kubelet) restarts during our upgrade.
    let mut attempts = 0;
    loop {
        attempts += 1;
        match nodes
            .patch(node_name, &pp, &Patch::Strategic(patch.clone()))
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) if attempts < 6 && is_transient(&e) => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("cordon({}) on {}", on, node_name));
            }
        }
    }
}

/// Heuristic: which kube errors are worth retrying once the apiserver
/// has had a moment to come back. We catch the rustls "unexpected EOF",
/// generic connect errors, and timeouts.
fn is_transient(e: &kube::Error) -> bool {
    let msg = format!("{:#}", e);
    msg.contains("connection error")
        || msg.contains("client error (Connect)")
        || msg.contains("client error (SendRequest)")
        || msg.contains("connection refused")
        || msg.contains("timed out")
        || msg.contains("close_notify")
}

async fn drain(
    client: &kube::Client,
    node_name: &str,
    timeout: Duration,
    json: bool,
) -> Result<usize> {
    let pods: Api<Pod> = Api::all(client.clone());
    let field = format!("spec.nodeName={}", node_name);
    let lp = ListParams::default().fields(&field);
    let list = pods.list(&lp).await.context("listing node pods")?;

    let mut evictable: Vec<String> = Vec::new();
    for pod in &list.items {
        if !is_drainable(pod) {
            continue;
        }
        let ns = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());
        let name = pod
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "<unnamed>".into());
        evictable.push(format!("{}/{}", ns, name));
    }

    // Issue evictions.
    for ident in &evictable {
        let (ns, name) = ident
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("malformed ident"))?;
        evict_one(client, ns, name).await?;
    }

    // Wait for evicted pods to disappear (or finish terminating).
    let start = Instant::now();
    let poll = Duration::from_secs(2);
    loop {
        if start.elapsed() > timeout {
            bail!(
                "timeout after {:?} waiting for pods on {} to terminate",
                timeout,
                node_name
            );
        }
        let still_there = pods
            .list(&lp)
            .await
            .context("relisting node pods")?
            .items
            .into_iter()
            .filter(|p| is_drainable(p))
            .count();
        if still_there == 0 {
            return Ok(evictable.len());
        }
        if !json {
            eprintln!("       draining: {} pod(s) remaining", still_there);
        }
        tokio::time::sleep(poll).await;
    }
}

/// kubectl drain semantics, minus the interactive prompts:
///   - skip pods owned by a DaemonSet (kubelet recreates them anyway)
///   - skip mirror pods (static pods reflected from the kubelet)
///   - skip pods already in Succeeded or Failed phase
fn is_drainable(pod: &Pod) -> bool {
    if let Some(refs) = pod.metadata.owner_references.as_ref()
        && refs.iter().any(|r| r.kind == "DaemonSet")
    {
        return false;
    }
    if pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kubernetes.io/config.mirror"))
        .is_some()
    {
        return false;
    }
    let phase = pod
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("");
    if phase == "Succeeded" || phase == "Failed" {
        return false;
    }
    true
}

async fn evict_one(client: &kube::Client, namespace: &str, name: &str) -> Result<()> {
    // PDB-respecting eviction via the Eviction subresource.
    let url = format!("/api/v1/namespaces/{}/pods/{}/eviction", namespace, name);
    let body = json!({
        "apiVersion": "policy/v1",
        "kind": "Eviction",
        "metadata": {"name": name, "namespace": namespace},
    });
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .context("building eviction request")?;
    match client.request_text(req).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ref e)) if e.code == 404 => Ok(()), // already gone
        Err(kube::Error::Api(ref e)) if e.code == 429 => {
            // PDB violation; wait briefly and retry once.
            tokio::time::sleep(Duration::from_secs(5)).await;
            let req2 = http::Request::builder()
                .method(http::Method::POST)
                .uri(&url)
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&body)?)
                .context("rebuilding eviction request")?;
            client
                .request_text(req2)
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)
                .with_context(|| format!("evicting {}/{} after PDB retry", namespace, name))
        }
        Err(e) => Err(e).with_context(|| format!("evicting {}/{}", namespace, name)),
    }
}

async fn wait_for_kubelet_version(
    client: &kube::Client,
    node_name: &str,
    expected: &str,
    timeout: Duration,
    json: bool,
) -> Result<()> {
    let api: Api<Node> = Api::all(client.clone());
    let start = Instant::now();
    let poll = Duration::from_secs(3);
    let mut last_log = Instant::now() - poll;

    loop {
        if start.elapsed() > timeout {
            bail!(
                "timeout after {:?} waiting for kubelet on {} to report {}",
                timeout,
                node_name,
                expected
            );
        }

        let node_opt = match api.get_opt(node_name).await {
            Ok(n) => n,
            Err(e) => {
                if !json && last_log.elapsed() >= Duration::from_secs(15) {
                    eprintln!("       waiting: kube API not reachable yet ({})", e);
                    last_log = Instant::now();
                }
                tokio::time::sleep(poll).await;
                continue;
            }
        };
        if let Some(node) = node_opt {
            let kubelet_ver = node
                .status
                .as_ref()
                .and_then(|s| s.node_info.as_ref())
                .map(|i| i.kubelet_version.clone())
                .unwrap_or_default();
            let ready = node
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .map(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
                .unwrap_or(false);

            if ready && version_matches(&kubelet_ver, expected) {
                return Ok(());
            }

            if !json && last_log.elapsed() >= Duration::from_secs(15) {
                eprintln!(
                    "       waiting: ready={} kubelet={} target={}",
                    ready, kubelet_ver, expected
                );
                last_log = Instant::now();
            }
        }

        tokio::time::sleep(poll).await;
    }
}

/// Compare `node.status.nodeInfo.kubeletVersion` (e.g. "v1.36.0") to our
/// expected version. Accept exact match or prefix match for cases where the
/// reported version includes build metadata.
fn version_matches(reported: &str, expected: &str) -> bool {
    reported == expected || reported.starts_with(expected)
}

/// Best-effort extraction of `vX.Y.Z` from a kubelet image reference like
/// `ghcr.io/siderolabs/kubelet:v1.36.0` or `ghcr.io/.../kubelet:v1.36.0-arm64`.
fn extract_version_from_image(image: &str) -> String {
    image
        .rsplit(':')
        .next()
        .map(|tag| tag.split('-').next().unwrap_or(tag).to_string())
        .unwrap_or_else(|| image.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_simple_tag() {
        assert_eq!(extract_version_from_image("foo:v1.36.0"), "v1.36.0");
        assert_eq!(
            extract_version_from_image("ghcr.io/x/kubelet:v1.36.0-arm64"),
            "v1.36.0"
        );
    }
}
