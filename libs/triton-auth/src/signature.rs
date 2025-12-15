// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! HTTP Signature generation for CloudAPI authentication
//!
//! Implements the HTTP Signature scheme used by Triton CloudAPI:
//!
//! ```text
//! Authorization: Signature keyId="/:account/keys/:fingerprint",algorithm="rsa-sha256",signature=":base64:"
//! ```
//!
//! The signature is computed over the concatenation of:
//! - `date: <RFC2822 date header value>`
//! - `\n`
//! - `(request-target): <method lowercase> <path>`

use crate::error::AuthError;
use base64::Engine;
use chrono::Utc;
use ssh_key::{HashAlg, PrivateKey};

/// Key type for algorithm selection in HTTP signatures
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    Rsa,
    Dsa,
    Ecdsa256,
    Ecdsa384,
    Ecdsa521,
    Ed25519,
}

impl KeyType {
    /// Determine key type from an SSH private key
    pub fn from_private_key(key: &PrivateKey) -> Self {
        use ssh_key::Algorithm;
        match key.algorithm() {
            Algorithm::Rsa { .. } => Self::Rsa,
            Algorithm::Dsa => Self::Dsa,
            Algorithm::Ecdsa { curve } => match curve.as_str() {
                "nistp256" => Self::Ecdsa256,
                "nistp384" => Self::Ecdsa384,
                "nistp521" => Self::Ecdsa521,
                _ => Self::Ecdsa256,
            },
            Algorithm::Ed25519 => Self::Ed25519,
            _ => Self::Rsa, // fallback
        }
    }

    /// Determine key type from an SSH public key
    pub fn from_public_key(key: &ssh_key::PublicKey) -> Self {
        use ssh_key::Algorithm;
        match key.algorithm() {
            Algorithm::Rsa { .. } => Self::Rsa,
            Algorithm::Dsa => Self::Dsa,
            Algorithm::Ecdsa { curve } => match curve.as_str() {
                "nistp256" => Self::Ecdsa256,
                "nistp384" => Self::Ecdsa384,
                "nistp521" => Self::Ecdsa521,
                _ => Self::Ecdsa256,
            },
            Algorithm::Ed25519 => Self::Ed25519,
            _ => Self::Rsa, // fallback
        }
    }

    /// Get the HTTP Signature algorithm string for this key type
    ///
    /// Returns strings like "rsa-sha256", "ecdsa-sha256", "ed25519-sha512"
    pub fn algorithm_string(&self) -> &'static str {
        match self {
            Self::Rsa => "rsa-sha256",
            Self::Dsa => "dsa-sha1",
            Self::Ecdsa256 => "ecdsa-sha256",
            Self::Ecdsa384 => "ecdsa-sha384",
            Self::Ecdsa521 => "ecdsa-sha512",
            Self::Ed25519 => "ed25519-sha512",
        }
    }

    /// Get the hash algorithm to use for signing
    ///
    /// Note: ssh-key 0.6 only supports Sha256 and Sha512 for HashAlg
    fn hash_alg(&self) -> HashAlg {
        match self {
            // Use SHA-256 for RSA, DSA, and ECDSA-256/384
            Self::Rsa | Self::Dsa | Self::Ecdsa256 | Self::Ecdsa384 => HashAlg::Sha256,
            // Use SHA-512 for ECDSA-521 and Ed25519
            Self::Ecdsa521 | Self::Ed25519 => HashAlg::Sha512,
        }
    }
}

/// HTTP Signature request signer
///
/// Constructs the signing string and Authorization header for CloudAPI requests.
pub struct RequestSigner {
    account: String,
    subuser: Option<String>,
    fingerprint: String, // MD5 hex format: aa:bb:cc:...
    key_type: KeyType,
}

impl RequestSigner {
    /// Create a new RequestSigner
    ///
    /// # Arguments
    /// * `account` - The CloudAPI account login name
    /// * `fingerprint` - The SSH key MD5 fingerprint (colon-separated hex)
    /// * `key_type` - The type of SSH key for algorithm selection
    pub fn new(account: &str, fingerprint: &str, key_type: KeyType) -> Self {
        Self {
            account: account.to_string(),
            subuser: None,
            fingerprint: fingerprint.to_string(),
            key_type,
        }
    }

    /// Set the RBAC sub-user for this signer
    pub fn with_subuser(mut self, subuser: impl Into<String>) -> Self {
        self.subuser = Some(subuser.into());
        self
    }

    /// Get the algorithm string for the HTTP Signature
    pub fn algorithm(&self) -> &'static str {
        self.key_type.algorithm_string()
    }

    /// Generate the keyId string for the Authorization header
    ///
    /// Format:
    /// - Without subuser: `/:account/keys/:fingerprint`
    /// - With subuser: `/:account/users/:subuser/keys/:fingerprint`
    pub fn key_id_string(&self) -> String {
        match &self.subuser {
            Some(subuser) => format!(
                "/{}/users/{}/keys/{}",
                self.account, subuser, self.fingerprint
            ),
            None => format!("/{}/keys/{}", self.account, self.fingerprint),
        }
    }

    /// Generate the signing string for an HTTP request
    ///
    /// The signing string is:
    /// ```text
    /// date: <date>
    /// (request-target): <method> <path>
    /// ```
    pub fn signing_string(&self, method: &str, path: &str, date: &str) -> String {
        format!(
            "date: {}\n(request-target): {} {}",
            date,
            method.to_lowercase(),
            path
        )
    }

    /// Generate a Date header value in RFC 2822 format
    ///
    /// Example: "Mon, 15 Dec 2025 10:30:00 GMT"
    pub fn date_header() -> String {
        Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }

    /// Generate the full Authorization header value
    ///
    /// # Arguments
    /// * `signature_b64` - The base64-encoded signature
    ///
    /// # Returns
    /// The complete Authorization header value:
    /// ```text
    /// Signature keyId="/:account/keys/:fp",algorithm="rsa-sha256",signature=":sig:"
    /// ```
    pub fn authorization_header(&self, signature_b64: &str) -> String {
        format!(
            "Signature keyId=\"{}\",algorithm=\"{}\",signature=\"{}\"",
            self.key_id_string(),
            self.algorithm(),
            signature_b64
        )
    }
}

/// Sign data with an SSH private key
///
/// # Arguments
/// * `key` - The SSH private key to sign with
/// * `data` - The data to sign (typically the signing string)
///
/// # Returns
/// The base64-encoded signature
pub fn sign_with_key(key: &PrivateKey, data: &[u8]) -> Result<String, AuthError> {
    let key_type = KeyType::from_private_key(key);
    let hash_alg = key_type.hash_alg();

    // Sign using the ssh-key crate
    // The first argument is a namespace (empty string for SSH signatures)
    let signature = key
        .sign("", hash_alg, data)
        .map_err(|e| AuthError::SigningError(format!("Failed to sign data: {}", e)))?;

    // Encode the signature as base64
    // signature_bytes() returns the raw serialized signature data
    let sig_bytes = signature.signature_bytes();
    Ok(base64::engine::general_purpose::STANDARD.encode(sig_bytes))
}

/// Encode raw signature bytes as base64
pub fn encode_signature(sig_bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sig_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signing_string_format() {
        let signer = RequestSigner::new(
            "testaccount",
            "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
            KeyType::Rsa,
        );

        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let signing_string = signer.signing_string("GET", "/testaccount/machines", date);

        assert!(signing_string.contains("date: Mon, 15 Dec 2025 10:30:00 GMT"));
        assert!(signing_string.contains("(request-target): get /testaccount/machines"));
        assert!(signing_string.starts_with("date:"));
        assert!(signing_string.contains('\n'));
    }

    #[test]
    fn test_signing_string_method_lowercase() {
        let signer = RequestSigner::new("test", "aa:bb:cc:dd", KeyType::Rsa);
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";

        // POST should become post
        let signing_string = signer.signing_string("POST", "/test/machines", date);
        assert!(signing_string.contains("(request-target): post /test/machines"));

        // GET should become get
        let signing_string = signer.signing_string("GET", "/test/machines", date);
        assert!(signing_string.contains("(request-target): get /test/machines"));
    }

    #[test]
    fn test_key_id_without_subuser() {
        let signer = RequestSigner::new(
            "testaccount",
            "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
            KeyType::Rsa,
        );

        assert_eq!(
            signer.key_id_string(),
            "/testaccount/keys/fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"
        );
    }

    #[test]
    fn test_key_id_with_subuser() {
        let signer = RequestSigner::new(
            "mainaccount",
            "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
            KeyType::Rsa,
        )
        .with_subuser("subuser");

        assert_eq!(
            signer.key_id_string(),
            "/mainaccount/users/subuser/keys/fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"
        );
    }

    #[test]
    fn test_authorization_header_format() {
        let signer = RequestSigner::new(
            "testaccount",
            "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
            KeyType::Rsa,
        );

        let auth = signer.authorization_header("dGVzdHNpZ25hdHVyZQ==");

        assert!(auth.starts_with("Signature keyId=\"/testaccount/keys/"));
        assert!(auth.contains("algorithm=\"rsa-sha256\""));
        assert!(auth.contains("signature=\"dGVzdHNpZ25hdHVyZQ==\""));
    }

    #[test]
    fn test_algorithm_strings() {
        assert_eq!(KeyType::Rsa.algorithm_string(), "rsa-sha256");
        assert_eq!(KeyType::Dsa.algorithm_string(), "dsa-sha1");
        assert_eq!(KeyType::Ecdsa256.algorithm_string(), "ecdsa-sha256");
        assert_eq!(KeyType::Ecdsa384.algorithm_string(), "ecdsa-sha384");
        assert_eq!(KeyType::Ecdsa521.algorithm_string(), "ecdsa-sha512");
        assert_eq!(KeyType::Ed25519.algorithm_string(), "ed25519-sha512");
    }

    #[test]
    fn test_date_header_format() {
        let date = RequestSigner::date_header();

        // Should be RFC 2822 format: "Day, DD Mon YYYY HH:MM:SS GMT"
        assert!(date.ends_with(" GMT"));
        assert!(date.contains(','));

        // Should have reasonable length (about 29 chars)
        assert!(date.len() >= 25 && date.len() <= 35);
    }
}
