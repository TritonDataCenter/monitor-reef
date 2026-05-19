// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the `/v1/k8s/*` endpoints (Kelp managed Kubernetes).
//!
//! Phase 1 covers cluster CRUD: a lean public `Cluster` type for API
//! responses, backed by a richer internal `ClusterRecord` in the server
//! that accumulates node inventory, credentials, and orchestration state
//! as the bootstrap endpoint fills it in.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A Kelp-managed Kubernetes cluster record (public API view).
///
/// The record exists independent of any provisioned VMs — `Created`
/// state means "we know about this cluster but have not bootstrapped
/// it yet." Bootstrap transitions the record through `Provisioning`
/// to `Running`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Cluster {
    /// Server-assigned identifier. Used in `/v1/k8s/clusters/{cluster}`.
    pub id: Uuid,

    /// Customer-supplied display name.
    pub name: String,

    /// Owning Triton account UUID. Derived from the authenticated
    /// caller at create time; cannot be set by the client.
    pub account_id: Uuid,

    /// Lifecycle state of the cluster record.
    pub state: ClusterState,

    /// Optional customer-supplied description.
    pub description: Option<String>,

    /// Triton fabric network the cluster nodes are provisioned on.
    /// `None` until set at bootstrap time.
    pub fabric_network_id: Option<Uuid>,

    /// Target Kubernetes version (e.g. `1.30.3`). `None` until bootstrap
    /// begins selecting images.
    pub kubernetes_version: Option<String>,

    /// Target Talos Linux version (e.g. `1.7.6`). `None` until bootstrap
    /// begins.
    pub talos_version: Option<String>,

    /// Kubernetes API server endpoint URL. `None` until the control
    /// plane is operational.
    pub endpoint: Option<String>,

    /// Number of control-plane nodes currently tracked.
    pub control_plane_count: u32,

    /// Number of worker nodes currently tracked.
    pub worker_count: u32,

    /// When the record was created.
    pub created_at: DateTime<Utc>,
}

/// Lifecycle state of a [`Cluster`] record.
///
/// Forward-compatible: clients deserialising an older binary can
/// receive new state names from a newer server and round-trip them
/// through `Unknown` rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClusterState {
    /// Record exists, no VMs provisioned. `POST .../bootstrap` (future)
    /// transitions out of this state.
    Created,

    /// Bootstrap is in progress.
    Provisioning,

    /// Bootstrap completed; the cluster is operational.
    Running,

    /// Cluster is reachable but at least one node or control-plane
    /// component is unhealthy.
    Degraded,

    /// Deletion is in progress; the record will disappear when the
    /// underlying VMs have been destroyed.
    Deleting,

    /// Catch-all for forward compatibility; an unrecognised state
    /// name from a newer server.
    #[serde(other)]
    Unknown,
}

/// Body of `POST /v1/k8s/clusters`.
///
/// The server assigns `id`, `account_id`, `state`, and `created_at`.
/// Version selection and network assignment happen at bootstrap time.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateClusterRequest {
    pub name: String,
    pub description: Option<String>,
    pub fabric_network_id: Option<Uuid>,
}

/// Path parameters for `/v1/k8s/clusters/{cluster}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClusterPath {
    /// Server-assigned cluster UUID.
    pub cluster: Uuid,
}

/// Response body for `GET /v1/k8s/clusters`.
///
/// A wrapper struct (rather than a bare `Vec<Cluster>`) so future
/// pagination metadata can be added without breaking the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClusterList {
    pub items: Vec<Cluster>,
}

/// Body of `POST /v1/k8s/clusters/{cluster}/bootstrap`.
///
/// The server provisions VMs via the operator CloudAPI client, generates
/// all Talos PKI and machine configs, applies them through the relay tunnel,
/// and retrieves the kubeconfig — all asynchronously.
///
/// Poll `GET /v1/k8s/clusters/{cluster}` until `state == "running"`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BootstrapClusterRequest {
    /// Number of control-plane nodes to provision. Must be at least 1.
    /// Use 3 for a highly-available control plane.
    pub control_plane_count: u32,
    /// Number of worker nodes to provision. May be 0.
    #[serde(default)]
    pub worker_count: u32,
    /// CloudAPI package name or UUID for each node VM.
    /// Defaults to `"sample-2G"`.
    #[serde(default = "default_package")]
    pub package: String,
    /// CloudAPI image name or UUID (must be a Talos nocloud image).
    /// Defaults to `"talos-1.12-nocloud"`.
    #[serde(default = "default_image")]
    pub image: String,
    /// Talos installer image tag, e.g. `"v1.12.7"`. Defaults to `"v1.12.7"`.
    #[serde(default)]
    pub talos_version: Option<String>,
    /// Disk to install Talos onto on each node. Defaults to `"/dev/sda"`.
    #[serde(default)]
    pub install_disk: Option<String>,
}

fn default_package() -> String {
    "sample-2G".to_string()
}

fn default_image() -> String {
    "talos-1.12-nocloud".to_string()
}

/// Specification for a single node during cluster bootstrap or scale-out.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeBootstrapSpec {
    /// Fabric IP of an already-running node that has the relay agent active.
    /// Example: `"10.0.0.5"`.
    pub fabric_ip: String,
    /// Role this node will play in the cluster.
    pub role: NodeBootstrapRole,
}

/// Role a node plays within a Kelp cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeBootstrapRole {
    ControlPlane,
    Worker,
}

/// Body of `POST /v1/k8s/clusters/{cluster}/upgrade`.
///
/// Triggers a rolling Talos OS upgrade across all nodes in the cluster.
/// Control-plane nodes are upgraded first (one at a time to maintain etcd
/// quorum), then worker nodes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeClusterRequest {
    /// Talos installer image to upgrade to, e.g.
    /// `ghcr.io/siderolabs/talos:v1.8.0`.
    pub talos_image: String,
    /// Preserve the ephemeral data partition across the upgrade.
    /// Default: `false` (data partition is wiped — correct for most upgrades).
    #[serde(default)]
    pub preserve: bool,
}

/// Body of `POST /v1/k8s/clusters/{cluster}/nodes`.
///
/// Each node must already have the relay agent running. The server
/// applies the supplied Talos machine config in maintenance mode and
/// triggers a reboot; the node then joins the existing cluster
/// automatically.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddNodesRequest {
    pub nodes: Vec<NodeBootstrapSpec>,
}

/// Response body for `GET /v1/k8s/clusters/{cluster}/kubeconfig`.
///
/// The `kubeconfig` field is a complete YAML kubeconfig document. Callers
/// can write it to `~/.kube/config` or merge it with an existing config
/// via `kubectl config view --merge`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KubeconfigResponse {
    /// Kubeconfig YAML.
    pub kubeconfig: String,
}
