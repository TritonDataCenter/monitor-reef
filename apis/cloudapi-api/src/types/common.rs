// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types used across CloudAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// UUID type
pub type Uuid = uuid::Uuid;

/// CloudAPI error response
///
/// This matches the actual error format returned by CloudAPI, which differs
/// from Dropshot's default error format. CloudAPI uses `code` instead of
/// `error_code` and `request_id` is optional.
///
/// Note: Named `ErrorResponse` rather than `Error` to distinguish this DTO
/// from Rust error types.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    /// Error code (e.g., "InvalidCredentials", "ResourceNotFound")
    pub code: String,
    /// Human-readable error message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Request ID for tracing (optional, not always present)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// RFC3339 timestamp
pub type Timestamp = String;

/// Key-value tags (values can be strings, booleans, or numbers)
pub type Tags = HashMap<String, Value>;

/// Key-value metadata (values can be strings, booleans, or numbers)
pub type Metadata = vmapi_api::MetadataObject;

/// Role tags for RBAC
pub type RoleTags = Vec<String>;

/// VM/Container brand for CloudAPI provisioning requests.
///
/// The brand determines the virtualization/containerization technology used.
/// Valid brands as defined in `lib/machines.js`:
/// - `bhyve`: FreeBSD hypervisor for hardware VMs
/// - `joyent`: Native SmartOS zone
/// - `joyent-minimal`: Minimal SmartOS zone
/// - `kvm`: KVM hardware VM
/// - `lx`: Linux-branded zone (Linux containers)
///
// NOTE: This enum intentionally differs from vmapi_api::Brand.
// CloudAPI's Brand restricts what brands can be specified in provisioning
// requests (CreateMachineRequest). VMAPI's Brand includes additional
// internal-only brands like "builder" that exist in the system but cannot
// be provisioned via CloudAPI. Output types (Machine, Package) and query
// filters (ListMachinesQuery) use vmapi_api::Brand to accurately represent
// and filter by any brand that may exist in the system.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Brand {
    Bhyve,
    Joyent,
    #[serde(rename = "joyent-minimal")]
    JoyentMinimal,
    Kvm,
    Lx,
}

/// Path parameter for account
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountPath {
    /// Account login name
    pub account: String,
}

/// Path parameter for datacenter
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatacenterPath {
    /// Account login name
    pub account: String,
    /// Datacenter name
    pub dc: String,
}
