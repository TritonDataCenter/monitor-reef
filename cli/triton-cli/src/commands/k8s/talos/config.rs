// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Talos configuration and secrets generation
//!
//! This module implements native generation of Talos cluster secrets and
//! machine configurations, compatible with what `talosctl gen secrets` produces.

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand::Rng;
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use time::{Duration, OffsetDateTime};

// Kubernetes version constant
const KUBERNETES_VERSION: &str = "v1.35.0";

/// Talos secrets bundle
///
/// This structure matches the output of `talosctl gen secrets` and contains
/// all the cryptographic material needed to bootstrap a Talos cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsBundle {
    /// Cluster-wide configuration
    pub cluster: ClusterSecrets,

    /// Kubernetes bootstrap and encryption secrets
    pub secrets: KubernetesSecrets,

    /// Trustd authentication token
    #[serde(rename = "trustdinfo")]
    pub trustd_info: TrustdInfo,

    /// Certificate authorities and keys
    pub certs: Certificates,
}

/// Cluster-wide secrets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSecrets {
    /// Base64-encoded cluster ID (32 bytes)
    pub id: String,

    /// Base64-encoded cluster secret (32 bytes)
    pub secret: String,
}

/// Kubernetes-related secrets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubernetesSecrets {
    /// Bootstrap token in format "abcdef.0123456789abcdef"
    #[serde(rename = "bootstraptoken")]
    pub bootstrap_token: String,

    /// Base64-encoded secretbox encryption secret (32 bytes)
    #[serde(rename = "secretboxencryptionsecret")]
    pub secretbox_encryption_secret: String,
}

/// Trustd authentication info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustdInfo {
    /// Trustd token in format "abcdef.0123456789abcdef"
    pub token: String,
}

/// Certificate authorities and keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificates {
    /// etcd CA certificate and private key
    pub etcd: CertificateAndKey,

    /// Kubernetes CA certificate and private key
    pub k8s: CertificateAndKey,

    /// Kubernetes aggregator (front-proxy) CA certificate and private key
    #[serde(rename = "k8saggregator")]
    pub k8s_aggregator: CertificateAndKey,

    /// Kubernetes service account key (ECDSA P-256 for modern compatibility)
    #[serde(rename = "k8sserviceaccount")]
    pub k8s_service_account: ServiceAccountKey,

    /// Talos (OS) CA certificate and private key
    pub os: CertificateAndKey,
}

/// Certificate and private key pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateAndKey {
    /// Base64-encoded PEM certificate
    pub crt: String,

    /// Base64-encoded PEM private key
    pub key: String,
}

/// Service account key (private key only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountKey {
    /// Base64-encoded PEM private key
    pub key: String,
}

impl SecretsBundle {
    /// Generate a new secrets bundle with all required cryptographic material
    pub fn generate() -> Result<Self> {
        let cluster = ClusterSecrets::generate()?;
        let secrets = KubernetesSecrets::generate()?;
        let trustd_info = TrustdInfo::generate()?;
        let certs = Certificates::generate()?;

        Ok(Self {
            cluster,
            secrets,
            trustd_info,
            certs,
        })
    }

    /// Save the secrets bundle to a YAML file
    pub async fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(self)?;
        tokio::fs::write(path, yaml)
            .await
            .with_context(|| format!("Failed to write secrets to {}", path.display()))?;
        Ok(())
    }

    /// Load a secrets bundle from a YAML file
    #[allow(dead_code)]
    pub async fn load(path: &Path) -> Result<Self> {
        let yaml = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read secrets from {}", path.display()))?;
        let bundle: Self = serde_yaml::from_str(&yaml)?;
        Ok(bundle)
    }
}

impl ClusterSecrets {
    fn generate() -> Result<Self> {
        Ok(Self {
            id: generate_random_base64(32)?,
            secret: generate_random_base64(32)?,
        })
    }
}

impl KubernetesSecrets {
    fn generate() -> Result<Self> {
        Ok(Self {
            bootstrap_token: generate_token(6, 16)?,
            secretbox_encryption_secret: generate_random_base64(32)?,
        })
    }
}

impl TrustdInfo {
    fn generate() -> Result<Self> {
        Ok(Self {
            token: generate_token(6, 16)?,
        })
    }
}

impl Certificates {
    fn generate() -> Result<Self> {
        // Generate ECDSA P-256 CAs for Kubernetes components
        let etcd = generate_ca("etcd", "etcd")?;
        let k8s = generate_ca("kubernetes", "kubernetes")?;
        let k8s_aggregator = generate_ca("", "front-proxy")?; // No organization for aggregator

        // Generate Ed25519 CA for Talos OS (required by Talos)
        // Note: talosctl only sets Organization, not CommonName
        let os = generate_ed25519_ca("talos", "")?;

        // Generate RSA 4096 service account key to match talosctl behavior
        let k8s_service_account = generate_rsa_service_account_key()?;

        Ok(Self {
            etcd,
            k8s,
            k8s_aggregator,
            k8s_service_account,
            os,
        })
    }
}

// Helper functions for cryptographic operations

/// Generate a random base64-encoded string of the specified byte length
fn generate_random_base64(byte_length: usize) -> Result<String> {
    let mut bytes = vec![0u8; byte_length];
    rand::thread_rng().fill(&mut bytes[..]);
    Ok(STANDARD.encode(&bytes))
}

/// Generate a token in the format "abc123.def456ghi789" (like kubeadm tokens)
///
/// This matches the token format used by Kubernetes bootstrap tokens and Talos trustd.
fn generate_token(len_first: usize, len_second: usize) -> Result<String> {
    const VALID_CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    let mut rng = rand::thread_rng();
    let mut token = String::with_capacity(len_first + 1 + len_second);

    // Generate first part
    for _ in 0..len_first {
        let idx = rng.gen_range(0..VALID_CHARS.len());
        token.push(VALID_CHARS[idx] as char);
    }

    token.push('.');

    // Generate second part
    for _ in 0..len_second {
        let idx = rng.gen_range(0..VALID_CHARS.len());
        token.push(VALID_CHARS[idx] as char);
    }

    Ok(token)
}

/// Generate a self-signed CA certificate
///
/// This creates an ECDSA P-256 CA certificate with appropriate key usage flags
/// for a certificate authority. The certificate is valid for 10 years.
fn generate_ca(organization: &str, common_name: &str) -> Result<CertificateAndKey> {
    let mut params = CertificateParams::default();

    // Set validity period to 10 years (87600 hours)
    let now = OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + Duration::hours(87600);

    // Set distinguished name
    let mut dn = DistinguishedName::new();
    if !organization.is_empty() {
        dn.push(DnType::OrganizationName, organization);
    }
    if !common_name.is_empty() {
        dn.push(DnType::CommonName, common_name);
    }
    params.distinguished_name = dn;

    // Set CA key usage
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];

    // Generate ECDSA key pair (P-256)
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;

    // Sign the certificate with its own key (self-signed)
    let cert = params.self_signed(&key_pair)?;

    // Encode certificate and key as base64-encoded PEM
    let crt_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    Ok(CertificateAndKey {
        crt: STANDARD.encode(crt_pem.as_bytes()),
        key: STANDARD.encode(key_pem.as_bytes()),
    })
}

/// Generate an Ed25519 self-signed CA certificate
///
/// This creates an Ed25519 CA certificate with appropriate key usage flags.
/// The certificate is valid for 10 years. Talos OS CA uses Ed25519.
fn generate_ed25519_ca(organization: &str, common_name: &str) -> Result<CertificateAndKey> {
    let mut params = CertificateParams::default();

    // Set validity period to 10 years (87600 hours)
    let now = OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + Duration::hours(87600);

    // Set distinguished name
    let mut dn = DistinguishedName::new();
    if !organization.is_empty() {
        dn.push(DnType::OrganizationName, organization);
    }
    if !common_name.is_empty() {
        dn.push(DnType::CommonName, common_name);
    }
    params.distinguished_name = dn;

    // Set CA key usage - Talos OS CA has DigitalSignature and KeyCertSign
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];

    // Extended key usage - server and client auth
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];

    // Generate Ed25519 key pair
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;

    // Sign the certificate with its own key (self-signed)
    let cert = params.self_signed(&key_pair)?;

    // Encode certificate and key as base64-encoded PEM
    let crt_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    Ok(CertificateAndKey {
        crt: STANDARD.encode(crt_pem.as_bytes()),
        key: STANDARD.encode(key_pem.as_bytes()),
    })
}

/// Generate an ECDSA P-256 private key for Kubernetes service account signing
///
/// Modern Kubernetes supports ECDSA keys for service account token signing.
/// This generates an ECDSA P-256 key which is more efficient than RSA and
/// fully supported by Kubernetes 1.20+.
#[allow(dead_code)]
fn generate_service_account_key() -> Result<ServiceAccountKey> {
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let key_pem = key_pair.serialize_pem();

    Ok(ServiceAccountKey {
        key: STANDARD.encode(key_pem.as_bytes()),
    })
}

/// Generate an RSA 4096 private key for Kubernetes service account signing
///
/// This generates an RSA 4096-bit key to match talosctl's default behavior.
/// While ECDSA is more efficient, talosctl uses RSA for backwards compatibility.
fn generate_rsa_service_account_key() -> Result<ServiceAccountKey> {
    let mut rng = rand::thread_rng();
    let bits = 4096;
    let private_key =
        RsaPrivateKey::new(&mut rng, bits).context("Failed to generate RSA private key")?;

    // Encode as PKCS#8 PEM
    let pem = private_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .context("Failed to encode RSA key as PEM")?;

    Ok(ServiceAccountKey {
        key: STANDARD.encode(pem.as_bytes()),
    })
}

/// Generate an admin client certificate signed by the Talos OS CA
///
/// This certificate is used by the talosconfig for authenticating to Talos nodes.
fn generate_admin_client_cert(os_ca: &CertificateAndKey) -> Result<CertificateAndKey> {
    // Decode the CA key
    let ca_key_pem = STANDARD.decode(&os_ca.key)?;
    let ca_key_pem_str = String::from_utf8(ca_key_pem)?;
    let ca_key_pair = KeyPair::from_pem(&ca_key_pem_str)?;

    // Recreate the CA certificate params for issuer
    // These must match exactly what generate_ed25519_ca() creates
    let now = OffsetDateTime::now_utc();
    let mut ca_params = CertificateParams::default();
    ca_params.not_before = now;
    ca_params.not_after = now + Duration::hours(87600); // 10 years
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::OrganizationName, "talos");
    // Note: no CommonName, matching generate_ed25519_ca("talos", "")
    ca_params.distinguished_name = ca_dn;
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    ca_params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];

    // Create issuer from CA params and key pair
    let ca_issuer = Issuer::new(ca_params, ca_key_pair);

    // Create client certificate parameters
    let mut client_params = CertificateParams::default();
    client_params.not_before = now;
    client_params.not_after = now + Duration::hours(8760); // 1 year

    // Set distinguished name with organization "os:admin"
    let mut dn = DistinguishedName::new();
    dn.push(DnType::OrganizationName, "os:admin");
    client_params.distinguished_name = dn;

    // Set client auth key usage - only DigitalSignature (matches talosctl)
    client_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    client_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];

    // Enable Authority Key Identifier extension (required for cert chain validation)
    client_params.use_authority_key_identifier_extension = true;

    // Generate Ed25519 key pair for the client
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;

    // Sign the certificate with the CA issuer
    let cert = client_params.signed_by(&key_pair, &ca_issuer)?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    Ok(CertificateAndKey {
        crt: STANDARD.encode(cert_pem.as_bytes()),
        key: STANDARD.encode(key_pem.as_bytes()),
    })
}

// Machine configuration structures

/// Output structure containing all generated machine configurations
#[derive(Debug, Clone)]
pub struct MachineConfigOutput {
    pub controlplane_yaml: String,

    pub worker_yaml: String,

    pub talosconfig_yaml: String,
}

/// Top-level Talos machine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosMachineConfig {
    version: String,

    debug: bool,

    persist: bool,

    machine: MachineSection,

    cluster: ClusterSection,
}

/// Machine-specific configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MachineSection {
    #[serde(rename = "type")]
    machine_type: String,

    token: String,

    ca: CertificateAndKey,

    #[serde(rename = "certSANs", skip_serializing_if = "Option::is_none")]
    cert_sans: Option<Vec<String>>,

    kubelet: KubeletConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<HashMap<String, serde_json::Value>>,

    install: InstallConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    registries: Option<HashMap<String, serde_json::Value>>,

    features: FeaturesConfig,

    #[serde(rename = "nodeLabels", skip_serializing_if = "Option::is_none")]
    node_labels: Option<HashMap<String, String>>,
}

/// Kubelet configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubeletConfig {
    image: String,

    #[serde(rename = "defaultRuntimeSeccompProfileEnabled")]
    default_runtime_seccomp_profile_enabled: bool,

    #[serde(rename = "disableManifestsDirectory")]
    disable_manifests_directory: bool,
}

/// Installation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallConfig {
    disk: String,

    image: String,

    wipe: bool,

    #[serde(rename = "grubUseUKICmdline")]
    grub_use_uki_cmdline: bool,
}

/// Features configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FeaturesConfig {
    #[serde(rename = "diskQuotaSupport")]
    disk_quota_support: bool,

    #[serde(rename = "kubePrism")]
    kube_prism: KubePrismConfig,

    #[serde(rename = "hostDNS")]
    host_dns: HostDNSConfig,
}

/// KubePrism configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubePrismConfig {
    enabled: bool,

    port: u16,
}

/// Host DNS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostDNSConfig {
    enabled: bool,

    #[serde(rename = "forwardKubeDNSToHost")]
    forward_kube_dns_to_host: bool,
}

/// Cluster-specific configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClusterSection {
    id: String,

    secret: String,

    #[serde(rename = "controlPlane", skip_serializing_if = "Option::is_none")]
    control_plane: Option<ControlPlaneConfig>,

    #[serde(rename = "clusterName")]
    cluster_name: String,

    network: ClusterNetworkConfig,

    token: String,

    #[serde(
        rename = "secretboxEncryptionSecret",
        skip_serializing_if = "Option::is_none"
    )]
    secretbox_encryption_secret: Option<String>,

    ca: CertificateAndKey,

    #[serde(rename = "aggregatorCA", skip_serializing_if = "Option::is_none")]
    aggregator_ca: Option<CertificateAndKey>,

    #[serde(rename = "serviceAccount", skip_serializing_if = "Option::is_none")]
    service_account: Option<ServiceAccountKey>,

    #[serde(rename = "apiServer", skip_serializing_if = "Option::is_none")]
    api_server: Option<ApiServerConfig>,

    #[serde(rename = "controllerManager", skip_serializing_if = "Option::is_none")]
    controller_manager: Option<ControllerManagerConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<ProxyConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler: Option<SchedulerConfig>,

    discovery: DiscoveryConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    etcd: Option<EtcdConfig>,

    #[serde(rename = "extraManifests")]
    extra_manifests: Vec<String>,

    #[serde(rename = "inlineManifests")]
    inline_manifests: Vec<serde_json::Value>,
}

/// Control plane endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlPlaneConfig {
    endpoint: String,
}

/// Cluster network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClusterNetworkConfig {
    #[serde(rename = "dnsDomain")]
    dns_domain: String,

    #[serde(rename = "podSubnets")]
    pod_subnets: Vec<String>,

    #[serde(rename = "serviceSubnets")]
    service_subnets: Vec<String>,
}

/// API server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiServerConfig {
    image: String,

    #[serde(rename = "certSANs", skip_serializing_if = "Option::is_none")]
    cert_sans: Option<Vec<String>>,

    #[serde(rename = "admissionControl", skip_serializing_if = "Option::is_none")]
    admission_control: Option<Vec<serde_json::Value>>,

    #[serde(rename = "auditPolicy", skip_serializing_if = "Option::is_none")]
    audit_policy: Option<serde_json::Value>,
}

/// Controller manager configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControllerManagerConfig {
    image: String,
}

/// Proxy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProxyConfig {
    image: String,
}

/// Scheduler configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchedulerConfig {
    image: String,
}

/// Discovery configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryConfig {
    enabled: bool,

    registries: DiscoveryRegistriesConfig,
}

/// Discovery registries configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryRegistriesConfig {
    kubernetes: KubernetesRegistryConfig,

    service: HashMap<String, serde_json::Value>,
}

/// Kubernetes registry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubernetesRegistryConfig {
    disabled: bool,
}

/// Etcd configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EtcdConfig {
    ca: CertificateAndKey,
}

/// Hostname configuration document
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostnameConfig {
    #[serde(rename = "apiVersion")]
    api_version: String,

    kind: String,

    auto: String,
}

/// Talos client configuration (talosconfig)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosClientConfig {
    context: String,

    contexts: HashMap<String, TalosContext>,
}

/// Talos context configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosContext {
    endpoints: Vec<String>,

    ca: String,

    crt: String,

    key: String,
}

/// Generate machine configurations for Talos cluster
///
/// This function generates controlplane, worker, and talosconfig YAML files
/// based on the provided secrets bundle and cluster parameters.
pub fn generate_machine_configs(
    secrets: &SecretsBundle,
    cluster_name: &str,
    endpoint_ip: &str,
    install_disk: &str,
    additional_sans: &[String],
    talos_version: &str,
) -> Result<MachineConfigOutput> {
    // Generate admin client certificate for talosconfig
    let admin_cert = generate_admin_client_cert(&secrets.certs.os)?;

    // Build certificate SANs list
    let mut cert_sans = vec![endpoint_ip.to_string()];
    cert_sans.extend(additional_sans.iter().cloned());

    // Generate controlplane configuration
    let controlplane_config = generate_controlplane_config(
        secrets,
        cluster_name,
        endpoint_ip,
        install_disk,
        &cert_sans,
        talos_version,
    )?;

    // Generate worker configuration
    let worker_config = generate_worker_config(
        secrets,
        cluster_name,
        endpoint_ip,
        install_disk,
        &cert_sans,
        talos_version,
    )?;

    // Generate talosconfig
    let talosconfig =
        generate_talosconfig(cluster_name, endpoint_ip, &secrets.certs.os, &admin_cert)?;

    // Serialize to YAML with hostname config appended
    let hostname_config = HostnameConfig {
        api_version: "v1alpha1".to_string(),
        kind: "HostnameConfig".to_string(),
        auto: "stable".to_string(),
    };

    let controlplane_yaml = format!(
        "{}\n---\n{}",
        serde_yaml::to_string(&controlplane_config)?,
        serde_yaml::to_string(&hostname_config)?
    );

    let worker_yaml = format!(
        "{}\n---\n{}",
        serde_yaml::to_string(&worker_config)?,
        serde_yaml::to_string(&hostname_config)?
    );

    let talosconfig_yaml = serde_yaml::to_string(&talosconfig)?;

    Ok(MachineConfigOutput {
        controlplane_yaml,
        worker_yaml,
        talosconfig_yaml,
    })
}

/// Generate controlplane machine configuration
fn generate_controlplane_config(
    secrets: &SecretsBundle,
    cluster_name: &str,
    endpoint_ip: &str,
    install_disk: &str,
    cert_sans: &[String],
    talos_version: &str,
) -> Result<TalosMachineConfig> {
    let machine = MachineSection {
        machine_type: "controlplane".to_string(),
        token: secrets.trustd_info.token.clone(),
        ca: secrets.certs.os.clone(),
        cert_sans: Some(cert_sans.to_vec()),
        kubelet: KubeletConfig {
            image: format!("ghcr.io/siderolabs/kubelet:{}", KUBERNETES_VERSION),
            default_runtime_seccomp_profile_enabled: true,
            disable_manifests_directory: true,
        },
        network: Some(HashMap::new()),
        install: InstallConfig {
            disk: install_disk.to_string(),
            image: format!("ghcr.io/siderolabs/installer:{}", talos_version),
            wipe: false,
            grub_use_uki_cmdline: true,
        },
        registries: None,
        features: FeaturesConfig {
            disk_quota_support: true,
            kube_prism: KubePrismConfig {
                enabled: true,
                port: 7445,
            },
            host_dns: HostDNSConfig {
                enabled: true,
                forward_kube_dns_to_host: true,
            },
        },
        node_labels: Some({
            let mut labels = HashMap::new();
            labels.insert(
                "node.kubernetes.io/exclude-from-external-load-balancers".to_string(),
                "".to_string(),
            );
            labels
        }),
    };

    let cluster = ClusterSection {
        id: secrets.cluster.id.clone(),
        secret: secrets.cluster.secret.clone(),
        control_plane: Some(ControlPlaneConfig {
            endpoint: format!("https://{}:6443", endpoint_ip),
        }),
        cluster_name: cluster_name.to_string(),
        network: ClusterNetworkConfig {
            dns_domain: "cluster.local".to_string(),
            pod_subnets: vec!["10.244.0.0/16".to_string()],
            service_subnets: vec!["10.96.0.0/12".to_string()],
        },
        token: secrets.secrets.bootstrap_token.clone(),
        secretbox_encryption_secret: Some(secrets.secrets.secretbox_encryption_secret.clone()),
        ca: secrets.certs.k8s.clone(),
        aggregator_ca: Some(secrets.certs.k8s_aggregator.clone()),
        service_account: Some(secrets.certs.k8s_service_account.clone()),
        api_server: Some(ApiServerConfig {
            image: format!("registry.k8s.io/kube-apiserver:{}", KUBERNETES_VERSION),
            cert_sans: Some(cert_sans.to_vec()),
            admission_control: Some(vec![serde_json::json!({
                "name": "PodSecurity",
                "configuration": {
                    "apiVersion": "pod-security.admission.config.k8s.io/v1alpha1",
                    "kind": "PodSecurityConfiguration",
                    "defaults": {
                        "enforce": "baseline",
                        "enforce-version": "latest",
                        "audit": "restricted",
                        "audit-version": "latest",
                        "warn": "restricted",
                        "warn-version": "latest"
                    },
                    "exemptions": {
                        "namespaces": ["kube-system"],
                        "runtimeClasses": [],
                        "usernames": []
                    }
                }
            })]),
            audit_policy: Some(serde_json::json!({
                "apiVersion": "audit.k8s.io/v1",
                "kind": "Policy",
                "rules": [
                    {"level": "Metadata"}
                ]
            })),
        }),
        controller_manager: Some(ControllerManagerConfig {
            image: format!(
                "registry.k8s.io/kube-controller-manager:{}",
                KUBERNETES_VERSION
            ),
        }),
        proxy: Some(ProxyConfig {
            image: format!("registry.k8s.io/kube-proxy:{}", KUBERNETES_VERSION),
        }),
        scheduler: Some(SchedulerConfig {
            image: format!("registry.k8s.io/kube-scheduler:{}", KUBERNETES_VERSION),
        }),
        discovery: DiscoveryConfig {
            enabled: true,
            registries: DiscoveryRegistriesConfig {
                kubernetes: KubernetesRegistryConfig { disabled: true },
                service: HashMap::new(),
            },
        },
        etcd: Some(EtcdConfig {
            ca: secrets.certs.etcd.clone(),
        }),
        extra_manifests: Vec::new(),
        inline_manifests: Vec::new(),
    };

    Ok(TalosMachineConfig {
        version: "v1alpha1".to_string(),
        debug: false,
        persist: true,
        machine,
        cluster,
    })
}

/// Generate worker machine configuration
fn generate_worker_config(
    secrets: &SecretsBundle,
    cluster_name: &str,
    endpoint_ip: &str,
    install_disk: &str,
    cert_sans: &[String],
    talos_version: &str,
) -> Result<TalosMachineConfig> {
    // Worker config is similar to controlplane but with key differences
    let machine = MachineSection {
        machine_type: "worker".to_string(),
        token: secrets.trustd_info.token.clone(),
        ca: CertificateAndKey {
            crt: secrets.certs.os.crt.clone(),
            key: "".to_string(), // Empty key for workers
        },
        cert_sans: Some(cert_sans.to_vec()),
        kubelet: KubeletConfig {
            image: format!("ghcr.io/siderolabs/kubelet:{}", KUBERNETES_VERSION),
            default_runtime_seccomp_profile_enabled: true,
            disable_manifests_directory: true,
        },
        network: Some(HashMap::new()),
        install: InstallConfig {
            disk: install_disk.to_string(),
            image: format!("ghcr.io/siderolabs/installer:{}", talos_version),
            wipe: false,
            grub_use_uki_cmdline: true,
        },
        registries: Some(HashMap::new()),
        features: FeaturesConfig {
            disk_quota_support: true,
            kube_prism: KubePrismConfig {
                enabled: true,
                port: 7445,
            },
            host_dns: HostDNSConfig {
                enabled: true,
                forward_kube_dns_to_host: true,
            },
        },
        node_labels: None,
    };

    let cluster = ClusterSection {
        id: secrets.cluster.id.clone(),
        secret: secrets.cluster.secret.clone(),
        control_plane: Some(ControlPlaneConfig {
            endpoint: format!("https://{}:6443", endpoint_ip),
        }),
        cluster_name: cluster_name.to_string(),
        network: ClusterNetworkConfig {
            dns_domain: "cluster.local".to_string(),
            pod_subnets: vec!["10.244.0.0/16".to_string()],
            service_subnets: vec!["10.96.0.0/12".to_string()],
        },
        token: secrets.secrets.bootstrap_token.clone(),
        secretbox_encryption_secret: None, // Workers don't have this
        ca: CertificateAndKey {
            crt: secrets.certs.k8s.crt.clone(),
            key: "".to_string(), // Empty key for workers
        },
        aggregator_ca: None, // Workers don't have these sections
        service_account: None,
        api_server: None,
        controller_manager: None,
        proxy: None,
        scheduler: None,
        discovery: DiscoveryConfig {
            enabled: true,
            registries: DiscoveryRegistriesConfig {
                kubernetes: KubernetesRegistryConfig { disabled: true },
                service: HashMap::new(),
            },
        },
        etcd: None, // Workers don't have etcd
        extra_manifests: Vec::new(),
        inline_manifests: Vec::new(),
    };

    Ok(TalosMachineConfig {
        version: "v1alpha1".to_string(),
        debug: false,
        persist: true,
        machine,
        cluster,
    })
}

/// Generate talosconfig (client configuration)
fn generate_talosconfig(
    cluster_name: &str,
    endpoint_ip: &str,
    os_ca: &CertificateAndKey,
    admin_cert: &CertificateAndKey,
) -> Result<TalosClientConfig> {
    let context = TalosContext {
        endpoints: vec![endpoint_ip.to_string()],
        ca: os_ca.crt.clone(),
        crt: admin_cert.crt.clone(),
        key: admin_cert.key.clone(),
    };

    let mut contexts = HashMap::new();
    contexts.insert(cluster_name.to_string(), context);

    Ok(TalosClientConfig {
        context: cluster_name.to_string(),
        contexts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token() {
        let token = generate_token(6, 16).expect("Failed to generate token");
        assert_eq!(token.len(), 6 + 1 + 16); // "abc123.def456ghi789" format
        assert!(token.contains('.'));

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 6);
        assert_eq!(parts[1].len(), 16);

        // Verify only valid characters
        for c in token.chars() {
            if c != '.' {
                assert!(c.is_ascii_lowercase() || c.is_ascii_digit());
            }
        }
    }

    #[test]
    fn test_generate_random_base64() {
        let b64 = generate_random_base64(32).expect("Failed to generate random base64");

        // Base64 encoding of 32 bytes should be 44 characters (with padding)
        assert!(!b64.is_empty());

        // Verify it decodes properly
        let decoded = STANDARD.decode(&b64).expect("Failed to decode base64");
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn test_generate_secrets_bundle() {
        let bundle = SecretsBundle::generate().expect("Failed to generate secrets bundle");

        // Verify cluster secrets
        assert!(!bundle.cluster.id.is_empty());
        assert!(!bundle.cluster.secret.is_empty());

        // Verify kubernetes secrets
        assert!(bundle.secrets.bootstrap_token.contains('.'));
        assert!(!bundle.secrets.secretbox_encryption_secret.is_empty());

        // Verify trustd info
        assert!(bundle.trustd_info.token.contains('.'));

        // Verify all certificates are present
        assert!(!bundle.certs.etcd.crt.is_empty());
        assert!(!bundle.certs.etcd.key.is_empty());
        assert!(!bundle.certs.k8s.crt.is_empty());
        assert!(!bundle.certs.k8s.key.is_empty());
        assert!(!bundle.certs.k8s_aggregator.crt.is_empty());
        assert!(!bundle.certs.k8s_aggregator.key.is_empty());
        assert!(!bundle.certs.k8s_service_account.key.is_empty());
        assert!(!bundle.certs.os.crt.is_empty());
        assert!(!bundle.certs.os.key.is_empty());
    }

    #[test]
    fn test_secrets_bundle_serialization() {
        let bundle = SecretsBundle::generate().expect("Failed to generate secrets bundle");

        // Serialize to YAML
        let yaml = serde_yaml::to_string(&bundle).expect("Failed to serialize to YAML");
        assert!(yaml.contains("cluster:"));
        assert!(yaml.contains("secrets:"));
        assert!(yaml.contains("trustdinfo:"));
        assert!(yaml.contains("certs:"));

        // Deserialize back
        let deserialized: SecretsBundle =
            serde_yaml::from_str(&yaml).expect("Failed to deserialize from YAML");

        assert_eq!(bundle.cluster.id, deserialized.cluster.id);
        assert_eq!(bundle.cluster.secret, deserialized.cluster.secret);
        assert_eq!(
            bundle.secrets.bootstrap_token,
            deserialized.secrets.bootstrap_token
        );
    }

    #[test]
    fn test_certificate_generation() {
        let cert = generate_ca("test-org", "test-ca").expect("Failed to generate CA");

        // Verify base64-encoded PEM is valid
        let crt_pem = STANDARD.decode(&cert.crt).expect("Failed to decode cert");
        let key_pem = STANDARD.decode(&cert.key).expect("Failed to decode key");

        let crt_str = String::from_utf8(crt_pem).expect("Cert PEM not UTF-8");
        let key_str = String::from_utf8(key_pem).expect("Key PEM not UTF-8");

        assert!(crt_str.contains("-----BEGIN CERTIFICATE-----"));
        assert!(crt_str.contains("-----END CERTIFICATE-----"));
        assert!(key_str.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(key_str.contains("-----END PRIVATE KEY-----"));
    }

    #[test]
    fn test_service_account_key_generation() {
        let key = generate_service_account_key().expect("Failed to generate service account key");

        // Verify base64-encoded PEM is valid
        let key_pem = STANDARD.decode(&key.key).expect("Failed to decode key");
        let key_str = String::from_utf8(key_pem).expect("Key PEM not UTF-8");

        assert!(key_str.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(key_str.contains("-----END PRIVATE KEY-----"));
    }
}
