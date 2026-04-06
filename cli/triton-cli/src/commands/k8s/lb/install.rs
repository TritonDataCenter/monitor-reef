// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Install the Triton LoadBalancer controller into a Kubernetes cluster

use anyhow::{Context, Result, bail};
use clap::Args;
use cloudapi_client::TypedClient;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::super::kube_client;

use super::{
    DEFAULT_CONTROLLER_IMAGE, DEFAULT_LB_IMAGE_NAME, DEFAULT_LB_PACKAGE, DEPLOYMENT_YAML_TEMPLATE,
    RBAC_YAML,
};
use crate::commands::k8s::state::ClusterState;
use crate::config::profile::Profile;

#[derive(Args, Clone)]
pub struct InstallArgs {
    /// Cluster name or UUID
    pub cluster: String,

    /// Package for LoadBalancer instances
    #[arg(long, default_value = DEFAULT_LB_PACKAGE)]
    pub package: String,

    /// Image for LoadBalancer instances (name or UUID)
    ///
    /// Defaults to the newest image named "cloud-load-balancer".
    #[arg(long)]
    pub image: Option<String>,

    /// Override the external CNS suffix (auto-discovered from public network)
    #[arg(long)]
    pub external_cns_suffix: Option<String>,

    /// Override the public network UUID (auto-discovered)
    #[arg(long)]
    pub public_network: Option<Uuid>,

    /// Path to SSH private key file
    ///
    /// If not specified, the key is located by scanning ~/.ssh/ for a key
    /// matching the profile's fingerprint.
    #[arg(long)]
    pub key_path: Option<PathBuf>,

    /// Controller container image
    #[arg(long, default_value = DEFAULT_CONTROLLER_IMAGE)]
    pub controller_image: String,
}

/// Configuration values for the LB controller
struct ControllerConfig {
    triton_url: String,
    triton_account: String,
    triton_insecure: bool,
    datacenter: String,
    cns_suffix: String,
    external_cns_suffix: String,
    default_package: String,
    default_image: Uuid,
    public_network: Uuid,
    fabric_network: Uuid,
    worker_cns_name: String,
    cluster_name: String,
}

pub async fn run(
    args: InstallArgs,
    client: &TypedClient,
    profile: &Profile,
    _json: bool,
) -> Result<()> {
    eprintln!("==> Loading cluster state");

    let cluster = ClusterState::load_by_name_or_uuid(&args.cluster)
        .await
        .context("Failed to load cluster state")?;

    eprintln!("    Cluster: {} ({})", cluster.name, cluster.uuid);

    // Verify cluster has a fabric network (required for LB controller)
    let fabric_network_id = cluster
        .fabric_network_id
        .ok_or_else(|| anyhow::anyhow!("Cluster has no fabric network configured"))?;

    // Get kubeconfig path
    let kubeconfig_path = cluster.cluster_dir()?.join("kubeconfig");
    if !kubeconfig_path.exists() {
        bail!(
            "Kubeconfig not found at {}. Has the cluster been bootstrapped?",
            kubeconfig_path.display()
        );
    }

    eprintln!("==> Discovering configuration");

    // Discover public network
    let public_network_id = if let Some(id) = args.public_network {
        eprintln!(
            "    Using specified public network: {}",
            &id.to_string()[..8]
        );
        id
    } else {
        let id = discover_public_network(client).await?;
        eprintln!("    Public network: {}", &id.to_string()[..8]);
        id
    };

    // Discover CNS suffix from public network
    let (cns_suffix, external_cns_suffix) =
        discover_cns_suffixes(public_network_id, client).await?;
    eprintln!("    CNS suffix: {}", cns_suffix);

    let external_cns_suffix = if let Some(suffix) = args.external_cns_suffix {
        eprintln!("    External CNS suffix (override): {}", suffix);
        suffix
    } else {
        eprintln!("    External CNS suffix: {}", external_cns_suffix);
        external_cns_suffix
    };

    // Discover fabric network details for worker CNS name
    let fabric_info =
        super::super::provisioning::discover_fabric_network(fabric_network_id, client)
            .await
            .context("Failed to discover fabric network")?;
    eprintln!(
        "    Fabric network: {} ({})",
        fabric_info.name,
        &fabric_network_id.to_string()[..8]
    );

    // Discover datacenter
    let datacenter = discover_datacenter(client).await?;
    eprintln!("    Datacenter: {}", datacenter);

    // Compute worker CNS name
    // Format: {fabric-name}.worker.svc.{account}.{datacenter}.{cns-suffix}
    let worker_cns_name = format!(
        "{}.worker.svc.{}.{}.{}",
        fabric_info.name, profile.account, datacenter, cns_suffix
    );
    eprintln!("    Worker CNS name: {}", worker_cns_name);

    // Resolve LB instance image
    let lb_image_id = if let Some(ref image) = args.image {
        super::super::provisioning::resolve_image_id(image, client).await?
    } else {
        find_newest_image_by_name(DEFAULT_LB_IMAGE_NAME, client).await?
    };
    eprintln!("    LB image: {}", &lb_image_id.to_string()[..8]);

    // Resolve LB instance package
    let lb_package_id =
        super::super::provisioning::resolve_package_id(&args.package, client).await?;
    eprintln!(
        "    LB package: {} ({})",
        args.package,
        &lb_package_id.to_string()[..8]
    );

    // Find private key and determine the key_id to use
    // If --key-path is specified, we compute the fingerprint from that key
    // Otherwise, we use the profile's key_id and find the matching key file
    let (private_key_path, key_id) = if let Some(path) = args.key_path {
        if !path.exists() {
            bail!("Specified key file does not exist: {}", path.display());
        }
        // Compute fingerprint from the provided key's public key
        let pub_path = path.with_extension("pub");
        if !pub_path.exists() {
            bail!(
                "Public key not found at {}. The .pub file is required to compute the fingerprint.",
                pub_path.display()
            );
        }
        let pub_content = tokio::fs::read_to_string(&pub_path)
            .await
            .context("Failed to read public key file")?;
        let pubkey = ssh_key::PublicKey::from_openssh(&pub_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse public key: {}", e))?;
        let fp = compute_md5_fingerprint(&pubkey)?;
        (path, fp)
    } else {
        let path = find_key_path_for_fingerprint(&profile.key_id)
            .await
            .context("Failed to find private key for profile fingerprint")?;
        (path, profile.key_id.clone())
    };
    eprintln!("    Private key: {}", private_key_path.display());
    eprintln!("    Key ID: {}", key_id);

    // Read private key content
    let private_key_content = tokio::fs::read_to_string(&private_key_path)
        .await
        .context("Failed to read private key file")?;

    // Build controller config
    let config = ControllerConfig {
        triton_url: profile.url.clone(),
        triton_account: profile.account.clone(),
        triton_insecure: profile.insecure,
        datacenter,
        cns_suffix,
        external_cns_suffix,
        default_package: lb_package_id.to_string(),
        default_image: lb_image_id,
        public_network: public_network_id,
        fabric_network: fabric_network_id,
        worker_cns_name,
        cluster_name: cluster.name.clone(),
    };

    eprintln!("==> Installing LoadBalancer controller");

    // Create Kubernetes client
    let k8s_client = kube_client::client_from_kubeconfig(&kubeconfig_path).await?;

    // Apply RBAC
    eprintln!("    Applying RBAC...");
    kube_client::apply_yaml_manifest(&k8s_client, RBAC_YAML).await?;

    // Create secret with credentials
    eprintln!("    Creating credentials secret...");
    let mut secret_data = BTreeMap::new();
    secret_data.insert("key-id".to_string(), key_id.clone());
    secret_data.insert("private-key".to_string(), private_key_content.clone());
    kube_client::create_or_update_secret(
        &k8s_client,
        "triton-credentials",
        "kube-system",
        secret_data,
    )
    .await?;

    // Create configmap
    eprintln!("    Creating configuration...");
    let config_data = build_configmap_data(&config);
    kube_client::create_or_update_configmap(
        &k8s_client,
        "triton-lb-controller-config",
        "kube-system",
        config_data,
    )
    .await?;

    // Apply deployment
    eprintln!("    Deploying controller...");
    let deployment_yaml =
        DEPLOYMENT_YAML_TEMPLATE.replace("{{CONTROLLER_IMAGE}}", &args.controller_image);
    kube_client::apply_yaml_manifest(&k8s_client, &deployment_yaml).await?;

    // Wait for rollout
    eprintln!("    Waiting for controller to be ready...");
    kube_client::wait_for_deployment_ready(
        &k8s_client,
        "triton-lb-controller",
        "kube-system",
        Duration::from_secs(180),
    )
    .await?;

    eprintln!();
    eprintln!("==> LoadBalancer controller installed successfully!");
    eprintln!();
    eprintln!("To create a LoadBalancer service:");
    eprintln!(
        "  kubectl --kubeconfig {} apply -f - <<EOF",
        kubeconfig_path.display()
    );
    eprintln!("apiVersion: v1");
    eprintln!("kind: Service");
    eprintln!("metadata:");
    eprintln!("  name: my-service");
    eprintln!("spec:");
    eprintln!("  type: LoadBalancer");
    eprintln!("  selector:");
    eprintln!("    app: my-app");
    eprintln!("  ports:");
    eprintln!("    - port: 80");
    eprintln!("      targetPort: 8080");
    eprintln!("EOF");

    Ok(())
}

/// Build the ConfigMap data for the controller configuration.
fn build_configmap_data(config: &ControllerConfig) -> BTreeMap<String, String> {
    let mut data = BTreeMap::new();
    data.insert("triton-url".to_string(), config.triton_url.clone());
    data.insert("triton-account".to_string(), config.triton_account.clone());
    data.insert(
        "triton-insecure".to_string(),
        config.triton_insecure.to_string(),
    );
    data.insert("datacenter".to_string(), config.datacenter.clone());
    data.insert("cns-suffix".to_string(), config.cns_suffix.clone());
    data.insert(
        "external-cns-suffix".to_string(),
        config.external_cns_suffix.clone(),
    );
    data.insert(
        "default-package".to_string(),
        config.default_package.clone(),
    );
    data.insert(
        "default-image".to_string(),
        config.default_image.to_string(),
    );
    data.insert(
        "public-network".to_string(),
        config.public_network.to_string(),
    );
    data.insert(
        "fabric-network".to_string(),
        config.fabric_network.to_string(),
    );
    data.insert(
        "worker-cns-name".to_string(),
        config.worker_cns_name.clone(),
    );
    data.insert("cluster-name".to_string(), config.cluster_name.clone());
    data.insert("requeue-after-seconds".to_string(), "30".to_string());
    data
}

/// Discover the first public, non-fabric network
async fn discover_public_network(client: &TypedClient) -> Result<Uuid> {
    let account = client.effective_account();

    let response = client
        .inner()
        .list_networks()
        .account(account)
        .send()
        .await
        .context("Failed to list networks")?;

    let networks = response.into_inner();

    for network in networks {
        if network.public && !network.fabric.unwrap_or(false) {
            return Ok(network.id);
        }
    }

    bail!("No public non-fabric network found")
}

/// Discover CNS suffixes from a public network
///
/// Returns (cns_suffix, external_cns_suffix).
/// The CNS suffix is typically like "cns.us-west-1.triton.zone".
/// The external CNS suffix is extracted by removing the "cns." prefix if present,
/// or looking for a suffix pattern that doesn't start with "svc.".
async fn discover_cns_suffixes(network_id: Uuid, client: &TypedClient) -> Result<(String, String)> {
    let account = client.effective_account();

    let response = client
        .inner()
        .get_network()
        .account(account)
        .network(network_id.to_string())
        .send()
        .await
        .context("Failed to get network details")?;

    let network = response.into_inner();

    let suffixes = network
        .suffixes
        .ok_or_else(|| anyhow::anyhow!("Network has no CNS suffixes configured"))?;

    if suffixes.is_empty() {
        bail!("Network has empty CNS suffixes list");
    }

    // The first suffix is typically the main CNS suffix
    let cns_suffix = suffixes[0].clone();

    // For external CNS suffix, we look for a pattern or derive it
    // Common patterns:
    //   cns.us-west-1.triton.zone -> us-west-1.triton.zone (external)
    //   cns.capsule.corp -> ext.corp or capsule.corp
    //
    // Try to find a suffix that looks like an external one, or derive it
    let external_cns_suffix = suffixes
        .iter()
        .find(|s| !s.starts_with("cns.") && !s.starts_with("svc."))
        .cloned()
        .unwrap_or_else(|| {
            // Derive from CNS suffix by removing "cns." prefix
            if let Some(stripped) = cns_suffix.strip_prefix("cns.") {
                stripped.to_string()
            } else {
                cns_suffix.clone()
            }
        });

    Ok((cns_suffix, external_cns_suffix))
}

/// Discover the datacenter name
async fn discover_datacenter(client: &TypedClient) -> Result<String> {
    let account = client.effective_account();

    // List datacenters and find the one that matches our URL
    let response = client
        .inner()
        .list_datacenters()
        .account(account)
        .send()
        .await
        .context("Failed to list datacenters")?;

    let datacenters = response.into_inner();

    // The datacenters response is a map of name -> URL
    // We need to find which one matches our current connection
    // For simplicity, if there's only one, use it
    // Otherwise, we'd need to match against the client's URL

    if datacenters.is_empty() {
        bail!("No datacenters found");
    }

    // If there's only one datacenter, use it
    if datacenters.len() == 1 {
        return datacenters
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Datacenter list unexpectedly empty"));
    }

    // Try to match by URL - the client's base URL should match one of the datacenter URLs
    // For now, just use the first one and log a warning
    let first_dc = datacenters
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Datacenter list unexpectedly empty"))?;
    eprintln!(
        "    Warning: Multiple datacenters found, using '{}'",
        first_dc
    );

    Ok(first_dc)
}

/// Find the newest image matching a name pattern
async fn find_newest_image_by_name(name: &str, client: &TypedClient) -> Result<Uuid> {
    let account = client.effective_account();

    let response = client
        .inner()
        .list_images()
        .account(account)
        .send()
        .await
        .context("Failed to list images")?;

    let images = response.into_inner();

    // Filter images by name and find the newest by published_at
    let matching: Vec<_> = images.into_iter().filter(|img| img.name == name).collect();

    if matching.is_empty() {
        bail!("No image found with name '{}'", name);
    }

    // Sort by published_at descending (newest first)
    // We know matching is non-empty from the check above
    let newest = matching
        .into_iter()
        .max_by_key(|img| img.published_at.clone())
        .ok_or_else(|| anyhow::anyhow!("No matching images found"))?;

    Ok(newest.id)
}

/// Find the private key file path for a given fingerprint
///
/// Scans ~/.ssh/ for .pub files, computes fingerprints, and returns the
/// corresponding private key path when a match is found.
async fn find_key_path_for_fingerprint(fingerprint: &str) -> Result<PathBuf> {
    let ssh_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
        .join(".ssh");

    if !ssh_dir.exists() {
        bail!("SSH directory not found: {}", ssh_dir.display());
    }

    let target_fp = triton_auth::Fingerprint::parse(fingerprint)
        .map_err(|e| anyhow::anyhow!("Failed to parse fingerprint from profile: {}", e))?;

    let mut entries = tokio::fs::read_dir(&ssh_dir)
        .await
        .context("Failed to read SSH directory")?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Look for .pub files
        if path.extension().is_some_and(|ext| ext == "pub")
            && let Ok(content) = tokio::fs::read_to_string(&path).await
            && let Ok(pubkey) = ssh_key::PublicKey::from_openssh(&content)
            && target_fp.matches(&pubkey)
        {
            // Found the matching public key, return the private key path
            let private_key_path = path.with_extension("");
            if private_key_path.exists() {
                return Ok(private_key_path);
            }
        }
    }

    bail!(
        "Could not find private key for fingerprint {} in {}",
        fingerprint,
        ssh_dir.display()
    )
}

/// Compute the MD5 fingerprint of a public key in colon-separated hex format
///
/// Returns a string like "61:e8:ca:1b:0f:31:e1:bf:7d:a6:a5:53:89:16:d1:32"
fn compute_md5_fingerprint(pubkey: &ssh_key::PublicKey) -> Result<String> {
    use md5::{Digest, Md5}; // md5 is re-exported from md-5 crate

    let key_bytes = pubkey
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("Failed to encode public key: {}", e))?;

    let mut hasher = Md5::new();
    hasher.update(&key_bytes);
    let result = hasher.finalize();

    let hex_parts: Vec<String> = result.iter().map(|b| format!("{:02x}", b)).collect();
    Ok(hex_parts.join(":"))
}
