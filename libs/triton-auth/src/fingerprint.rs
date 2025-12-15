// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH key fingerprint handling for Triton authentication
//!
//! This module supports both MD5 and SHA256 fingerprint formats:
//!
//! - **MD5**: `aa:bb:cc:dd:...` or `MD5:aa:bb:cc:dd:...` (16 bytes, 32 hex chars)
//! - **SHA256**: `SHA256:base64data` (32 bytes, 43 base64 chars)
//!
//! Users can provide fingerprints in either format (matching modern `ssh-keygen -l`
//! output which defaults to SHA256). The library will:
//!
//! 1. Parse and understand both formats
//! 2. Match keys using the provided format
//! 3. Always use MD5 format in the Authorization header (CloudAPI requirement)

use base64::Engine;
use md5::{Digest, Md5};
use sha2::Sha256;
use ssh_key::PublicKey;

/// A parsed SSH key fingerprint (either MD5 or SHA256)
///
/// This enum allows users to provide fingerprints in either format while
/// enabling proper key matching and Authorization header generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fingerprint {
    /// MD5 fingerprint (16 bytes)
    Md5([u8; 16]),
    /// SHA256 fingerprint (32 bytes)
    Sha256([u8; 32]),
}

impl Fingerprint {
    /// Parse a fingerprint string in either MD5 or SHA256 format
    ///
    /// Accepted formats:
    /// - MD5: `aa:bb:cc:dd:...`, `MD5:aa:bb:cc:dd:...`, or plain hex
    /// - SHA256: `SHA256:base64data` (with or without padding, matching `ssh-keygen -l` output)
    ///
    /// # Example
    ///
    /// ```
    /// use triton_auth::Fingerprint;
    ///
    /// // MD5 format
    /// let md5_fp = Fingerprint::parse("fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6").unwrap();
    ///
    /// // SHA256 format (without padding, as output by ssh-keygen)
    /// let sha256_fp = Fingerprint::parse("SHA256:VMEY3GIT7bS01hFFB6kjrLICl1tf2jomkT9JqpsUQmU").unwrap();
    ///
    /// // SHA256 format (with padding also works)
    /// let sha256_fp2 = Fingerprint::parse("SHA256:VMEY3GIT7bS01hFFB6kjrLICl1tf2jomkT9JqpsUQmU=").unwrap();
    /// ```
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();

        if let Some(b64) = s.strip_prefix("SHA256:") {
            // SHA256 fingerprint in base64 (may or may not have padding)
            // OpenSSH outputs without padding, but we accept both
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(b64.trim_end_matches('='))
                .map_err(|e| format!("Invalid SHA256 fingerprint base64: {}", e))?;

            if bytes.len() != 32 {
                return Err(format!(
                    "Invalid SHA256 fingerprint length: expected 32 bytes, got {}",
                    bytes.len()
                ));
            }

            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(Fingerprint::Sha256(arr))
        } else {
            // MD5 fingerprint
            let bytes = parse_md5_fingerprint(s)?;
            Ok(Fingerprint::Md5(bytes))
        }
    }

    /// Check if this fingerprint matches a public key
    ///
    /// Computes the appropriate hash (MD5 or SHA256) of the key and compares.
    pub fn matches(&self, key: &PublicKey) -> bool {
        let key_bytes = match key.to_bytes() {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        match self {
            Fingerprint::Md5(expected) => {
                let mut hasher = Md5::new();
                hasher.update(&key_bytes);
                let result = hasher.finalize();
                result.as_slice() == expected
            }
            Fingerprint::Sha256(expected) => {
                let mut hasher = Sha256::new();
                hasher.update(&key_bytes);
                let result = hasher.finalize();
                result.as_slice() == expected
            }
        }
    }

    /// Check if this fingerprint matches raw public key bytes (OpenSSH wire format)
    pub fn matches_bytes(&self, key_bytes: &[u8]) -> bool {
        match self {
            Fingerprint::Md5(expected) => {
                let mut hasher = Md5::new();
                hasher.update(key_bytes);
                let result = hasher.finalize();
                result.as_slice() == expected
            }
            Fingerprint::Sha256(expected) => {
                let mut hasher = Sha256::new();
                hasher.update(key_bytes);
                let result = hasher.finalize();
                result.as_slice() == expected
            }
        }
    }

    /// Convert to MD5 format string (for Authorization header)
    ///
    /// **Note**: This only works if the fingerprint is already MD5.
    /// For SHA256 fingerprints, you must use `md5_fingerprint()` on the actual
    /// public key to get the MD5 representation.
    pub fn to_md5_string(&self) -> Option<String> {
        match self {
            Fingerprint::Md5(bytes) => Some(format_md5_fingerprint(bytes)),
            Fingerprint::Sha256(_) => None,
        }
    }

    /// Format as a display string
    ///
    /// SHA256 fingerprints are output without padding (matching `ssh-keygen -l` format)
    pub fn to_string_repr(&self) -> String {
        match self {
            Fingerprint::Md5(bytes) => format_md5_fingerprint(bytes),
            Fingerprint::Sha256(bytes) => {
                // Output without padding to match ssh-keygen format
                let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
                format!("SHA256:{}", b64)
            }
        }
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string_repr())
    }
}

impl std::str::FromStr for Fingerprint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Fingerprint::parse(s)
    }
}

/// Calculate MD5 fingerprint of an SSH public key in colon-separated hex format
///
/// Returns format like "aa:bb:cc:dd:ee:ff:..."
///
/// # Example
///
/// ```ignore
/// use ssh_key::PublicKey;
/// use triton_auth::fingerprint::md5_fingerprint;
///
/// let key: PublicKey = /* load key */;
/// let fp = md5_fingerprint(&key);
/// // fp = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"
/// ```
pub fn md5_fingerprint(key: &PublicKey) -> String {
    // Get the key in OpenSSH wire format (type + key data)
    // unwrap() is safe here because encoding a valid PublicKey should always succeed
    let key_bytes = key.to_bytes().expect("Failed to encode public key");
    md5_fingerprint_bytes(&key_bytes)
}

/// Calculate SHA256 fingerprint of an SSH public key in base64 format
///
/// Returns format like "SHA256:base64data"
pub fn sha256_fingerprint(key: &PublicKey) -> String {
    let key_bytes = key.to_bytes().expect("Failed to encode public key");
    sha256_fingerprint_bytes(&key_bytes)
}

/// Calculate MD5 fingerprint from raw public key bytes (OpenSSH wire format)
///
/// The bytes should be in OpenSSH wire format: 4-byte length + key type string
/// + key-specific data.
pub fn md5_fingerprint_bytes(key_bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(key_bytes);
    let result = hasher.finalize();

    result
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(":")
}

/// Calculate SHA256 fingerprint from raw public key bytes (OpenSSH wire format)
///
/// Returns format matching `ssh-keygen -l` output (without padding)
pub fn sha256_fingerprint_bytes(key_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key_bytes);
    let result = hasher.finalize();
    // Use NO_PAD to match ssh-keygen output format
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(result);
    format!("SHA256:{}", b64)
}

/// Parse an MD5 fingerprint string into bytes
///
/// Accepts formats:
/// - Colon-separated: "aa:bb:cc:dd:..."
/// - Plain hex: "aabbccdd..."
/// - With optional "MD5:" prefix
pub fn parse_fingerprint(fp: &str) -> Result<[u8; 16], String> {
    parse_md5_fingerprint(fp)
}

/// Parse an MD5 fingerprint string into bytes (internal helper)
fn parse_md5_fingerprint(fp: &str) -> Result<[u8; 16], String> {
    let fp = fp.trim();

    // Remove MD5: prefix if present
    let fp = fp.strip_prefix("MD5:").unwrap_or(fp);

    // Remove colons
    let hex: String = fp.chars().filter(|c| *c != ':').collect();

    if hex.len() != 32 {
        return Err(format!(
            "Invalid MD5 fingerprint length: expected 32 hex chars, got {}",
            hex.len()
        ));
    }

    let mut result = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hex_str = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        result[i] = u8::from_str_radix(hex_str, 16).map_err(|e| e.to_string())?;
    }

    Ok(result)
}

/// Format MD5 fingerprint bytes as colon-separated hex string
pub fn format_fingerprint(bytes: &[u8; 16]) -> String {
    format_md5_fingerprint(bytes)
}

/// Format MD5 fingerprint bytes as colon-separated hex string (internal helper)
fn format_md5_fingerprint(bytes: &[u8; 16]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_fingerprint_format() {
        // Test that fingerprint has correct format
        let test_bytes = b"test public key data";
        let fp = md5_fingerprint_bytes(test_bytes);

        // Should be 16 bytes = 32 hex chars + 15 colons = 47 chars
        assert_eq!(fp.len(), 47);
        assert_eq!(fp.chars().filter(|c| *c == ':').count(), 15);

        // Each segment should be 2 hex chars
        for segment in fp.split(':') {
            assert_eq!(segment.len(), 2);
            assert!(segment.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn test_parse_fingerprint_colon_separated() {
        let fp = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
        let bytes = parse_fingerprint(fp).unwrap();
        assert_eq!(bytes[0], 0xfa);
        assert_eq!(bytes[1], 0x56);
        assert_eq!(bytes[15], 0xc6);
    }

    #[test]
    fn test_parse_fingerprint_with_prefix() {
        let fp = "MD5:fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
        let bytes = parse_fingerprint(fp).unwrap();
        assert_eq!(bytes[0], 0xfa);
    }

    #[test]
    fn test_parse_fingerprint_plain_hex() {
        let fp = "fa56a16bcc0497fee29854c42e0d26c6";
        let bytes = parse_fingerprint(fp).unwrap();
        assert_eq!(bytes[0], 0xfa);
        assert_eq!(bytes[15], 0xc6);
    }

    #[test]
    fn test_format_fingerprint() {
        let bytes = [
            0xfa, 0x56, 0xa1, 0x6b, 0xcc, 0x04, 0x97, 0xfe, 0xe2, 0x98, 0x54, 0xc4, 0x2e, 0x0d,
            0x26, 0xc6,
        ];
        let fp = format_fingerprint(&bytes);
        assert_eq!(fp, "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6");
    }

    #[test]
    fn test_roundtrip() {
        let original = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6";
        let bytes = parse_fingerprint(original).unwrap();
        let formatted = format_fingerprint(&bytes);
        assert_eq!(original, formatted);
    }

    // SHA256 fingerprint tests

    #[test]
    fn test_fingerprint_parse_md5() {
        let fp = Fingerprint::parse("fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6").unwrap();
        assert!(matches!(fp, Fingerprint::Md5(_)));
        assert_eq!(
            fp.to_string(),
            "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"
        );
    }

    #[test]
    fn test_fingerprint_parse_md5_with_prefix() {
        let fp = Fingerprint::parse("MD5:fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6").unwrap();
        assert!(matches!(fp, Fingerprint::Md5(_)));
    }

    #[test]
    fn test_fingerprint_parse_sha256() {
        // SHA256 fingerprint without padding (as output by ssh-keygen)
        let fp = Fingerprint::parse("SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4").unwrap();
        assert!(matches!(fp, Fingerprint::Sha256(_)));
        // Output should also be without padding
        assert_eq!(
            fp.to_string(),
            "SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4"
        );
    }

    #[test]
    fn test_fingerprint_parse_sha256_with_padding() {
        // SHA256 fingerprint with padding should also work
        let fp = Fingerprint::parse("SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4=").unwrap();
        assert!(matches!(fp, Fingerprint::Sha256(_)));
    }

    #[test]
    fn test_fingerprint_to_md5_string() {
        let md5_fp = Fingerprint::parse("fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6").unwrap();
        assert_eq!(
            md5_fp.to_md5_string(),
            Some("fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6".to_string())
        );

        let sha256_fp =
            Fingerprint::parse("SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4").unwrap();
        assert_eq!(sha256_fp.to_md5_string(), None);
    }

    #[test]
    fn test_fingerprint_matches_bytes() {
        let test_data = b"test public key data";

        // Calculate actual MD5 of test data
        let mut hasher = Md5::new();
        hasher.update(test_data);
        let md5_result = hasher.finalize();
        let md5_fp_str = md5_result
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(":");

        let fp = Fingerprint::parse(&md5_fp_str).unwrap();
        assert!(fp.matches_bytes(test_data));

        // Calculate actual SHA256 of test data (without padding, like ssh-keygen)
        let mut hasher = Sha256::new();
        hasher.update(test_data);
        let sha256_result = hasher.finalize();
        let sha256_fp_str = format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha256_result)
        );

        let fp = Fingerprint::parse(&sha256_fp_str).unwrap();
        assert!(fp.matches_bytes(test_data));
    }

    #[test]
    fn test_fingerprint_from_str() {
        let fp: Fingerprint = "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6"
            .parse()
            .unwrap();
        assert!(matches!(fp, Fingerprint::Md5(_)));

        // Without padding (ssh-keygen format)
        let fp: Fingerprint = "SHA256:29GY+6bxcBkcNNUzTnEcTdTv1W3d3PN/OxyplcYSoX4"
            .parse()
            .unwrap();
        assert!(matches!(fp, Fingerprint::Sha256(_)));
    }

    #[test]
    fn test_sha256_fingerprint_bytes() {
        let test_data = b"test public key data";
        let fp = sha256_fingerprint_bytes(test_data);
        assert!(fp.starts_with("SHA256:"));
    }
}
