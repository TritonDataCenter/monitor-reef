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
pub type Timestamp = chrono::DateTime<chrono::Utc>;

/// Key-value metadata (values can be strings, booleans, or numbers)
///
/// Newtype wrapper rather than a type alias so the generated OpenAPI
/// spec carries `MetadataObject` as a named schema rather than an
/// anonymous `additionalProperties` object. See the note on `Tags`
/// below for the full rationale.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MetadataObject(pub HashMap<String, Value>);

impl std::ops::Deref for MetadataObject {
    type Target = HashMap<String, Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for MetadataObject {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for MetadataObject {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        MetadataObject(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl From<HashMap<String, Value>> for MetadataObject {
    fn from(map: HashMap<String, Value>) -> Self {
        MetadataObject(map)
    }
}

impl From<serde_json::Map<String, Value>> for MetadataObject {
    fn from(map: serde_json::Map<String, Value>) -> Self {
        MetadataObject(map.into_iter().collect())
    }
}

/// Key-value tags (values can be strings, booleans, or numbers)
///
/// Newtype wrapper rather than a type alias so the generated OpenAPI
/// spec carries `Tags` as a named schema rather than an anonymous
/// `additionalProperties` object. A `pub type Tags = HashMap<String, Value>`
/// is erased by schemars at compile time, causing every field typed
/// `Tags` to inline the map shape and downstream code generators
/// (Progenitor, oapi-codegen) to emit unnamed `serde_json::Map` /
/// `map[string]interface{}` types per field. The newtype preserves
/// the name so all clients see a single `Tags` type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Tags(pub HashMap<String, Value>);

impl std::ops::Deref for Tags {
    type Target = HashMap<String, Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Tags {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Tags {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Tags(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl From<HashMap<String, Value>> for Tags {
    fn from(map: HashMap<String, Value>) -> Self {
        Tags(map)
    }
}

impl From<serde_json::Map<String, Value>> for Tags {
    fn from(map: serde_json::Map<String, Value>) -> Self {
        Tags(map.into_iter().collect())
    }
}

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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[schemars(rename = "VmBrand")]
pub enum Brand {
    Bhyve,
    /// Internal brand for image build zones (not provisionable via CloudAPI)
    Builder,
    Joyent,
    #[serde(rename = "joyent-minimal")]
    JoyentMinimal,
    Kvm,
    Lx,
    /// Unknown brand (forward compatibility)
    #[serde(other)]
    Unknown,
}

/// VM state
///
/// These states reflect the possible values returned by VMAPI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
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
    /// Unknown state (forward compatibility)
    #[serde(other)]
    Unknown,
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
            "unknown" => Ok(VmState::Unknown),
            _ => Ok(VmState::Unknown),
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
