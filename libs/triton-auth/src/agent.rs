// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! SSH agent integration for key-based authentication
//!
//! This module provides functions to interact with an SSH agent for:
//! - Finding keys by fingerprint
//! - Signing data using keys stored in the agent
//!
//! The SSH agent is accessed via the `SSH_AUTH_SOCK` environment variable.

use crate::error::AuthError;
use ssh_agent_client_rs::{Client, Identity};
use ssh_key::PublicKey;
use std::path::Path;

/// Get the SSH agent socket path from environment
fn get_agent_socket_path() -> Result<String, AuthError> {
    std::env::var("SSH_AUTH_SOCK").map_err(|_| {
        AuthError::AgentError(
            "SSH_AUTH_SOCK environment variable not set. Is ssh-agent running?".to_string(),
        )
    })
}

/// Connect to the SSH agent
fn connect_to_agent() -> Result<Client, AuthError> {
    let socket_path = get_agent_socket_path()?;
    Client::connect(Path::new(&socket_path))
        .map_err(|e| AuthError::AgentError(format!("Failed to connect to SSH agent: {}", e)))
}

/// Find a key in the SSH agent matching the given MD5 fingerprint
///
/// Returns the public key if found, otherwise returns KeyNotFound error.
pub async fn find_key_in_agent(fingerprint: &str) -> Result<PublicKey, AuthError> {
    // ssh-agent-client-rs is synchronous, so we run it in a blocking task
    let fingerprint = fingerprint.to_string();
    tokio::task::spawn_blocking(move || find_key_in_agent_sync(&fingerprint))
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Extract the public key from an Identity enum
fn extract_public_key(identity: &Identity<'_>) -> Option<PublicKey> {
    match identity {
        Identity::PublicKey(key_box) => Some(key_box.as_ref().clone().into_owned()),
        Identity::Certificate(cert_box) => {
            // Certificates also contain a public key (as KeyData)
            // Convert KeyData to PublicKey
            Some(cert_box.as_ref().public_key().clone().into())
        }
    }
}

/// Synchronous version of find_key_in_agent
fn find_key_in_agent_sync(fingerprint: &str) -> Result<PublicKey, AuthError> {
    let mut client = connect_to_agent()?;

    let identities = client
        .list_all_identities()
        .map_err(|e| AuthError::AgentError(format!("Failed to list agent identities: {}", e)))?;

    for identity in identities {
        if let Some(pub_key) = extract_public_key(&identity) {
            let id_fp = crate::fingerprint::md5_fingerprint(&pub_key);
            if id_fp == fingerprint {
                return Ok(pub_key);
            }
        }
    }

    Err(AuthError::KeyNotFound(format!(
        "Key with fingerprint {} not found in SSH agent",
        fingerprint
    )))
}

/// Sign data using a key from the SSH agent
///
/// # Arguments
/// * `fingerprint` - MD5 fingerprint of the key to use (colon-separated hex)
/// * `data` - Data to sign
///
/// # Returns
/// The raw signature bytes (not base64 encoded)
pub async fn sign_with_agent(fingerprint: &str, data: &[u8]) -> Result<Vec<u8>, AuthError> {
    let fingerprint = fingerprint.to_string();
    let data = data.to_vec();
    tokio::task::spawn_blocking(move || sign_with_agent_sync(&fingerprint, &data))
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Synchronous version of sign_with_agent
fn sign_with_agent_sync(fingerprint: &str, data: &[u8]) -> Result<Vec<u8>, AuthError> {
    let mut client = connect_to_agent()?;

    // First find the key
    let identities = client
        .list_all_identities()
        .map_err(|e| AuthError::AgentError(format!("Failed to list agent identities: {}", e)))?;

    for identity in identities {
        if let Some(pub_key) = extract_public_key(&identity) {
            let id_fp = crate::fingerprint::md5_fingerprint(&pub_key);
            if id_fp == fingerprint {
                // Sign with this key
                let signature = client.sign(identity, data).map_err(|e| {
                    AuthError::SigningError(format!("SSH agent signing failed: {}", e))
                })?;

                // Convert signature to bytes
                // The signature format depends on the key type
                return Ok(signature.as_bytes().to_vec());
            }
        }
    }

    Err(AuthError::KeyNotFound(format!(
        "Key with fingerprint {} not found in SSH agent",
        fingerprint
    )))
}

/// List all keys available in the SSH agent
pub async fn list_agent_keys() -> Result<Vec<(PublicKey, String)>, AuthError> {
    tokio::task::spawn_blocking(list_agent_keys_sync)
        .await
        .map_err(|e| AuthError::AgentError(format!("Task join error: {}", e)))?
}

/// Synchronous version of list_agent_keys
fn list_agent_keys_sync() -> Result<Vec<(PublicKey, String)>, AuthError> {
    let mut client = connect_to_agent()?;

    let identities = client
        .list_all_identities()
        .map_err(|e| AuthError::AgentError(format!("Failed to list agent identities: {}", e)))?;

    let keys: Vec<(PublicKey, String)> = identities
        .into_iter()
        .filter_map(|identity| {
            extract_public_key(&identity).map(|key| {
                let fp = crate::fingerprint::md5_fingerprint(&key);
                (key, fp)
            })
        })
        .collect();

    Ok(keys)
}

/// Check if the SSH agent is available and accessible
pub async fn is_agent_available() -> bool {
    if get_agent_socket_path().is_err() {
        return false;
    }

    tokio::task::spawn_blocking(|| connect_to_agent().is_ok())
        .await
        .unwrap_or(false)
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
