// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Kubernetes control plane and kubelet rolling upgrade.
//!
//! Mirrors the behavior of `talosctl upgrade-k8s` natively in Rust by
//! mutating the locally-stored `controlplane.yaml` / `worker.yaml`, pushing
//! the changes via Talos `ApplyConfiguration(NoReboot)`, and watching the
//! resulting Kubernetes rollout via the `kube` crate.

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_client::TypedClient;
use serde::Serialize;

use super::images;
use super::state::ClusterState;

pub mod discovery;
pub mod kube_proxy;
pub mod kubelet;
pub mod machine_config;
pub mod preflight;
pub mod prepull;
pub mod static_pod;

#[derive(Args, Clone)]
pub struct UpgradeK8sArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Target Kubernetes version (e.g. v1.36.0)
    #[arg(long)]
    pub to: String,

    /// Source Kubernetes version (auto-detected from cluster if omitted)
    #[arg(long)]
    pub from: Option<String>,

    /// Preview the plan without applying any changes
    #[arg(long)]
    pub dry_run: bool,

    /// Skip preflight health and quorum checks
    #[arg(long)]
    pub force: bool,

    /// Pre-pull component images on each node before patching
    #[arg(long, default_value_t = true)]
    pub pre_pull_images: bool,

    /// Also roll kubelet across every node after the control plane is up
    #[arg(long, default_value_t = true)]
    pub upgrade_kubelet: bool,

    /// Override the kube-apiserver image (default: registry.k8s.io/kube-apiserver:<ver>)
    #[arg(long)]
    pub apiserver_image: Option<String>,

    /// Override the kube-controller-manager image
    #[arg(long)]
    pub controller_manager_image: Option<String>,

    /// Override the kube-scheduler image
    #[arg(long)]
    pub scheduler_image: Option<String>,

    /// Override the kube-proxy image
    #[arg(long)]
    pub proxy_image: Option<String>,

    /// Override the kubelet image
    #[arg(long)]
    pub kubelet_image: Option<String>,

    /// Pre-flight cluster health check timeout
    #[arg(long, default_value = "2m")]
    pub health_timeout: String,

    /// Per-node deadline for waiting on static pod / kubelet roll
    #[arg(long, default_value = "5m")]
    pub per_node_timeout: String,
}

/// Concrete set of image references resolved for this upgrade.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedImages {
    pub apiserver: String,

    pub controller_manager: String,

    pub scheduler: String,

    pub proxy: String,

    pub kubelet: String,
}

impl ResolvedImages {
    pub fn resolve(args: &UpgradeK8sArgs, version: &str) -> Self {
        Self {
            apiserver: args
                .apiserver_image
                .clone()
                .unwrap_or_else(|| images::apiserver_image(version)),

            controller_manager: args
                .controller_manager_image
                .clone()
                .unwrap_or_else(|| images::controller_manager_image(version)),

            scheduler: args
                .scheduler_image
                .clone()
                .unwrap_or_else(|| images::scheduler_image(version)),

            proxy: args
                .proxy_image
                .clone()
                .unwrap_or_else(|| images::proxy_image(version)),

            kubelet: args
                .kubelet_image
                .clone()
                .unwrap_or_else(|| images::kubelet_image(version)),
        }
    }
}

/// JSON output structure for `--json` mode.
#[derive(Debug, Serialize)]
pub struct UpgradeOutput {
    pub cluster: String,

    pub from_version: String,

    pub to_version: String,

    pub dry_run: bool,

    pub images: ResolvedImages,

    pub control_plane_nodes: Vec<String>,

    pub worker_nodes: Vec<String>,

    pub status: String,
}

pub async fn run(args: UpgradeK8sArgs, _client: &TypedClient, json: bool) -> Result<()> {
    let target_version = images::normalize_version(&args.to);

    let state = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    let cluster_dir = state.cluster_dir()?;
    let talosconfig = cluster_dir.join("talosconfig");
    let kubeconfig = cluster_dir.join("kubeconfig");

    if !tokio::fs::try_exists(&talosconfig).await.unwrap_or(false) {
        anyhow::bail!(
            "Cluster {} has no talosconfig — was it bootstrapped?",
            state.name
        );
    }
    if !tokio::fs::try_exists(&kubeconfig).await.unwrap_or(false) {
        anyhow::bail!(
            "Cluster {} has no kubeconfig — cannot reach the Kubernetes API",
            state.name
        );
    }

    let plan = discovery::Plan::build(&state, &target_version, args.from.as_deref(), &kubeconfig)
        .await
        .context("Failed to build upgrade plan")?;

    let images = ResolvedImages::resolve(&args, &target_version);

    if !json {
        plan.print_header(&images);
    }

    // Preflight (unless --force or --dry-run)
    if !args.force {
        preflight::run(&plan, &state, &talosconfig, &args.health_timeout, json)
            .await
            .context("preflight checks failed")?;
    }

    // Dry-run stops after preflight.
    if args.dry_run {
        if json {
            let output = UpgradeOutput {
                cluster: state.name.clone(),
                from_version: plan.from_version.clone(),
                to_version: target_version.clone(),
                dry_run: true,
                images,
                control_plane_nodes: plan.control_plane_node_names(),
                worker_nodes: plan.worker_node_names(),
                status: "dry-run".to_string(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!();
            eprintln!("==> Dry-run complete. No changes made.");
        }
        return Ok(());
    }

    // Pre-pull images on relevant nodes.
    if args.pre_pull_images {
        prepull::run(
            &plan,
            &state,
            &talosconfig,
            &images,
            args.upgrade_kubelet,
            json,
        )
        .await
        .context("image pre-pull failed")?;
    }

    let controlplane_yaml = cluster_dir.join("controlplane.yaml");
    let worker_yaml = cluster_dir.join("worker.yaml");

    let per_node_timeout = parse_duration(&args.per_node_timeout)?;

    // Static pods on each control plane node, sequentially.
    static_pod::run(
        &plan,
        &state,
        &controlplane_yaml,
        &talosconfig,
        &kubeconfig,
        &images,
        per_node_timeout,
        json,
    )
    .await
    .context("control plane static pod upgrade failed")?;

    // kube-proxy DaemonSet roll.
    kube_proxy::run(
        &plan,
        &state,
        &controlplane_yaml,
        &talosconfig,
        &kubeconfig,
        &images,
        per_node_timeout,
        json,
    )
    .await
    .context("kube-proxy upgrade failed")?;

    // kubelet across all nodes (control plane first, then workers).
    if args.upgrade_kubelet {
        kubelet::run(
            &plan,
            &state,
            &controlplane_yaml,
            &worker_yaml,
            &talosconfig,
            &kubeconfig,
            &images,
            per_node_timeout,
            json,
        )
        .await
        .context("kubelet upgrade failed")?;
    }

    // Verify and update local state.
    let final_version = discovery::detect_k8s_version(&kubeconfig)
        .await
        .context("Failed to detect final Kubernetes version")?;

    if !json {
        eprintln!();
        eprintln!("==> Upgrade complete");
        eprintln!(
            "    Kubernetes version: {} -> {} (observed: {})",
            plan.from_version, target_version, final_version
        );
        if !args.upgrade_kubelet {
            eprintln!("    NOTE: kubelet roll was skipped (--upgrade-kubelet=false)");
        }
        eprintln!(
            "    NOTE: Bootstrap manifests (CoreDNS pod manifest etc.) were not re-synced. \n\
                  If CoreDNS or similar bootstrap manifests changed between k8s versions, \n\
                  reconcile them manually."
        );
    } else {
        let output = UpgradeOutput {
            cluster: state.name.clone(),
            from_version: plan.from_version.clone(),
            to_version: target_version.clone(),
            dry_run: false,
            images,
            control_plane_nodes: plan.control_plane_node_names(),
            worker_nodes: plan.worker_node_names(),
            status: "completed".to_string(),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Parse a human-readable duration string like "10m", "1h", "30s", "1h30m".
pub fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            let n: u64 = current_num
                .parse()
                .with_context(|| format!("invalid duration '{}'", s))?;
            current_num.clear();
            match ch {
                'h' => total_secs += n * 3600,
                'm' => total_secs += n * 60,
                's' => total_secs += n,
                _ => anyhow::bail!("unknown duration unit '{}' in '{}'", ch, s),
            }
        }
    }
    if !current_num.is_empty() {
        let n: u64 = current_num.parse()?;
        total_secs += n;
    }
    if total_secs == 0 {
        anyhow::bail!("duration '{}' resolves to zero", s);
    }
    Ok(std::time::Duration::from_secs(total_secs))
}
