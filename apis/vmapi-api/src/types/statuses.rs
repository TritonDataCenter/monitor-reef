// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Status-related types for VMAPI
//!
//! Note: VMAPI uses snake_case for JSON field names (internal Triton API convention).

use super::common::{Uuid, VmState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Query Parameters
// ============================================================================

/// Query parameters for getting statuses
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetStatusesQuery {
    /// Comma-separated list of VM UUIDs to get status for
    pub uuids: String,
}

// ============================================================================
// Response Types
// ============================================================================

/// Status entry for a single VM
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VmStatus {
    /// VM state
    pub state: VmState,
    /// Last modified timestamp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// Response mapping VM UUIDs to their statuses.
///
/// Newtype wrapper rather than a type alias so the generated OpenAPI
/// spec carries `StatusesResponse` as a named schema, giving
/// downstream clients a named type for the VM-statuses endpoint's
/// response rather than an anonymous map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct StatusesResponse(pub HashMap<Uuid, VmStatus>);

impl std::ops::Deref for StatusesResponse {
    type Target = HashMap<Uuid, VmStatus>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for StatusesResponse {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<HashMap<Uuid, VmStatus>> for StatusesResponse {
    fn from(m: HashMap<Uuid, VmStatus>) -> Self {
        StatusesResponse(m)
    }
}

impl FromIterator<(Uuid, VmStatus)> for StatusesResponse {
    fn from_iter<I: IntoIterator<Item = (Uuid, VmStatus)>>(iter: I) -> Self {
        StatusesResponse(iter.into_iter().collect())
    }
}
