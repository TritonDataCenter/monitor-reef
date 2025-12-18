// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH agent integration for key-based authentication
//!
//! This module provides functions to interact with an SSH agent for:
//! - Finding keys by fingerprint (MD5 or SHA256)
//! - Signing data using keys stored in the agent
//!
//! The SSH agent is accessed via the `SSH_AUTH_SOCK` environment variable.

use crate::error::AuthError;
use crate::ssh_agent::SshAgentClient;
use ssh_encoding::Decode;
use ssh_key::PublicKey;

/// Find a key in the SSH agent matching the given fingerprint (MD5 or SHA256)
///
/// The fingerprint can be in either format:
/// - MD5: `aa:bb:cc:dd:...` or `MD5:aa:bb:cc:dd:...`
/// - SHA256: `SHA256:base64data`
///
/// Returns the public key if found, otherwise returns KeyNotFound error.
pub async fn find_key_in_agent(fingerprint: &str) -> Result<PublicKey, AuthError> {
    let fp = fingerprint.to_string();

    // ssh_agent module is synchronous, so we run it in a blocking task
    tokio::task::spawn_blocking(move || find_key_in_agent_sync(&fp))
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Synchronous version of find_key_in_agent
fn find_key_in_agent_sync(fingerprint: &str) -> Result<PublicKey, AuthError> {
    let mut client = SshAgentClient::connect_env()?;
    let identity = client.find_identity(fingerprint)?;

    // Convert raw key bytes to ssh_key::PublicKey
    parse_public_key_from_bytes(&identity.raw_key)
}

/// Sign data using a key from the SSH agent
///
/// # Arguments
/// * `fingerprint` - Fingerprint of the key to use (MD5 or SHA256 format)
/// * `data` - Data to sign
///
/// # Returns
/// The raw signature bytes (not base64 encoded)
pub async fn sign_with_agent(fingerprint: &str, data: &[u8]) -> Result<Vec<u8>, AuthError> {
    let fp = fingerprint.to_string();
    let data = data.to_vec();

    tokio::task::spawn_blocking(move || sign_with_agent_sync(&fp, &data))
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Synchronous version of sign_with_agent
fn sign_with_agent_sync(fingerprint: &str, data: &[u8]) -> Result<Vec<u8>, AuthError> {
    let mut client = SshAgentClient::connect_env()?;
    let identity = client.find_identity(fingerprint)?;
    client.sign_data(&identity, data)
}

/// List all keys available in the SSH agent
pub async fn list_agent_keys() -> Result<Vec<(PublicKey, String)>, AuthError> {
    tokio::task::spawn_blocking(list_agent_keys_sync)
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Synchronous version of list_agent_keys
fn list_agent_keys_sync() -> Result<Vec<(PublicKey, String)>, AuthError> {
    let mut client = SshAgentClient::connect_env()?;
    let identities = client.list_identities()?;

    let mut keys = Vec::new();
    for ident in identities {
        if let Ok(pub_key) = parse_public_key_from_bytes(&ident.raw_key) {
            keys.push((pub_key, ident.md5_fp));
        }
    }

    Ok(keys)
}

/// Check if the SSH agent is available and accessible
pub async fn is_agent_available() -> bool {
    tokio::task::spawn_blocking(|| SshAgentClient::connect_env().is_ok())
        .await
        .unwrap_or(false)
}

/// Parse an ssh_key::PublicKey from raw SSH wire format bytes
fn parse_public_key_from_bytes(bytes: &[u8]) -> Result<PublicKey, AuthError> {
    use ssh_key::public::KeyData;

    // The raw_key is in SSH wire format, which ssh_key can decode
    let key_data = KeyData::decode(&mut &bytes[..])
        .map_err(|e| AuthError::KeyLoadError(format!("Failed to parse public key: {}", e)))?;

    Ok(PublicKey::from(key_data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_agent_availability_check() {
        // This test just verifies the function doesn't panic
        // Actual result depends on whether ssh-agent is running
        let _available = is_agent_available().await;
    }
}
