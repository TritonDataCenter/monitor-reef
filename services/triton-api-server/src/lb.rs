// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CloudAPI discovery helpers and Kubernetes operations for lb install.

use anyhow::{Context, Result, bail};
use cloudapi_client::TypedClient;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::core::DynamicObject;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::{Duration, Instant};
use uuid::Uuid;

const FIELD_MANAGER: &str = "triton-api-server";

// ---------------------------------------------------------------------------
// CloudAPI discovery helpers
// ---------------------------------------------------------------------------

/// Find the first non-fabric public network UUID.
pub async fn discover_public_network(cloudapi: &TypedClient, account: &str) -> Result<Uuid> {
    let networks = cloudapi
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await
        .context("list networks")?
        .into_inner();

    for n in networks {
        if n.public && !n.fabric.unwrap_or(false) {
            return Ok(n.id);
        }
    }
    bail!("no public non-fabric network found")
}

/// Derive the external CNS suffix from a public network's suffix list.
///
/// Public network suffixes look like `svc.{account}.{ext-domain}`.  We strip
/// the `{type}.{account}.` prefix and return the rest (e.g. `"ext.corp"`).
pub async fn discover_external_cns_suffix(
    cloudapi: &TypedClient,
    account: &str,
    network_id: Uuid,
) -> Result<String> {
    let network = cloudapi
        .inner()
        .get_network()
        .account(account)
        .network(network_id.to_string())
        .send()
        .await
        .context("get public network")?
        .into_inner();

    let suffixes = network
        .suffixes
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("public network {network_id} has no CNS suffixes"))?;

    let account_dot = format!(".{}.", account);
    suffixes
        .iter()
        .find_map(|s| {
            let after = s.find(&account_dot).map(|p| &s[p + account_dot.len()..])?;
            if !after.contains(".cns.") {
                Some(after.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot extract external CNS suffix from public network suffixes: {suffixes:?}"
            )
        })
}

/// Derive the internal CNS root domain from a fabric network's suffix list.
///
/// Fabric suffixes look like `svc.{account}.{dc}.cns.{root}`.  We find the
/// first suffix that contains `.cns.` and return from that point onward
/// (e.g. `"cns.capsule.corp"`).
pub async fn discover_internal_cns_root(
    cloudapi: &TypedClient,
    account: &str,
    fabric_network_id: Uuid,
) -> Result<String> {
    let network = cloudapi
        .inner()
        .get_network()
        .account(account)
        .network(fabric_network_id.to_string())
        .send()
        .await
        .context("get fabric network")?
        .into_inner();

    let suffixes = network
        .suffixes
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("fabric network {fabric_network_id} has no CNS suffixes"))?;

    suffixes
        .iter()
        .find_map(|s| s.find(".cns.").map(|p| s[p + 1..].to_string()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no internal CNS suffix (containing '.cns.') in fabric network suffixes: {suffixes:?}"
            )
        })
}

/// Return the datacenter name.  If only one datacenter is visible, use it
/// directly; otherwise fall back to the first entry.
pub async fn discover_datacenter(cloudapi: &TypedClient, account: &str) -> Result<String> {
    let dcs = cloudapi
        .inner()
        .list_datacenters()
        .account(account)
        .send()
        .await
        .context("list datacenters")?
        .into_inner();

    dcs.keys()
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("datacenter list is empty"))
}

/// Resolve an image name/UUID to a UUID.
///
/// If `name_or_uuid` is already a UUID it is returned unchanged.
/// Otherwise the newest image whose name matches is returned.
pub async fn resolve_image_uuid(
    cloudapi: &TypedClient,
    account: &str,
    name_or_uuid: &str,
) -> Result<Uuid> {
    if let Ok(id) = uuid::Uuid::parse_str(name_or_uuid) {
        return Ok(id);
    }
    find_newest_image_by_name(cloudapi, account, name_or_uuid).await
}

/// Find the UUID of the newest image with a given name.
pub async fn find_newest_image_by_name(
    cloudapi: &TypedClient,
    account: &str,
    name: &str,
) -> Result<Uuid> {
    let images = cloudapi
        .inner()
        .list_images()
        .account(account)
        .send()
        .await
        .context("list images")?
        .into_inner();

    let mut matching: Vec<_> = images.into_iter().filter(|i| i.name == name).collect();
    if matching.is_empty() {
        bail!("no image named '{name}' found");
    }
    matching.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(matching[0].id)
}

/// Resolve a package name or UUID to a UUID.
pub async fn resolve_package_uuid(
    cloudapi: &TypedClient,
    account: &str,
    name_or_uuid: &str,
) -> Result<Uuid> {
    if let Ok(id) = uuid::Uuid::parse_str(name_or_uuid) {
        return Ok(id);
    }
    let packages = cloudapi
        .inner()
        .list_packages()
        .account(account)
        .send()
        .await
        .context("list packages")?
        .into_inner();

    packages
        .into_iter()
        .find(|p| p.name == name_or_uuid)
        .map(|p| p.id)
        .ok_or_else(|| anyhow::anyhow!("package '{name_or_uuid}' not found"))
}

// ---------------------------------------------------------------------------
// Kubernetes helpers
// ---------------------------------------------------------------------------

/// Apply a multi-document YAML manifest using server-side apply.
///
/// Documents are parsed eagerly (synchronously) before any async k8s call so
/// that the non-`Send` `serde_yaml::Deserializer` is not held across `.await`.
pub async fn apply_yaml_manifest(client: &kube::Client, yaml: &str) -> Result<()> {
    let documents: Vec<serde_json::Value> = {
        let mut docs = Vec::new();
        for document in serde_yaml::Deserializer::from_str(yaml) {
            let value: serde_json::Value =
                serde_yaml::with::singleton_map_recursive::deserialize(document)
                    .context("parse YAML document")?;
            if !value.is_null() {
                docs.push(value);
            }
        }
        docs
    };

    for value in documents {
        apply_dynamic_object(client, &value).await?;
    }
    Ok(())
}

async fn apply_dynamic_object(client: &kube::Client, value: &serde_json::Value) -> Result<()> {
    let api_version = value["apiVersion"]
        .as_str()
        .context("object missing apiVersion")?;
    let kind = value["kind"].as_str().context("object missing kind")?;
    let name = value["metadata"]["name"]
        .as_str()
        .context("object missing metadata.name")?;
    let namespace = value["metadata"]["namespace"].as_str();

    let ar = api_resource_for(api_version, kind)?;
    let pp = PatchParams::apply(FIELD_MANAGER);

    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
        None => {
            if is_cluster_scoped(kind) {
                Api::all_with(client.clone(), &ar)
            } else {
                Api::default_namespaced_with(client.clone(), &ar)
            }
        }
    };

    api.patch(name, &pp, &Patch::Apply(value))
        .await
        .with_context(|| format!("apply {kind} '{name}'"))?;
    Ok(())
}

fn api_resource_for(api_version: &str, kind: &str) -> Result<kube::api::ApiResource> {
    let (group, version) = if api_version.contains('/') {
        let (g, v) = api_version.split_once('/').unwrap();
        (g, v)
    } else {
        ("", api_version)
    };

    let plural = match kind {
        "ServiceAccount" => "serviceaccounts",
        "ClusterRole" => "clusterroles",
        "ClusterRoleBinding" => "clusterrolebindings",
        "Role" => "roles",
        "RoleBinding" => "rolebindings",
        "Secret" => "secrets",
        "ConfigMap" => "configmaps",
        "Deployment" => "deployments",
        "Service" => "services",
        _ => bail!("unsupported resource kind: {kind}"),
    };

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

/// Create or replace a Secret with string data.
pub async fn upsert_secret(
    client: &kube::Client,
    name: &str,
    namespace: &str,
    data: BTreeMap<String, String>,
) -> Result<()> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = Secret {
        metadata: kube::core::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        string_data: Some(data),
        ..Default::default()
    };
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(&secret),
    )
    .await
    .with_context(|| format!("upsert Secret '{name}'"))?;
    Ok(())
}

/// Create or replace a ConfigMap.
pub async fn upsert_configmap(
    client: &kube::Client,
    name: &str,
    namespace: &str,
    data: BTreeMap<String, String>,
) -> Result<()> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let cm = ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    api.patch(name, &PatchParams::apply(FIELD_MANAGER), &Patch::Apply(&cm))
        .await
        .with_context(|| format!("upsert ConfigMap '{name}'"))?;
    Ok(())
}

/// Delete a namespaced resource, ignoring NotFound.
pub async fn delete_namespaced<T>(client: &kube::Client, name: &str, namespace: &str) -> Result<()>
where
    T: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + Debug
        + Send
        + Sync,
    <T as kube::Resource>::DynamicType: Default,
{
    let api: Api<T> = Api::namespaced(client.clone(), namespace);
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(anyhow::anyhow!(e)).with_context(|| format!("delete {name}")),
    }
}

/// Delete a cluster-scoped resource, ignoring NotFound.
pub async fn delete_cluster_scoped<T>(client: &kube::Client, name: &str) -> Result<()>
where
    T: kube::Resource<Scope = k8s_openapi::ClusterResourceScope>
        + Clone
        + DeserializeOwned
        + Debug
        + Send
        + Sync,
    <T as kube::Resource>::DynamicType: Default,
{
    let api: Api<T> = Api::all(client.clone());
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(anyhow::anyhow!(e)).with_context(|| format!("delete {name}")),
    }
}

/// Get the `triton-lb-controller` Deployment, returning `None` if absent.
pub async fn get_lb_deployment(client: &kube::Client) -> Result<Option<Deployment>> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), "kube-system");
    api.get_opt("triton-lb-controller")
        .await
        .context("get triton-lb-controller Deployment")
}

/// Poll until the Deployment has available_replicas >= spec replicas or timeout.
pub async fn wait_for_deployment_ready(client: &kube::Client, timeout: Duration) -> Result<()> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), "kube-system");
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout {
            bail!("timeout waiting for triton-lb-controller Deployment to be ready");
        }
        if let Some(dep) = api.get_opt("triton-lb-controller").await? {
            let desired = dep.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
            let available = dep
                .status
                .as_ref()
                .and_then(|s| s.available_replicas)
                .unwrap_or(0);
            if available >= desired {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Type aliases re-exported for use in `main.rs` remove handler.
pub use k8s_openapi::api::apps::v1::Deployment as K8sDeployment;
pub use k8s_openapi::api::core::v1::ConfigMap as K8sConfigMap;
pub use k8s_openapi::api::core::v1::Secret as K8sSecret;
pub use k8s_openapi::api::core::v1::ServiceAccount as K8sServiceAccount;
pub use k8s_openapi::api::rbac::v1::ClusterRole as K8sClusterRole;
pub use k8s_openapi::api::rbac::v1::ClusterRoleBinding as K8sClusterRoleBinding;
