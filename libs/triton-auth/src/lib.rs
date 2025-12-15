// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Triton HTTP Signature Authentication Library
//!
//! This library provides SSH key-based HTTP Signature authentication for
//! Triton CloudAPI requests. It supports:
//!
//! - Loading SSH keys from files (RSA, ECDSA, Ed25519, DSA)
//! - SSH agent integration for secure key access
//! - Both MD5 and SHA256 fingerprint formats for key identification
//! - HTTP Signature generation per the CloudAPI authentication scheme
//!
//! # Fingerprint Format Support
//!
//! This library accepts fingerprints in either format (matching `ssh-keygen -l` output):
//!
//! - **MD5**: `aa:bb:cc:dd:...` or `MD5:aa:bb:cc:dd:...`
//! - **SHA256**: `SHA256:base64data` (modern default)
//!
//! The library will automatically use the appropriate hash to match keys, and always
//! uses MD5 format in the Authorization header (CloudAPI server requirement).
//!
//! # Authentication Flow
//!
//! 1. Create an [`AuthConfig`] with account details and key source
//! 2. For each HTTP request:
//!    a. Generate a Date header value
//!    b. Construct the signing string from date, method, and path
//!    c. Sign the string using the configured key
//!    d. Construct the Authorization header with keyId, algorithm, and signature
//!
//! # Example
//!
//! ```ignore
//! use triton_auth::{AuthConfig, KeySource, sign_request};
//!
//! // Configure authentication - can use either MD5 or SHA256 fingerprint
//! let config = AuthConfig::new(
//!     "myaccount",
//!     "SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4=",  // SHA256 format
//!     KeySource::auto("SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4="),
//! );
//!
//! // Or use MD5 format
//! let config = AuthConfig::new(
//!     "myaccount",
//!     "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",  // MD5 format
//!     KeySource::auto("fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"),
//! );
//!
//! // Sign a request
//! let (date_header, auth_header) = sign_request(&config, "GET", "/myaccount/machines").await?;
//! ```
//!
//! # HTTP Signature Format
//!
//! The Authorization header follows this format:
//!
//! ```text
//! Authorization: Signature keyId="/:account/keys/:md5_fingerprint",algorithm="rsa-sha256",signature=":base64:"
//! ```
//!
//! Note: The keyId always uses MD5 fingerprint format (CloudAPI server requirement).
//!
//! The signature is computed over:
//!
//! ```text
//! date: <RFC2822 date>
//! (request-target): <method lowercase> <path>
//! ```

pub mod agent;
pub mod error;
pub mod fingerprint;
pub mod key_loader;
pub mod legacy_pem;
pub mod signature;
pub mod ssh_agent;

pub use error::AuthError;
pub use fingerprint::{
    Fingerprint, format_fingerprint, md5_fingerprint, parse_fingerprint, sha256_fingerprint,
};
pub use key_loader::{KeyLoader, KeySource};
pub use legacy_pem::{LegacyPrivateKey, PemKeyFormat};
pub use signature::{KeyType, RequestSigner, encode_signature, sign_with_key};

/// Authentication configuration for CloudAPI requests
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Account login name (used in keyId path)
    pub account: String,
    /// RBAC sub-user login (optional)
    pub user: Option<String>,
    /// SSH key fingerprint (MD5 or SHA256 format)
    /// Can be: "aa:bb:cc:..." or "MD5:aa:bb:..." or "SHA256:base64..."
    pub key_id: String,
    /// How to load/access the signing key
    pub key_source: KeySource,
    /// RBAC roles to assume (optional, added as query param)
    pub roles: Option<Vec<String>>,
}

impl AuthConfig {
    /// Create a new AuthConfig
    ///
    /// # Arguments
    /// * `account` - The CloudAPI account login name
    /// * `key_id` - SSH key fingerprint (MD5 or SHA256 format)
    /// * `key_source` - How to load the signing key (file, agent, or auto)
    ///
    /// # Fingerprint Formats
    /// - MD5: `aa:bb:cc:dd:...` or `MD5:aa:bb:cc:dd:...`
    /// - SHA256: `SHA256:base64data`
    pub fn new(
        account: impl Into<String>,
        key_id: impl Into<String>,
        key_source: KeySource,
    ) -> Self {
        Self {
            account: account.into(),
            user: None,
            key_id: key_id.into(),
            key_source,
            roles: None,
        }
    }

    /// Set RBAC sub-user for this configuration
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set RBAC roles for this configuration
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = Some(roles);
        self
    }
}

/// Sign an HTTP request and return the Date and Authorization headers
///
/// # Arguments
/// * `config` - Authentication configuration
/// * `method` - HTTP method (GET, POST, PUT, DELETE, etc.)
/// * `path` - Request path (e.g., "/myaccount/machines")
///
/// # Returns
/// A tuple of (date_header_value, authorization_header_value)
///
/// # Fingerprint Handling
/// The `key_id` in config can be either MD5 or SHA256 format. When matching keys,
/// the library uses the appropriate hash algorithm. However, the Authorization
/// header always uses MD5 format (CloudAPI server requirement).
///
/// # Errors
/// Returns an error if key loading or signing fails
pub async fn sign_request(
    config: &AuthConfig,
    method: &str,
    path: &str,
) -> Result<(String, String), AuthError> {
    // Generate the date header
    let date = RequestSigner::date_header();

    // Determine key type, get public key for MD5 fingerprint, and sign
    let (key_type, signature_b64, md5_fp) = match &config.key_source {
        KeySource::Agent { fingerprint } => {
            // Find key in agent to determine type
            let pub_key = agent::find_key_in_agent(fingerprint).await?;
            let key_type = KeyType::from_public_key(&pub_key);
            // Always compute MD5 fingerprint for the Authorization header
            let md5_fp = md5_fingerprint(&pub_key);

            // Create signing string using MD5 fingerprint
            let signer = create_signer_with_fp(config, key_type, &md5_fp);
            let signing_string = signer.signing_string(method, path, &date);
            let sig_bytes = agent::sign_with_agent(fingerprint, signing_string.as_bytes()).await?;
            (key_type, encode_signature(&sig_bytes), md5_fp)
        }
        KeySource::File { .. } => {
            // Load key from file
            let key = KeyLoader::load_private_key(&config.key_source).await?;
            let key_type = KeyType::from_private_key(&key);
            // Always compute MD5 fingerprint for the Authorization header
            let md5_fp = md5_fingerprint(key.public_key());

            // Create signing string and sign
            let signer = create_signer_with_fp(config, key_type, &md5_fp);
            let signing_string = signer.signing_string(method, path, &date);
            let signature_b64 = sign_with_key(&key, signing_string.as_bytes())?;
            (key_type, signature_b64, md5_fp)
        }
        KeySource::Auto { fingerprint } => {
            // Try agent first, fall back to file
            match agent::find_key_in_agent(fingerprint).await {
                Ok(pub_key) => {
                    let key_type = KeyType::from_public_key(&pub_key);
                    // Always compute MD5 fingerprint for the Authorization header
                    let md5_fp = md5_fingerprint(&pub_key);

                    let signer = create_signer_with_fp(config, key_type, &md5_fp);
                    let signing_string = signer.signing_string(method, path, &date);
                    let sig_bytes =
                        agent::sign_with_agent(fingerprint, signing_string.as_bytes()).await?;
                    (key_type, encode_signature(&sig_bytes), md5_fp)
                }
                Err(_) => {
                    // Fall back to file-based key loading
                    // Use internal search which will scan ~/.ssh/ for matching keys
                    let key = key_loader::KeyLoader::load_private_key(&KeySource::Auto {
                        fingerprint: fingerprint.clone(),
                    })
                    .await?;
                    let key_type = KeyType::from_private_key(&key);
                    // Always compute MD5 fingerprint for the Authorization header
                    let md5_fp = md5_fingerprint(key.public_key());

                    let signer = create_signer_with_fp(config, key_type, &md5_fp);
                    let signing_string = signer.signing_string(method, path, &date);
                    let signature_b64 = sign_with_key(&key, signing_string.as_bytes())?;
                    (key_type, signature_b64, md5_fp)
                }
            }
        }
    };

    // Create the authorization header using MD5 fingerprint
    let signer = create_signer_with_fp(config, key_type, &md5_fp);
    let auth_header = signer.authorization_header(&signature_b64);

    Ok((date, auth_header))
}

/// Helper to create a RequestSigner from config with explicit MD5 fingerprint
fn create_signer_with_fp(config: &AuthConfig, key_type: KeyType, md5_fp: &str) -> RequestSigner {
    let signer = RequestSigner::new(&config.account, md5_fp, key_type);
    if let Some(ref user) = config.user {
        signer.with_subuser(user)
    } else {
        signer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_builder() {
        let config = AuthConfig::new("myaccount", "aa:bb:cc:dd", KeySource::agent("aa:bb:cc:dd"))
            .with_user("subuser")
            .with_roles(vec!["admin".to_string(), "operator".to_string()]);

        assert_eq!(config.account, "myaccount");
        assert_eq!(config.key_id, "aa:bb:cc:dd");
        assert_eq!(config.user, Some("subuser".to_string()));
        assert_eq!(
            config.roles,
            Some(vec!["admin".to_string(), "operator".to_string()])
        );
    }
}
