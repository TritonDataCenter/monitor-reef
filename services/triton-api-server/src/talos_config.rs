// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Native Talos cluster secret and machine-config generation.
//!
//! Generates the same output as `talosctl gen secrets` + `talosctl gen config`
//! using pure Rust crypto (rcgen, rsa). No external binary required.

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand_core::{OsRng, RngCore};
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::{Duration, OffsetDateTime};

const KUBERNETES_VERSION: &str = "v1.35.0";
pub const DEFAULT_TALOS_VERSION: &str = "v1.12.7";
pub const DEFAULT_INSTALL_DISK: &str = "/dev/sda";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsBundle {
    pub cluster: ClusterSecrets,
    pub secrets: KubernetesSecrets,
    #[serde(rename = "trustdinfo")]
    pub trustd_info: TrustdInfo,
    pub certs: Certificates,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSecrets {
    pub id: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubernetesSecrets {
    #[serde(rename = "bootstraptoken")]
    pub bootstrap_token: String,
    #[serde(rename = "secretboxencryptionsecret")]
    pub secretbox_encryption_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustdInfo {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificates {
    pub etcd: CertificateAndKey,
    pub k8s: CertificateAndKey,
    #[serde(rename = "k8saggregator")]
    pub k8s_aggregator: CertificateAndKey,
    #[serde(rename = "k8sserviceaccount")]
    pub k8s_service_account: ServiceAccountKey,
    pub os: CertificateAndKey,
}

/// Certificate and private key pair — values are base64-encoded PEM strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateAndKey {
    pub crt: String,
    pub key: String,
}

/// Service account key (private key only) — value is a base64-encoded PEM string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountKey {
    pub key: String,
}

impl SecretsBundle {
    pub fn generate() -> Result<Self> {
        Ok(Self {
            cluster: ClusterSecrets {
                id: generate_random_base64(32)?,
                secret: generate_random_base64(32)?,
            },
            secrets: KubernetesSecrets {
                bootstrap_token: generate_token(6, 16)?,
                secretbox_encryption_secret: generate_random_base64(32)?,
            },
            trustd_info: TrustdInfo {
                token: generate_token(6, 16)?,
            },
            certs: Certificates {
                etcd: generate_ca("etcd", "etcd")?,
                k8s: generate_ca("kubernetes", "kubernetes")?,
                k8s_aggregator: generate_ca("", "front-proxy")?,
                k8s_service_account: generate_rsa_service_account_key()?,
                os: generate_ed25519_ca("talos", "")?,
            },
        })
    }
}

/// Output of [`generate_machine_configs`].
#[derive(Debug, Clone)]
pub struct MachineConfigOutput {
    pub controlplane_yaml: String,
    pub worker_yaml: String,
    pub talosconfig_yaml: String,
    /// Raw PEM bytes for the OS CA certificate (for mTLS verification).
    pub ca_pem_raw: Vec<u8>,
    /// Raw PEM bytes for the admin operator client certificate.
    pub crt_pem_raw: Vec<u8>,
    /// Raw PEM bytes for the admin operator client private key.
    pub key_pem_raw: Vec<u8>,
}

/// Generate control-plane YAML, worker YAML, and talosconfig for a new cluster.
pub fn generate_machine_configs(
    secrets: &SecretsBundle,
    cluster_name: &str,
    endpoint_ip: &str,
    install_disk: &str,
    talos_version: &str,
) -> Result<MachineConfigOutput> {
    let admin_cert = generate_admin_client_cert(&secrets.certs.os)?;

    let cert_sans = vec![endpoint_ip.to_string()];

    let cp_config = generate_controlplane_config(
        secrets,
        cluster_name,
        endpoint_ip,
        install_disk,
        &cert_sans,
        talos_version,
    )?;
    let worker_config = generate_worker_config(
        secrets,
        cluster_name,
        endpoint_ip,
        install_disk,
        &cert_sans,
        talos_version,
    )?;
    let talosconfig =
        generate_talosconfig(cluster_name, endpoint_ip, &secrets.certs.os, &admin_cert)?;

    let hostname_suffix = "---\napiVersion: v1alpha1\nkind: HostnameConfig\nauto: stable\n";

    let controlplane_yaml = format!(
        "{}\n{}",
        serde_yaml::to_string(&cp_config)?,
        hostname_suffix
    );
    let worker_yaml = format!(
        "{}\n{}",
        serde_yaml::to_string(&worker_config)?,
        hostname_suffix
    );
    let talosconfig_yaml = serde_yaml::to_string(&talosconfig)?;

    let ca_pem_raw = STANDARD
        .decode(&secrets.certs.os.crt)
        .context("decode OS CA cert")?;
    let crt_pem_raw = STANDARD
        .decode(&admin_cert.crt)
        .context("decode admin cert")?;
    let key_pem_raw = STANDARD
        .decode(&admin_cert.key)
        .context("decode admin key")?;

    Ok(MachineConfigOutput {
        controlplane_yaml,
        worker_yaml,
        talosconfig_yaml,
        ca_pem_raw,
        crt_pem_raw,
        key_pem_raw,
    })
}

/// Generate an admin client cert signed by the OS CA (for talosconfig / mTLS).
pub fn generate_admin_client_cert(os_ca: &CertificateAndKey) -> Result<CertificateAndKey> {
    let ca_key_pem = STANDARD.decode(&os_ca.key)?;
    let ca_key_pem_str = String::from_utf8(ca_key_pem)?;
    let ca_key_pair = KeyPair::from_pem(&ca_key_pem_str)?;

    let now = OffsetDateTime::now_utc();
    let mut ca_params = CertificateParams::default();
    ca_params.not_before = now;
    ca_params.not_after = now + Duration::hours(87600);
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::OrganizationName, "talos");
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
    let ca_issuer = Issuer::new(ca_params, ca_key_pair);

    let mut client_params = CertificateParams::default();
    client_params.not_before = now;
    client_params.not_after = now + Duration::hours(8760);
    let mut dn = DistinguishedName::new();
    dn.push(DnType::OrganizationName, "os:admin");
    client_params.distinguished_name = dn;
    client_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    client_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    client_params.use_authority_key_identifier_extension = true;

    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let cert = client_params.signed_by(&key_pair, &ca_issuer)?;

    Ok(CertificateAndKey {
        crt: STANDARD.encode(cert.pem().as_bytes()),
        key: STANDARD.encode(key_pair.serialize_pem().as_bytes()),
    })
}

// ---------------------------------------------------------------------------
// Machine config structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosMachineConfig {
    version: String,
    debug: bool,
    persist: bool,
    machine: MachineSection,
    cluster: ClusterSection,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubeletConfig {
    image: String,
    #[serde(rename = "defaultRuntimeSeccompProfileEnabled")]
    default_runtime_seccomp_profile_enabled: bool,
    #[serde(rename = "disableManifestsDirectory")]
    disable_manifests_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallConfig {
    disk: String,
    image: String,
    wipe: bool,
    #[serde(rename = "grubUseUKICmdline")]
    grub_use_uki_cmdline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FeaturesConfig {
    #[serde(rename = "diskQuotaSupport")]
    disk_quota_support: bool,
    #[serde(rename = "kubePrism")]
    kube_prism: KubePrismConfig,
    #[serde(rename = "hostDNS")]
    host_dns: HostDNSConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubePrismConfig {
    enabled: bool,
    port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostDNSConfig {
    enabled: bool,
    #[serde(rename = "forwardKubeDNSToHost")]
    forward_kube_dns_to_host: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClusterSection {
    id: String,
    secret: String,
    #[serde(rename = "controlPlane", skip_serializing_if = "Option::is_none")]
    control_plane: Option<ControlPlaneEndpoint>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlPlaneEndpoint {
    endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClusterNetworkConfig {
    #[serde(rename = "dnsDomain")]
    dns_domain: String,
    #[serde(rename = "podSubnets")]
    pod_subnets: Vec<String>,
    #[serde(rename = "serviceSubnets")]
    service_subnets: Vec<String>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControllerManagerConfig {
    image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProxyConfig {
    image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchedulerConfig {
    image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryConfig {
    enabled: bool,
    registries: DiscoveryRegistriesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryRegistriesConfig {
    kubernetes: KubernetesRegistryConfig,
    service: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KubernetesRegistryConfig {
    disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EtcdConfig {
    ca: CertificateAndKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosClientConfig {
    context: String,
    contexts: HashMap<String, TalosContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TalosContext {
    endpoints: Vec<String>,
    ca: String,
    crt: String,
    key: String,
}

// ---------------------------------------------------------------------------
// Config generation helpers
// ---------------------------------------------------------------------------

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
            let mut m = HashMap::new();
            m.insert(
                "node.kubernetes.io/exclude-from-external-load-balancers".to_string(),
                "".to_string(),
            );
            m
        }),
    };

    let cluster = ClusterSection {
        id: secrets.cluster.id.clone(),
        secret: secrets.cluster.secret.clone(),
        control_plane: Some(ControlPlaneEndpoint {
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
                "rules": [{"level": "Metadata"}]
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

fn generate_worker_config(
    secrets: &SecretsBundle,
    cluster_name: &str,
    endpoint_ip: &str,
    install_disk: &str,
    cert_sans: &[String],
    talos_version: &str,
) -> Result<TalosMachineConfig> {
    let machine = MachineSection {
        machine_type: "worker".to_string(),
        token: secrets.trustd_info.token.clone(),
        ca: CertificateAndKey {
            crt: secrets.certs.os.crt.clone(),
            key: "".to_string(),
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
        control_plane: Some(ControlPlaneEndpoint {
            endpoint: format!("https://{}:6443", endpoint_ip),
        }),
        cluster_name: cluster_name.to_string(),
        network: ClusterNetworkConfig {
            dns_domain: "cluster.local".to_string(),
            pod_subnets: vec!["10.244.0.0/16".to_string()],
            service_subnets: vec!["10.96.0.0/12".to_string()],
        },
        token: secrets.secrets.bootstrap_token.clone(),
        secretbox_encryption_secret: None,
        ca: CertificateAndKey {
            crt: secrets.certs.k8s.crt.clone(),
            key: "".to_string(),
        },
        aggregator_ca: None,
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
        etcd: None,
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

fn generate_talosconfig(
    cluster_name: &str,
    endpoint_ip: &str,
    os_ca: &CertificateAndKey,
    admin_cert: &CertificateAndKey,
) -> Result<TalosClientConfig> {
    let mut contexts = HashMap::new();
    contexts.insert(
        cluster_name.to_string(),
        TalosContext {
            endpoints: vec![endpoint_ip.to_string()],
            ca: os_ca.crt.clone(),
            crt: admin_cert.crt.clone(),
            key: admin_cert.key.clone(),
        },
    );
    Ok(TalosClientConfig {
        context: cluster_name.to_string(),
        contexts,
    })
}

// ---------------------------------------------------------------------------
// Crypto primitives
// ---------------------------------------------------------------------------

fn generate_random_base64(byte_length: usize) -> Result<String> {
    let mut bytes = vec![0u8; byte_length];
    OsRng.fill_bytes(&mut bytes);
    Ok(STANDARD.encode(&bytes))
}

fn generate_token(len_first: usize, len_second: usize) -> Result<String> {
    const VALID: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut tok = String::with_capacity(len_first + 1 + len_second);
    for _ in 0..len_first {
        tok.push(VALID[OsRng.next_u32() as usize % VALID.len()] as char);
    }
    tok.push('.');
    for _ in 0..len_second {
        tok.push(VALID[OsRng.next_u32() as usize % VALID.len()] as char);
    }
    Ok(tok)
}

fn generate_ca(organization: &str, common_name: &str) -> Result<CertificateAndKey> {
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.not_before = now;
    params.not_after = now + Duration::hours(87600);
    let mut dn = DistinguishedName::new();
    if !organization.is_empty() {
        dn.push(DnType::OrganizationName, organization);
    }
    if !common_name.is_empty() {
        dn.push(DnType::CommonName, common_name);
    }
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let cert = params.self_signed(&key_pair)?;
    Ok(CertificateAndKey {
        crt: STANDARD.encode(cert.pem().as_bytes()),
        key: STANDARD.encode(key_pair.serialize_pem().as_bytes()),
    })
}

fn generate_ed25519_ca(organization: &str, common_name: &str) -> Result<CertificateAndKey> {
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.not_before = now;
    params.not_after = now + Duration::hours(87600);
    let mut dn = DistinguishedName::new();
    if !organization.is_empty() {
        dn.push(DnType::OrganizationName, organization);
    }
    if !common_name.is_empty() {
        dn.push(DnType::CommonName, common_name);
    }
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let cert = params.self_signed(&key_pair)?;
    Ok(CertificateAndKey {
        crt: STANDARD.encode(cert.pem().as_bytes()),
        key: STANDARD.encode(key_pair.serialize_pem().as_bytes()),
    })
}

fn generate_rsa_service_account_key() -> Result<ServiceAccountKey> {
    let private_key = RsaPrivateKey::new(&mut OsRng, 4096).context("generate RSA key")?;
    let pem = private_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .context("encode RSA key as PEM")?;
    Ok(ServiceAccountKey {
        key: STANDARD.encode(pem.as_bytes()),
    })
}
