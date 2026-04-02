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
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose};
use serde::{Deserialize, Serialize};
use std::path::Path;
use time::{Duration, OffsetDateTime};

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
        // Generate all CAs with 10-year validity
        let etcd = generate_ca("etcd", "etcd")?;
        let k8s = generate_ca("kubernetes", "kubernetes")?;
        let k8s_aggregator = generate_ca("", "front-proxy")?; // No organization for aggregator
        let os = generate_ca("talos", "talos")?;

        // Generate ECDSA service account key.
        // Modern Kubernetes supports ECDSA keys for service account tokens.
        let k8s_service_account = generate_service_account_key()?;

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

/// Generate an ECDSA P-256 private key for Kubernetes service account signing
///
/// Modern Kubernetes supports ECDSA keys for service account token signing.
/// This generates an ECDSA P-256 key which is more efficient than RSA and
/// fully supported by Kubernetes 1.20+.
fn generate_service_account_key() -> Result<ServiceAccountKey> {
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let key_pem = key_pair.serialize_pem();

    Ok(ServiceAccountKey {
        key: STANDARD.encode(key_pem.as_bytes()),
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
