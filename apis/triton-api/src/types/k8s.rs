// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Types for the `/v1/k8s/*` endpoints (Kelp managed Kubernetes).
//!
//! Phase 1 only covers cluster CRUD: enough surface for higher-level
//! operations (bootstrap, kubeconfig, health, scaling) to have a
//! server-side cluster record to operate on. Node inventory, network
//! configuration, and in-cluster component state are deferred until
//! the bootstrap endpoint lands and they have something to track.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A Kelp-managed Kubernetes cluster record.
///
/// The record exists independent of any provisioned VMs — `Created`
/// state means "we know about this cluster but have not bootstrapped
/// it yet." Bootstrap (a future endpoint) transitions the record
/// through `Provisioning` to `Running`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Cluster {
    /// Server-assigned identifier. Used in `/v1/k8s/clusters/{cluster}`.
    pub id: Uuid,

    /// Customer-supplied display name. Not unique across accounts and
    /// not used as an identifier.
    pub name: String,

    /// Owning Triton account UUID. Derived from the authenticated
    /// caller at create time; cannot be set by the client.
    pub account_id: Uuid,

    /// Lifecycle state of the cluster record.
    pub state: ClusterState,

    /// Target Kubernetes version (e.g. `1.30.3`). Compared against the
    /// `kubernetes_version -> talos image` map maintained by the
    /// service when bootstrap eventually selects an image.
    pub kubernetes_version: String,

    /// Target Talos Linux version (e.g. `1.7.6`). Same mapping
    /// caveat as `kubernetes_version`.
    pub talos_version: String,

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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateClusterRequest {
    pub name: String,
    pub kubernetes_version: String,
    pub talos_version: String,
}

/// Path parameters for `/v1/k8s/clusters/{cluster}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClusterPath {
    /// Server-assigned cluster UUID.
    pub cluster: Uuid,
}

/// Body of `GET /v1/k8s/clusters`.
///
/// A wrapper struct (rather than a bare `Vec<Cluster>`) so future
/// pagination metadata can be added without breaking the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClusterList {
    pub items: Vec<Cluster>,
}
