// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2018 Joyent, Inc.
// Copyright 2024 MNX Cloud, Inc.
// Copyright 2025 Edgecast Cloud LLC.

//! Direct SSH agent protocol implementation
//!
//! This module provides a minimal SSH agent client that communicates directly
//! with the SSH agent via Unix socket. It implements just the operations needed
//! for HTTP Signature authentication:
//!
//! - List identities (keys) in the agent
//! - Sign data with a specific key
//!
//! This implementation gives us full control over the SSH agent protocol,
//! including the ability to specify RSA signature algorithm flags (SHA-256 vs SHA-512).

use std::fmt;
use std::io::prelude::*;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use base64::Engine;
use md5::Md5;
use sha2::Sha256;

// Re-export Digest trait from md5 for both hashers
use md5::Digest;

use crate::error::AuthError;

/// SSH agent protocol message types
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// SSH agent signature flags (RFC 8332)
/// Request RSA signature using SHA-256 hash algorithm
const SSH_AGENT_RSA_SHA2_256: u32 = 0x02;

/// Read a big-endian u32 from a buffer at the given offset
fn read_u32be(buf: &[u8], offset: usize) -> u32 {
    ((buf[offset] as u32) << 24)
        + ((buf[offset + 1] as u32) << 16)
        + ((buf[offset + 2] as u32) << 8)
        + (buf[offset + 3] as u32)
}

/// Read a u8 from a buffer at the given offset
fn read_u8(buf: &[u8], offset: usize) -> u8 {
    buf[offset]
}

/// Read a string from a buffer at the given offset with the given length
fn read_string(buf: &[u8], offset: usize, len: usize) -> String {
    let slice = &buf[offset..(offset + len)];
    String::from_utf8(slice.to_vec()).unwrap_or_default()
}

/// Write bytes to a buffer at the given offset
fn write_bytes(buf: &mut [u8], bytes: &[u8], offset: usize) {
    buf[offset..(bytes.len() + offset)].copy_from_slice(bytes);
}

/// Write a big-endian u32 to a buffer at the given offset
fn write_u32be(buf: &mut [u8], num: u32, offset: usize) {
    buf[offset] = ((num >> 24) & 0xff) as u8;
    buf[offset + 1] = ((num >> 16) & 0xff) as u8;
    buf[offset + 2] = ((num >> 8) & 0xff) as u8;
    buf[offset + 3] = (num & 0xff) as u8;
}

/// Write a u8 to a buffer at the given offset
fn write_u8(buf: &mut [u8], num: u8, offset: usize) {
    buf[offset] = num;
}

/// Compute MD5 fingerprint of SSH public key bytes
///
/// Returns colon-separated hex string like "aa:bb:cc:dd:..."
pub fn md5_fingerprint(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let sum = hasher.finalize();
    let strs: Vec<String> = sum.iter().map(|b| format!("{:02x}", b)).collect();
    strs.join(":")
}

/// Compute SHA256 fingerprint of SSH public key bytes
///
/// Returns string like "SHA256:base64data"
pub fn sha256_fingerprint(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sum = hasher.finalize();
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(sum)
    )
}

/// An SSH identity (key) from the agent
#[derive(Clone)]
pub struct SshIdentity {
    /// The key type (e.g., "ssh-rsa", "ecdsa-sha2-nistp256", "ssh-ed25519")
    pub key_type: String,
    /// The key comment (usually the key file path or email)
    pub comment: String,
    /// MD5 fingerprint in colon-separated hex format
    pub md5_fp: String,
    /// SHA256 fingerprint in "SHA256:base64" format
    pub sha256_fp: String,
    /// Raw public key bytes in SSH wire format
    pub raw_key: Vec<u8>,
}

impl SshIdentity {
    /// Create a new SshIdentity from raw key bytes and comment
    pub fn new(bytes: &[u8], comment: &str) -> SshIdentity {
        // The type of the key is held in the key itself - extract it here
        let type_len = read_u32be(bytes, 0) as usize;
        let key_type = read_string(bytes, 4, type_len);

        // Generate fingerprints
        let md5_fp = md5_fingerprint(bytes);
        let sha256_fp = sha256_fingerprint(bytes);

        SshIdentity {
            raw_key: bytes.to_vec(),
            key_type,
            comment: comment.to_string(),
            md5_fp,
            sha256_fp,
        }
    }

    /// Check if this identity matches the given fingerprint
    ///
    /// Supports both MD5 (aa:bb:cc:...) and SHA256 (SHA256:base64) formats
    pub fn matches_fingerprint(&self, fingerprint: &str) -> bool {
        let fp = fingerprint.trim();

        if fp.starts_with("SHA256:") {
            self.sha256_fp == fp
        } else if let Some(fp_stripped) = fp.strip_prefix("MD5:") {
            self.md5_fp == fp_stripped
        } else {
            // Assume MD5 format if no prefix
            self.md5_fp == fp
        }
    }

    /// Check if this is an RSA key
    pub fn is_rsa(&self) -> bool {
        self.key_type == "ssh-rsa"
    }
}

impl fmt::Display for SshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SshIdentity: {} {} {}",
            self.key_type, self.sha256_fp, self.comment
        )
    }
}

/// SSH agent client for communicating with ssh-agent
pub struct SshAgentClient {
    stream: UnixStream,
}

impl SshAgentClient {
    /// Connect to the SSH agent using the given socket path
    pub fn connect(socket_path: &str) -> Result<SshAgentClient, AuthError> {
        let stream = UnixStream::connect(socket_path)
            .map_err(|e| AuthError::AgentError(format!("Failed to connect to SSH agent: {}", e)))?;

        stream
            .set_read_timeout(Some(Duration::new(5, 0)))
            .map_err(|e| AuthError::AgentError(format!("Failed to set read timeout: {}", e)))?;

        Ok(SshAgentClient { stream })
    }

    /// Connect to the SSH agent using SSH_AUTH_SOCK environment variable
    pub fn connect_env() -> Result<SshAgentClient, AuthError> {
        let socket_path = std::env::var("SSH_AUTH_SOCK").map_err(|_| {
            AuthError::AgentError(
                "SSH_AUTH_SOCK environment variable not set. Is ssh-agent running?".to_string(),
            )
        })?;
        Self::connect(&socket_path)
    }

    /// List all identities (keys) in the SSH agent
    pub fn list_identities(&mut self) -> Result<Vec<SshIdentity>, AuthError> {
        let mut identities: Vec<SshIdentity> = Vec::new();

        // Write request for identities
        let buf = [0, 0, 0, 1, SSH_AGENTC_REQUEST_IDENTITIES];
        self.stream
            .write_all(&buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to write to SSH agent: {}", e)))?;

        // Read the response length first
        let mut buf = vec![0; 4];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to read from SSH agent: {}", e)))?;
        let len = read_u32be(&buf, 0);

        // Read the rest of the response
        let mut buf = vec![0; len as usize];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to read from SSH agent: {}", e)))?;

        let mut idx = 0;

        // First byte should be the correct response type
        let response_type = read_u8(&buf, idx);
        if response_type != SSH_AGENT_IDENTITIES_ANSWER {
            return Err(AuthError::AgentError(format!(
                "Unexpected response type: {}",
                response_type
            )));
        }
        idx += 1;

        // Next u32 is the number of keys in the agent
        let num_keys = read_u32be(&buf, idx);
        idx += 4;

        // Loop through each key found
        for _ in 0..num_keys {
            // Read key length
            let len = read_u32be(&buf, idx) as usize;
            idx += 4;

            // Extract the bytes for the key
            let bytes = &buf[idx..(idx + len)];
            idx += len;

            // Read the comment
            let len = read_u32be(&buf, idx) as usize;
            idx += 4;
            let comment = read_string(&buf, idx, len);
            idx += len;

            // Make a new SshIdentity
            let ident = SshIdentity::new(bytes, &comment);
            identities.push(ident);
        }

        Ok(identities)
    }

    /// Find a key in the agent matching the given fingerprint
    pub fn find_identity(&mut self, fingerprint: &str) -> Result<SshIdentity, AuthError> {
        let identities = self.list_identities()?;

        for ident in identities {
            if ident.matches_fingerprint(fingerprint) {
                return Ok(ident);
            }
        }

        Err(AuthError::KeyNotFound(format!(
            "Key with fingerprint {} not found in SSH agent",
            fingerprint
        )))
    }

    /// Sign data with the given identity
    ///
    /// For RSA keys, this requests SHA-256 signatures (rsa-sha2-256) which is
    /// required for CloudAPI compatibility.
    pub fn sign_data(&mut self, identity: &SshIdentity, data: &[u8]) -> Result<Vec<u8>, AuthError> {
        let mut idx = 0;
        let mut buf = vec![0; 4 + 1 + 4 + identity.raw_key.len() + 4 + data.len() + 4];

        let len = buf.len() - 4;
        write_u32be(&mut buf, len as u32, idx);
        idx += 4;

        write_u8(&mut buf, SSH_AGENTC_SIGN_REQUEST, idx);
        idx += 1;

        write_u32be(&mut buf, identity.raw_key.len() as u32, idx);
        idx += 4;

        write_bytes(&mut buf, &identity.raw_key, idx);
        idx += identity.raw_key.len();

        write_u32be(&mut buf, data.len() as u32, idx);
        idx += 4;

        write_bytes(&mut buf, data, idx);
        idx += data.len();

        // Write signature flags
        // For RSA keys, request SHA-256 signatures (required for CloudAPI)
        let flags = if identity.is_rsa() {
            SSH_AGENT_RSA_SHA2_256
        } else {
            0
        };
        write_u32be(&mut buf, flags, idx);

        self.stream
            .write_all(&buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to write to SSH agent: {}", e)))?;

        // Read the response length first
        let mut buf = vec![0; 4];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to read from SSH agent: {}", e)))?;
        let len = read_u32be(&buf, 0);

        // Read the rest of the response
        let mut buf = vec![0; len as usize];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| AuthError::AgentError(format!("Failed to read from SSH agent: {}", e)))?;

        let mut idx = 0;

        // First byte should be the correct response type
        let response_type = read_u8(&buf, idx);
        if response_type != SSH_AGENT_SIGN_RESPONSE {
            return Err(AuthError::AgentError(format!(
                "Unexpected response type: {}, expected sign response",
                response_type
            )));
        }
        idx += 1;

        // Next u32 is the total signature blob length
        let _total_len = read_u32be(&buf, idx);
        idx += 4;

        // Read signature type string
        let len = read_u32be(&buf, idx) as usize;
        idx += 4;
        let _sig_type = read_string(&buf, idx, len);
        idx += len;

        // Read the actual signature bytes
        let len = read_u32be(&buf, idx) as usize;
        let blob = &buf[(idx + 4)..(idx + 4 + len)];

        Ok(blob.to_vec())
    }
}

impl fmt::Display for SshAgentClient {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SshAgentClient: {:?}", self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_fingerprint() {
        // Test vector: a simple byte sequence
        let bytes = b"test key data";
        let fp = md5_fingerprint(bytes);
        // Should be colon-separated hex
        assert!(fp.contains(':'));
        assert_eq!(fp.matches(':').count(), 15); // 16 bytes = 15 colons
    }

    #[test]
    fn test_sha256_fingerprint() {
        let bytes = b"test key data";
        let fp = sha256_fingerprint(bytes);
        assert!(fp.starts_with("SHA256:"));
    }

    #[test]
    fn test_identity_matches_fingerprint() {
        // Create a mock identity with known fingerprints
        let ident = SshIdentity {
            key_type: "ssh-rsa".to_string(),
            comment: "test".to_string(),
            md5_fp: "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99".to_string(),
            sha256_fp: "SHA256:abcdefghijklmnop".to_string(),
            raw_key: vec![],
        };

        // Test MD5 matching (no prefix)
        assert!(ident.matches_fingerprint("aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"));

        // Test MD5 matching (with prefix)
        assert!(ident.matches_fingerprint("MD5:aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"));

        // Test SHA256 matching
        assert!(ident.matches_fingerprint("SHA256:abcdefghijklmnop"));

        // Test non-matching
        assert!(!ident.matches_fingerprint("SHA256:different"));
        assert!(!ident.matches_fingerprint("00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00"));
    }

    #[test]
    fn test_is_rsa() {
        let rsa_ident = SshIdentity {
            key_type: "ssh-rsa".to_string(),
            comment: "".to_string(),
            md5_fp: "".to_string(),
            sha256_fp: "".to_string(),
            raw_key: vec![],
        };
        assert!(rsa_ident.is_rsa());

        let ed25519_ident = SshIdentity {
            key_type: "ssh-ed25519".to_string(),
            comment: "".to_string(),
            md5_fp: "".to_string(),
            sha256_fp: "".to_string(),
            raw_key: vec![],
        };
        assert!(!ed25519_ident.is_rsa());
    }
}
