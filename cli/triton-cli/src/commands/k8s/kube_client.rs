// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Shared Kubernetes client utilities for native K8s API access.
//!
//! This module provides helper functions for interacting with the Kubernetes
//! API directly, replacing shell-outs to kubectl.

use anyhow::{Context, Result, bail};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Secret};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::DynamicObject;
use kube::discovery::Scope;
use kube::{Client, Config, Resource};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;
use std::time::Duration;

/// Field manager name for server-side apply operations.
const FIELD_MANAGER: &str = "triton-cli";

/// Create a kube Client from a specific kubeconfig file path.
pub async fn client_from_kubeconfig(path: &Path) -> Result<Client> {
    let kubeconfig = Kubeconfig::read_from(path)
        .with_context(|| format!("Failed to read kubeconfig from {}", path.display()))?;

    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .context("Failed to create kube config from kubeconfig")?;

    Client::try_from(config).context("Failed to create kube client")
}

/// Apply a multi-document YAML manifest using server-side apply.
///
/// Parses the YAML, determines the resource type for each document,
/// and applies using Patch::Apply for idempotent create-or-update semantics.
pub async fn apply_yaml_manifest(client: &Client, yaml: &str) -> Result<()> {
    for document in serde_yaml::Deserializer::from_str(yaml) {
        let value: serde_json::Value =
            serde_yaml::with::singleton_map_recursive::deserialize(document)
                .context("Failed to parse YAML document")?;

        // Skip empty documents (can happen with multi-doc YAML)
        if value.is_null() {
            continue;
        }

        apply_dynamic_object(client, &value).await?;
    }
    Ok(())
}

/// Apply a single Kubernetes object from its JSON/YAML value.
async fn apply_dynamic_object(client: &Client, value: &serde_json::Value) -> Result<()> {
    let api_version = value["apiVersion"]
        .as_str()
        .context("Object missing apiVersion")?;
    let kind = value["kind"].as_str().context("Object missing kind")?;
    let name = value["metadata"]["name"]
        .as_str()
        .context("Object missing metadata.name")?;
    let namespace = value["metadata"]["namespace"].as_str();

    // Determine API resource from apiVersion and kind
    let ar = api_resource_for(api_version, kind)?;

    let pp = PatchParams::apply(FIELD_MANAGER);

    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
        None => {
            // Check if resource is namespaced or cluster-scoped
            if is_cluster_scoped(kind) {
                Api::all_with(client.clone(), &ar)
            } else {
                Api::default_namespaced_with(client.clone(), &ar)
            }
        }
    };

    api.patch(name, &pp, &Patch::Apply(value))
        .await
        .with_context(|| format!("Failed to apply {} '{}'", kind, name))?;

    Ok(())
}

/// Determine the ApiResource for a given apiVersion and kind.
fn api_resource_for(api_version: &str, kind: &str) -> Result<kube::api::ApiResource> {
    // Map common resources - could be extended or replaced with discovery
    let (group, version) = if api_version.contains('/') {
        let parts: Vec<&str> = api_version.splitn(2, '/').collect();
        (parts[0], parts[1])
    } else {
        ("", api_version) // core API
    };

    let (plural, scope) = match kind {
        "ServiceAccount" => ("serviceaccounts", Scope::Namespaced),
        "ClusterRole" => ("clusterroles", Scope::Cluster),
        "ClusterRoleBinding" => ("clusterrolebindings", Scope::Cluster),
        "Role" => ("roles", Scope::Namespaced),
        "RoleBinding" => ("rolebindings", Scope::Namespaced),
        "Secret" => ("secrets", Scope::Namespaced),
        "ConfigMap" => ("configmaps", Scope::Namespaced),
        "Deployment" => ("deployments", Scope::Namespaced),
        "Service" => ("services", Scope::Namespaced),
        "Pod" => ("pods", Scope::Namespaced),
        _ => bail!("Unknown resource kind: {}", kind),
    };

    // Suppress unused warning - scope is used for documentation purposes
    // and could be used in future enhancements
    let _ = scope;

    Ok(kube::api::ApiResource {
        group: group.to_string(),
        version: version.to_string(),
        api_version: api_version.to_string(),
        kind: kind.to_string(),
        plural: plural.to_string(),
    })
}

fn is_cluster_scoped(kind: &str) -> bool {
    matches!(kind, "ClusterRole" | "ClusterRoleBinding" | "Namespace")
}

/// Create or update a Secret with string data.
pub async fn create_or_update_secret(
    client: &Client,
    name: &str,
    namespace: &str,
    data: BTreeMap<String, String>,
) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);

    let secret = Secret {
        metadata: kube::core::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        string_data: Some(data),
        ..Default::default()
    };

    let pp = PatchParams::apply(FIELD_MANAGER);
    secrets
        .patch(name, &pp, &Patch::Apply(&secret))
        .await
        .with_context(|| format!("Failed to create/update Secret '{}'", name))?;

    Ok(())
}

/// Create or update a ConfigMap with data.
pub async fn create_or_update_configmap(
    client: &Client,
    name: &str,
    namespace: &str,
    data: BTreeMap<String, String>,
) -> Result<()> {
    let configmaps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);

    let cm = ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };

    let pp = PatchParams::apply(FIELD_MANAGER);
    configmaps
        .patch(name, &pp, &Patch::Apply(&cm))
        .await
        .with_context(|| format!("Failed to create/update ConfigMap '{}'", name))?;

    Ok(())
}

/// Delete a namespaced resource, ignoring NotFound errors.
pub async fn delete_namespaced<T>(client: &Client, name: &str, namespace: &str) -> Result<()>
where
    T: Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + Debug
        + Send
        + Sync,
    <T as Resource>::DynamicType: Default,
{
    let api: Api<T> = Api::namespaced(client.clone(), namespace);
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()), // NotFound is OK
        Err(e) => Err(e).context(format!("Failed to delete {}", name)),
    }
}

/// Delete a cluster-scoped resource, ignoring NotFound errors.
pub async fn delete_cluster_scoped<T>(client: &Client, name: &str) -> Result<()>
where
    T: Resource<Scope = k8s_openapi::ClusterResourceScope>
        + Clone
        + DeserializeOwned
        + Debug
        + Send
        + Sync,
    <T as Resource>::DynamicType: Default,
{
    let api: Api<T> = Api::all(client.clone());
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()), // NotFound is OK
        Err(e) => Err(e).context(format!("Failed to delete {}", name)),
    }
}

/// Get a Deployment, returning None if not found.
pub async fn get_deployment(
    client: &Client,
    name: &str,
    namespace: &str,
) -> Result<Option<Deployment>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    match deployments.get_opt(name).await {
        Ok(d) => Ok(d),
        Err(e) => Err(e).context(format!("Failed to get Deployment '{}'", name)),
    }
}

/// Wait for a Deployment to be available (rollout complete).
///
/// Polls the deployment status until available replicas >= desired replicas,
/// with a configurable timeout.
pub async fn wait_for_deployment_ready(
    client: &Client,
    name: &str,
    namespace: &str,
    timeout: Duration,
) -> Result<()> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_secs(2);

    loop {
        if start.elapsed() > timeout {
            bail!(
                "Timeout waiting for deployment '{}' to be ready after {:?}",
                name,
                timeout
            );
        }

        match deployments.get_opt(name).await? {
            Some(deployment) => {
                let spec_replicas = deployment
                    .spec
                    .as_ref()
                    .and_then(|s| s.replicas)
                    .unwrap_or(1);
                let available = deployment
                    .status
                    .as_ref()
                    .and_then(|s| s.available_replicas)
                    .unwrap_or(0);

                if available >= spec_replicas {
                    return Ok(());
                }
            }
            None => {
                // Deployment doesn't exist yet, keep waiting
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// List pods by label selector and return the phase of the first pod.
pub async fn get_pod_status_by_label(
    client: &Client,
    namespace: &str,
    label_selector: &str,
) -> Result<Option<String>> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(label_selector);

    let pod_list = pods.list(&lp).await.context("Failed to list pods")?;

    if let Some(pod) = pod_list.items.first() {
        let phase = pod.status.as_ref().and_then(|s| s.phase.clone());
        return Ok(phase);
    }

    Ok(None)
}

// Re-export types that callers need for generic delete functions
pub use k8s_openapi::api::apps::v1::Deployment as K8sDeployment;
pub use k8s_openapi::api::core::v1::ConfigMap as K8sConfigMap;
pub use k8s_openapi::api::core::v1::Secret as K8sSecret;
pub use k8s_openapi::api::core::v1::ServiceAccount as K8sServiceAccount;
pub use k8s_openapi::api::rbac::v1::ClusterRole as K8sClusterRole;
pub use k8s_openapi::api::rbac::v1::ClusterRoleBinding as K8sClusterRoleBinding;
