// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Default Kubernetes component image references.
//!
//! These match the registry/repo conventions used by upstream Talos so
//! a cluster bootstrapped by us is byte-identical to one produced by
//! `talosctl gen config`, and so the upgrade-k8s flow can produce the
//! same image strings the operator would expect.

/// Default registry for upstream Kubernetes control plane components.
pub const K8S_REGISTRY: &str = "registry.k8s.io";

/// Default repo path for the Talos-maintained kubelet image.
pub const KUBELET_REPO: &str = "ghcr.io/siderolabs/kubelet";

/// Compute the default image reference for the kubelet at the given
/// Kubernetes version (e.g. "v1.36.0").
pub fn kubelet_image(version: &str) -> String {
    format!("{}:{}", KUBELET_REPO, version)
}

/// Compute the default image reference for `kube-apiserver`.
pub fn apiserver_image(version: &str) -> String {
    format!("{}/kube-apiserver:{}", K8S_REGISTRY, version)
}

/// Compute the default image reference for `kube-controller-manager`.
pub fn controller_manager_image(version: &str) -> String {
    format!("{}/kube-controller-manager:{}", K8S_REGISTRY, version)
}

/// Compute the default image reference for `kube-scheduler`.
pub fn scheduler_image(version: &str) -> String {
    format!("{}/kube-scheduler:{}", K8S_REGISTRY, version)
}

/// Compute the default image reference for `kube-proxy`.
pub fn proxy_image(version: &str) -> String {
    format!("{}/kube-proxy:{}", K8S_REGISTRY, version)
}

/// Normalize a user-provided version string to start with a single `v`.
/// `"1.36.0"` and `"v1.36.0"` both become `"v1.36.0"`.
pub fn normalize_version(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    }
}
