/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Context, Result, bail};
use std::path::Path;

use super::client::{self, NodeTargetInterceptor};
use super::proto::machine;
use super::retry;

/// Apply a Talos machine configuration with optional patches to a node.
///
/// This function applies a base configuration and merges it with zero or more
/// patch documents. The configuration is applied in NO_REBOOT mode, meaning
/// the node will not automatically reboot after applying the configuration.
///
/// # Arguments
///
/// * `endpoint` - The Talos node endpoint (hostname or IP address)
/// * `base_config_path` - Path to the base configuration YAML file
/// * `patch_paths` - Array of paths to patch YAML files to apply
/// * `talosconfig` - Optional path to talosconfig file (uses default if None)
/// * `do_retry` - Whether to retry the operation on failure with exponential backoff
/// * `verbose` - Whether to print verbose output during the operation
///
/// # Multi-document YAML Handling
///
/// When processing multi-document YAML files (documents separated by `---`),
/// only the first document in each file is used for configuration. Subsequent
/// documents are ignored. This matches Talos behavior where machine config
/// is the first document and additional documents (like HostnameConfig) are
/// metadata only.
///
/// # Config Merging
///
/// Patches are applied using strategic merge semantics where patch values
/// override base values at the same path. Maps are merged recursively, while
/// arrays and scalars are replaced entirely.
pub async fn run(
    endpoint: &str,
    base_config_path: &Path,
    patch_paths: &[&Path],
    talosconfig: Option<&str>,
    do_retry: bool,
    verbose: bool,
) -> Result<()> {
    run_via(
        endpoint,
        None,
        base_config_path,
        patch_paths,
        talosconfig,
        do_retry,
        verbose,
    )
    .await
}

/// Apply a Talos machine configuration via a proxy node.
///
/// Similar to `run`, but routes the request through the endpoint to a target node.
/// This is useful for applying config to worker nodes that are only reachable via
/// the control plane.
///
/// # Arguments
///
/// * `endpoint` - The Talos API endpoint to connect to (control plane IP)
/// * `target_node` - Optional target node IP to route the request to via the endpoint
/// * `base_config_path` - Path to the base configuration YAML file
/// * `patch_paths` - Array of paths to patch YAML files to apply
/// * `talosconfig` - Optional path to talosconfig file
/// * `do_retry` - Whether to retry the operation on failure
/// * `verbose` - Whether to print verbose output
pub async fn run_via(
    endpoint: &str,
    target_node: Option<&str>,
    base_config_path: &Path,
    patch_paths: &[&Path],
    talosconfig: Option<&str>,
    do_retry: bool,
    verbose: bool,
) -> Result<()> {
    if do_retry {
        retry::with_retry(verbose, || {
            apply_config_once_via(
                endpoint,
                target_node,
                base_config_path,
                patch_paths,
                talosconfig,
                verbose,
            )
        })
        .await
    } else {
        apply_config_once_via(
            endpoint,
            target_node,
            base_config_path,
            patch_paths,
            talosconfig,
            verbose,
        )
        .await
    }
}

async fn apply_config_once_via(
    endpoint: &str,
    target_node: Option<&str>,
    base_config_path: &Path,
    patch_paths: &[&Path],
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<()> {
    // Load and merge the configuration
    let merged_config = load_and_merge_config(base_config_path, patch_paths, verbose).await?;

    // Serialize the merged configuration to YAML bytes
    let config_yaml = serde_yaml::to_string(&merged_config)
        .context("serializing merged configuration to YAML")?;

    let target_desc = if let Some(target) = target_node {
        format!("{} (via {})", target, endpoint)
    } else {
        endpoint.to_string()
    };

    if verbose {
        eprintln!("applying configuration to {}", target_desc);
        eprintln!("configuration size: {} bytes", config_yaml.len());
    }

    // Connect to the Talos node
    let channel = client::connect(endpoint, talosconfig, verbose).await?;

    // Build the ApplyConfiguration request
    let req = machine::ApplyConfigurationRequest {
        data: config_yaml.into_bytes(),
        mode: machine::apply_configuration_request::Mode::NoReboot as i32,
        dry_run: false,
        try_mode_timeout: None,
    };

    // Apply the configuration (with optional proxy routing)
    let resp = if let Some(target) = target_node {
        let interceptor = NodeTargetInterceptor::new(&[target]);
        let mut client = machine::machine_service_client::MachineServiceClient::with_interceptor(
            channel,
            interceptor,
        );
        client
            .apply_configuration(req)
            .await
            .context("applying configuration via gRPC (proxied)")?
            .into_inner()
    } else {
        let mut client = machine::machine_service_client::MachineServiceClient::new(channel);
        client
            .apply_configuration(req)
            .await
            .context("applying configuration via gRPC")?
            .into_inner()
    };

    // Check for errors in the response
    for msg in &resp.messages {
        if let Some(ref meta) = msg.metadata
            && !meta.error.is_empty()
        {
            bail!("apply configuration error: {}", meta.error);
        }
    }

    if verbose {
        eprintln!("configuration applied successfully to {}", target_desc);
    }

    Ok(())
}

/// Load base configuration and merge with patches using strategic merge semantics.
///
/// This function loads the base configuration from `base_config_path` and applies
/// each patch in order. Only the first YAML document in each file is used.
async fn load_and_merge_config(
    base_config_path: &Path,
    patch_paths: &[&Path],
    verbose: bool,
) -> Result<serde_yaml::Value> {
    // Load base configuration
    if verbose {
        eprintln!(
            "loading base configuration from {}",
            base_config_path.display()
        );
    }

    let base_content = tokio::fs::read_to_string(base_config_path)
        .await
        .with_context(|| format!("reading base config from {}", base_config_path.display()))?;

    let base_config = parse_first_document(&base_content)
        .with_context(|| format!("parsing base config from {}", base_config_path.display()))?;

    // Start with the base configuration
    let mut merged = base_config;

    // Apply each patch in order
    for patch_path in patch_paths {
        if verbose {
            eprintln!("applying patch from {}", patch_path.display());
        }

        let patch_content = tokio::fs::read_to_string(patch_path)
            .await
            .with_context(|| format!("reading patch from {}", patch_path.display()))?;

        let patch = parse_first_document(&patch_content)
            .with_context(|| format!("parsing patch from {}", patch_path.display()))?;

        merge_yaml(&mut merged, &patch);
    }

    Ok(merged)
}

/// Parse the first YAML document from a multi-document YAML string.
///
/// If the input contains multiple documents separated by `---`, only the first
/// document is parsed and returned. This matches Talos behavior where additional
/// documents (like HostnameConfig) are treated as metadata.
fn parse_first_document(yaml_content: &str) -> Result<serde_yaml::Value> {
    // Split on document separator and take the first document
    let first_doc = yaml_content
        .split("\n---")
        .next()
        .unwrap_or(yaml_content)
        .trim();

    serde_yaml::from_str(first_doc).context("parsing YAML document")
}

/// Merge two YAML values using strategic merge semantics.
///
/// This implements a simplified strategic merge where:
/// - Maps are merged recursively (keys in patch override keys in base)
/// - Arrays are replaced entirely (not merged element-by-element)
/// - Scalars are replaced entirely
/// - Null values in patches remove the corresponding key from base
fn merge_yaml(base: &mut serde_yaml::Value, patch: &serde_yaml::Value) {
    use serde_yaml::Value;

    match (base, patch) {
        // Both are maps - merge recursively
        (Value::Mapping(base_map), Value::Mapping(patch_map)) => {
            for (key, patch_value) in patch_map {
                if patch_value.is_null() {
                    // Null in patch means delete the key
                    base_map.remove(key);
                } else if let Some(base_value) = base_map.get_mut(key) {
                    // Key exists in both - recurse
                    merge_yaml(base_value, patch_value);
                } else {
                    // Key only in patch - add it
                    base_map.insert(key.clone(), patch_value.clone());
                }
            }
        }
        // For all other cases (arrays, scalars, type mismatches), patch replaces base
        (base_value, patch_value) => {
            *base_value = patch_value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_first_document() {
        let multi_doc = r#"
version: v1alpha1
machine:
  type: controlplane
---
apiVersion: v1alpha1
kind: HostnameConfig
auto: stable
"#;

        let result = parse_first_document(multi_doc).expect("failed to parse");

        // Should only get the first document
        assert!(result.is_mapping());
        let map = result.as_mapping().expect("not a mapping");
        assert!(map.contains_key("version"));
        assert!(map.contains_key("machine"));
        // Should NOT contain the second document's keys
        assert!(!map.contains_key("apiVersion"));
        assert!(!map.contains_key("kind"));
    }

    #[test]
    fn test_merge_yaml_maps() {
        let base_yaml = r#"
machine:
  type: controlplane
  token: abc123
cluster:
  name: test
"#;
        let patch_yaml = r#"
machine:
  type: worker
  install:
    disk: /dev/sda
cluster:
  endpoint: https://10.0.0.1:6443
"#;

        let mut base: serde_yaml::Value = serde_yaml::from_str(base_yaml).expect("parse base");
        let patch: serde_yaml::Value = serde_yaml::from_str(patch_yaml).expect("parse patch");

        merge_yaml(&mut base, &patch);

        let result = base.as_mapping().expect("not a mapping");

        // Check machine section was merged
        let machine = result
            .get("machine")
            .expect("no machine")
            .as_mapping()
            .expect("not a mapping");
        assert_eq!(machine.get("type").and_then(|v| v.as_str()), Some("worker")); // overridden
        assert_eq!(
            machine.get("token").and_then(|v| v.as_str()),
            Some("abc123")
        ); // preserved
        assert!(machine.contains_key("install")); // added

        // Check cluster section was merged
        let cluster = result
            .get("cluster")
            .expect("no cluster")
            .as_mapping()
            .expect("not a mapping");
        assert_eq!(cluster.get("name").and_then(|v| v.as_str()), Some("test")); // preserved
        assert!(cluster.contains_key("endpoint")); // added
    }

    #[test]
    fn test_merge_yaml_null_removes_key() {
        let base_yaml = r#"
machine:
  type: controlplane
  token: abc123
  install:
    disk: /dev/sda
"#;
        let patch_yaml = r#"
machine:
  token: null
"#;

        let mut base: serde_yaml::Value = serde_yaml::from_str(base_yaml).expect("parse base");
        let patch: serde_yaml::Value = serde_yaml::from_str(patch_yaml).expect("parse patch");

        merge_yaml(&mut base, &patch);

        let result = base.as_mapping().expect("not a mapping");
        let machine = result
            .get("machine")
            .expect("no machine")
            .as_mapping()
            .expect("not a mapping");

        // token should be removed
        assert!(!machine.contains_key("token"));
        // other keys should remain
        assert!(machine.contains_key("type"));
        assert!(machine.contains_key("install"));
    }

    #[test]
    fn test_merge_yaml_array_replace() {
        let base_yaml = r#"
items:
  - a
  - b
  - c
"#;
        let patch_yaml = r#"
items:
  - x
  - y
"#;

        let mut base: serde_yaml::Value = serde_yaml::from_str(base_yaml).expect("parse base");
        let patch: serde_yaml::Value = serde_yaml::from_str(patch_yaml).expect("parse patch");

        merge_yaml(&mut base, &patch);

        let result = base.as_mapping().expect("not a mapping");
        let items = result
            .get("items")
            .expect("no items")
            .as_sequence()
            .expect("not an array");

        // Array should be completely replaced, not merged
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("x"));
        assert_eq!(items[1].as_str(), Some("y"));
    }

    #[test]
    fn test_merge_yaml_scalar_replace() {
        let base_yaml = r#"
value: 42
"#;
        let patch_yaml = r#"
value: "hello"
"#;

        let mut base: serde_yaml::Value = serde_yaml::from_str(base_yaml).expect("parse base");
        let patch: serde_yaml::Value = serde_yaml::from_str(patch_yaml).expect("parse patch");

        merge_yaml(&mut base, &patch);

        let result = base.as_mapping().expect("not a mapping");

        // Scalar should be replaced even with type change
        assert_eq!(result.get("value").and_then(|v| v.as_str()), Some("hello"));
    }
}
