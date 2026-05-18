// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Local machine config (`controlplane.yaml` / `worker.yaml`) mutation.
//!
//! The bootstrap flow stores both files in `cluster_dir/` and treats them as
//! the source-of-truth for what's on each node. The upgrade flow patches
//! image fields in those files and pushes them back via
//! `talos::apply_config::run_via` exactly the way `control add` and
//! `worker add` already do.

use anyhow::{Context, Result};
use serde_yaml::Value;
use std::path::Path;

/// One of the image-bearing fields we know how to mutate.
#[derive(Debug, Clone, Copy)]
pub enum ImageField {
    ApiServer,

    ControllerManager,

    Scheduler,

    Proxy,

    Kubelet,
}

impl ImageField {
    /// YAML key path from the document root.
    pub fn path(self) -> &'static [&'static str] {
        match self {
            ImageField::ApiServer => &["cluster", "apiServer", "image"],
            ImageField::ControllerManager => &["cluster", "controllerManager", "image"],
            ImageField::Scheduler => &["cluster", "scheduler", "image"],
            ImageField::Proxy => &["cluster", "proxy", "image"],
            ImageField::Kubelet => &["machine", "kubelet", "image"],
        }
    }

    #[allow(dead_code)]
    pub fn description(self) -> &'static str {
        match self {
            ImageField::ApiServer => "cluster.apiServer.image",
            ImageField::ControllerManager => "cluster.controllerManager.image",
            ImageField::Scheduler => "cluster.scheduler.image",
            ImageField::Proxy => "cluster.proxy.image",
            ImageField::Kubelet => "machine.kubelet.image",
        }
    }
}

/// Read the YAML file at `path` and return the first document as a Value.
pub async fn load_yaml(path: &Path) -> Result<Value> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    // Use only the first YAML document, mirroring apply_config's behavior
    // for multi-doc files.
    let first = content.split("\n---").next().unwrap_or(&content).trim();
    serde_yaml::from_str(first).with_context(|| format!("parsing {}", path.display()))
}

/// Path to the per-node network patch file, if it exists. Naming matches
/// what `bootstrap.rs` / `control add` / `worker add` already use:
/// `<cluster_dir>/<node_name>-network-patch.yaml`.
pub async fn network_patch_path(cluster_dir: &Path, node_name: &str) -> Option<std::path::PathBuf> {
    let path = cluster_dir.join(format!("{}-network-patch.yaml", node_name));
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        Some(path)
    } else {
        None
    }
}

/// Write `value` back to `path` as YAML.
pub async fn save_yaml(path: &Path, value: &Value) -> Result<()> {
    let serialized =
        serde_yaml::to_string(value).with_context(|| format!("serializing {}", path.display()))?;
    tokio::fs::write(path, serialized)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Set the image field at the well-known path. Returns the previous value
/// (if any) so callers can log the diff.
pub fn set_image(doc: &mut Value, field: ImageField, image: &str) -> Result<Option<String>> {
    let path = field.path();
    let prev = get_str_at(doc, path);
    set_str_at(doc, path, image)?;
    Ok(prev)
}

/// Convenience: read the current image string at the field, if present.
#[allow(dead_code)]
pub fn get_image(doc: &Value, field: ImageField) -> Option<String> {
    get_str_at(doc, field.path())
}

fn get_str_at(doc: &Value, path: &[&str]) -> Option<String> {
    let mut node = doc;
    for segment in path {
        node = node.get(*segment)?;
    }
    node.as_str().map(|s| s.to_string())
}

fn set_str_at(doc: &mut Value, path: &[&str], value: &str) -> Result<()> {
    if path.is_empty() {
        anyhow::bail!("empty YAML path");
    }
    let mut node = doc;
    for segment in &path[..path.len() - 1] {
        if !matches!(node, Value::Mapping(_)) {
            anyhow::bail!("cannot descend into non-mapping at '{}'", segment);
        }
        let key = Value::String((*segment).to_string());
        // Ensure the intermediate mapping exists.
        if let Value::Mapping(map) = node {
            if !map.contains_key(&key) {
                map.insert(key.clone(), Value::Mapping(Default::default()));
            }
            node = map
                .get_mut(&key)
                .ok_or_else(|| anyhow::anyhow!("just-inserted key missing"))?;
        }
    }
    let final_key = path.last().unwrap();
    if let Value::Mapping(map) = node {
        map.insert(
            Value::String((*final_key).to_string()),
            Value::String(value.to_string()),
        );
        Ok(())
    } else {
        anyhow::bail!("cannot set field on non-mapping value")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> Value {
        serde_yaml::from_str(
            r#"
machine:
  type: controlplane
  kubelet:
    image: ghcr.io/siderolabs/kubelet:v1.35.0
cluster:
  apiServer:
    image: registry.k8s.io/kube-apiserver:v1.35.0
  controllerManager:
    image: registry.k8s.io/kube-controller-manager:v1.35.0
  scheduler:
    image: registry.k8s.io/kube-scheduler:v1.35.0
  proxy:
    image: registry.k8s.io/kube-proxy:v1.35.0
"#,
        )
        .expect("test fixture parses")
    }

    #[test]
    fn reads_known_image_fields() {
        let doc = sample_config();
        assert_eq!(
            get_image(&doc, ImageField::ApiServer).as_deref(),
            Some("registry.k8s.io/kube-apiserver:v1.35.0")
        );
        assert_eq!(
            get_image(&doc, ImageField::Kubelet).as_deref(),
            Some("ghcr.io/siderolabs/kubelet:v1.35.0")
        );
    }

    #[test]
    fn writes_image_back_at_path() {
        let mut doc = sample_config();
        let prev = set_image(
            &mut doc,
            ImageField::ApiServer,
            "registry.k8s.io/kube-apiserver:v1.36.0",
        )
        .expect("set image");
        assert_eq!(
            prev.as_deref(),
            Some("registry.k8s.io/kube-apiserver:v1.35.0")
        );
        assert_eq!(
            get_image(&doc, ImageField::ApiServer).as_deref(),
            Some("registry.k8s.io/kube-apiserver:v1.36.0")
        );
    }

    #[test]
    fn creates_missing_intermediates() {
        let mut doc: Value = serde_yaml::from_str("machine:\n  type: worker").expect("parse");
        set_image(
            &mut doc,
            ImageField::Kubelet,
            "ghcr.io/siderolabs/kubelet:v1.36.0",
        )
        .expect("set");
        assert_eq!(
            get_image(&doc, ImageField::Kubelet).as_deref(),
            Some("ghcr.io/siderolabs/kubelet:v1.36.0")
        );
    }
}
