// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Error types for triton-auth

use thiserror::Error;

/// Errors that can occur during authentication operations
#[derive(Error, Debug)]
pub enum AuthError {
    /// Failed to load an SSH key from file
    #[error("Failed to load key: {0}")]
    KeyLoadError(String),

    /// Key with the specified fingerprint was not found
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// Error communicating with SSH agent
    #[error("SSH agent error: {0}")]
    AgentError(String),

    /// Error during cryptographic signing
    #[error("Signing error: {0}")]
    SigningError(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// SSH key encoding/decoding error
    #[error("SSH key error: {0}")]
    SshKeyError(#[from] ssh_key::Error),
}
