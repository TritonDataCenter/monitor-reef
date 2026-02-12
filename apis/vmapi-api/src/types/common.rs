// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types used across VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// UUID type
pub type Uuid = uuid::Uuid;

/// RFC3339 timestamp
pub type Timestamp = String;

/// Key-value metadata (values can be strings, booleans, or numbers)
pub type MetadataObject = HashMap<String, Value>;

/// Key-value tags (values can be strings, booleans, or numbers)
pub type Tags = HashMap<String, Value>;

/// VM/Container brand as returned by VMAPI.
///
/// The brand determines the virtualization/containerization technology used.
/// - `bhyve`: FreeBSD hypervisor for hardware VMs
/// - `builder`: Internal brand for image build zones (not provisionable via CloudAPI)
/// - `joyent`: Native SmartOS zone
/// - `joyent-minimal`: Minimal SmartOS zone
/// - `kvm`: KVM hardware VM
/// - `lx`: Linux-branded zone (Linux containers)
///
// NOTE: This enum represents all brands that can exist in the system.
// CloudAPI has a separate, more restrictive Brand enum that only includes
// brands that can be specified in provisioning requests. This enum includes
// internal-only brands like "builder" that exist in SmartOS but are not
// exposed for public provisioning. CloudAPI's output types (Machine, Package)
// use this enum to accurately represent VM state.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Brand {
    Bhyve,
    /// Internal brand for image build zones (not provisionable via CloudAPI)
    Builder,
    Joyent,
    #[serde(rename = "joyent-minimal")]
    JoyentMinimal,
    Kvm,
    Lx,
}

/// VM state
///
/// These states reflect the possible values returned by VMAPI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Running,
    Stopped,
    Stopping,
    Provisioning,
    Failed,
    Destroyed,
    Incomplete,
    Configured,
    Ready,
    Receiving,
}

impl VmState {
    /// Check if this state represents a failed VM.
    pub fn is_failed(&self) -> bool {
        matches!(self, VmState::Failed)
    }

    /// Check if this state represents a destroyed VM.
    pub fn is_destroyed(&self) -> bool {
        matches!(self, VmState::Destroyed)
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            VmState::Running => "running",
            VmState::Stopped => "stopped",
            VmState::Stopping => "stopping",
            VmState::Provisioning => "provisioning",
            VmState::Failed => "failed",
            VmState::Destroyed => "destroyed",
            VmState::Incomplete => "incomplete",
            VmState::Configured => "configured",
            VmState::Ready => "ready",
            VmState::Receiving => "receiving",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for VmState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "running" => Ok(VmState::Running),
            "stopped" => Ok(VmState::Stopped),
            "stopping" => Ok(VmState::Stopping),
            "provisioning" => Ok(VmState::Provisioning),
            "failed" => Ok(VmState::Failed),
            "destroyed" => Ok(VmState::Destroyed),
            "incomplete" => Ok(VmState::Incomplete),
            "configured" => Ok(VmState::Configured),
            "ready" => Ok(VmState::Ready),
            "receiving" => Ok(VmState::Receiving),
            _ => Err(format!("unknown VM state: {}", s)),
        }
    }
}

/// Ping response
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingResponse {
    /// Ping status (e.g., "OK")
    pub status: String,
    /// Health check status details
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
    /// Backend services health status
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_status: Option<String>,
}
