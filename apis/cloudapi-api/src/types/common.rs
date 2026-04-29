// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Common types used across CloudAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// UUID type
pub type Uuid = vmapi_api::Uuid;

/// Standard Triton/restify error codes.
///
/// These CamelCase codes match the `code` field in CloudAPI JSON error
/// responses.  The list covers the stock restify error codes plus
/// CloudAPI-specific additions.  The `Unknown` variant ensures forward
/// compatibility when the server introduces new codes.
// Implementation note: Unknown uses #[serde(other)] for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ErrorCode {
    BadDigest,
    BadMethod,
    ConnectTimeout,
    InternalError,
    InvalidArgument,
    InvalidContent,
    InvalidCredentials,
    InvalidHeader,
    InvalidParameters,
    InvalidVersion,
    MissingParameter,
    NotAuthorized,
    PreconditionFailed,
    RequestExpired,
    RequestThrottled,
    ResourceNotFound,
    ServiceUnavailable,
    ValidationFailed,
    /// Catch-all for unrecognised error codes from the server.
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("Unknown"),
            other => {
                // Serialize to get the exact CamelCase wire name.
                // All named variants serialize as bare strings so
                // serde_json::to_string wraps in quotes; strip them.
                let s = serde_json::to_string(other).unwrap_or_default();
                f.write_str(s.trim_matches('"'))
            }
        }
    }
}

/// CloudAPI error response
///
/// This matches the actual error format returned by CloudAPI. CloudAPI uses
/// `code` instead of `error_code` and `request_id` is optional.
// Note: Named ErrorResponse rather than Error to distinguish this DTO
// from Rust error types.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    /// Error code
    pub code: ErrorCode,
    /// Human-readable error message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Request ID for tracing (optional, not always present)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// RFC3339 timestamp
pub type Timestamp = vmapi_api::Timestamp;

/// Key-value tags (values can be strings, booleans, or numbers)
pub use vmapi_api::Tags;

/// Key-value metadata (values can be strings, booleans, or numbers)
pub use vmapi_api::MetadataObject as Metadata;

/// Role tags for RBAC
///
/// Newtype wrapper rather than a type alias so the generated OpenAPI
/// spec carries `RoleTags` as a named schema. Every RBAC-taggable
/// resource (FirewallRule, Image, Machine, Network, Package, Policy,
/// Role, SshKey, User, plus the bulk replace-role-tags request/
/// response) carries a `role-tag` field — with an alias, each was
/// inlined as an anonymous `Vec<String>` per field. The newtype
/// preserves the name so all clients see a single `RoleTags` type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleTags(pub Vec<String>);

impl std::ops::Deref for RoleTags {
    type Target = Vec<String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RoleTags {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vec<String>> for RoleTags {
    fn from(v: Vec<String>) -> Self {
        RoleTags(v)
    }
}

impl<S: Into<String>> FromIterator<S> for RoleTags {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        RoleTags(iter.into_iter().map(Into::into).collect())
    }
}

impl IntoIterator for RoleTags {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a RoleTags {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl PartialEq<Vec<String>> for RoleTags {
    fn eq(&self, other: &Vec<String>) -> bool {
        &self.0 == other
    }
}

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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
