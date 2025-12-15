// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Common types used across CloudAPI

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// UUID type
pub type Uuid = String;

/// RFC3339 timestamp
pub type Timestamp = String;

/// Key-value tags (values can be strings, booleans, or numbers)
pub type Tags = HashMap<String, Value>;

/// Key-value metadata
pub type Metadata = HashMap<String, String>;

/// Role tags for RBAC
pub type RoleTags = Vec<String>;

/// VM/Container brand
///
/// The brand determines the virtualization/containerization technology used.
/// Valid brands as defined in `lib/machines.js`:
/// - `bhyve`: FreeBSD hypervisor for hardware VMs
/// - `joyent`: Native SmartOS zone
/// - `joyent-minimal`: Minimal SmartOS zone
/// - `kvm`: KVM hardware VM
/// - `lx`: Linux-branded zone (Linux containers)
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
