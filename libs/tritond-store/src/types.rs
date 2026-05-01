// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Domain types shared between the storage layer and the wire surface.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A tenancy boundary. Every Triton Cloud resource ultimately rolls up
/// to a silo; in production each silo is bound to its own identity
/// provider. Phase 0 carries only the bare-minimum identifying fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Silo {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for creating a silo.
///
/// Distinct from [`Silo`] because the server assigns `id` and
/// `created_at`. `description` is optional on the wire and stored as
/// an empty string when omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewSilo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}
