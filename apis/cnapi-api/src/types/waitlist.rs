// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::Uuid;

/// Path parameter for ticket endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TicketPath {
    pub ticket_uuid: Uuid,
}

/// Ticket status
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TicketStatus {
    Queued,
    Active,
    Expired,
    Finished,
    /// Forward-compatible catch-all
    #[serde(other)]
    Unknown,
}

/// Waitlist ticket
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Ticket {
    pub uuid: Uuid,
    pub server_uuid: Uuid,
    pub scope: String,
    pub id: String,
    #[serde(default)]
    pub status: Option<TicketStatus>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Body for POST /servers/:server_uuid/tickets (create)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitlistCreateParams {
    pub scope: String,
    pub id: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

/// Body for PUT /tickets/:ticket_uuid (update)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitlistUpdateParams {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Query parameters for GET /tickets/:ticket_uuid/wait
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WaitlistWaitParams {
    /// Timeout in seconds
    #[serde(default)]
    pub timeout: Option<u64>,
}
