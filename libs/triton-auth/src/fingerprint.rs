// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! MD5 fingerprint calculation for SSH public keys
//!
//! CloudAPI uses MD5 fingerprints in colon-separated hex format for key
//! identification in HTTP Signature authentication.

use md5::{Digest, Md5};
use ssh_key::PublicKey;

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

/// Parse an MD5 fingerprint string into bytes
///
/// Accepts formats:
/// - Colon-separated: "aa:bb:cc:dd:..."
/// - Plain hex: "aabbccdd..."
/// - With optional "MD5:" prefix
pub fn parse_fingerprint(fp: &str) -> Result<[u8; 16], String> {
    let fp = fp.trim();

    // Remove MD5: prefix if present
    let fp = fp.strip_prefix("MD5:").unwrap_or(fp);

    // Remove colons
    let hex: String = fp.chars().filter(|c| *c != ':').collect();

    if hex.len() != 32 {
        return Err(format!(
            "Invalid fingerprint length: expected 32 hex chars, got {}",
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

/// Format fingerprint bytes as colon-separated hex string
pub fn format_fingerprint(bytes: &[u8; 16]) -> String {
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
}
