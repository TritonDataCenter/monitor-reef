// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The access-token body. These claims are denormalized on purpose
//! (RFD 00004 §"Token shape"): a verifier reconstructs a principal with
//! no store round-trip, so this crate has no `identity-store`, `tritond`,
//! or FoundationDB dependency.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which kind of realm minted this token. Mirrors
/// `identity-store::RealmScope` but is duplicated here so the verifier
/// stays dependency-free. `Unknown` keeps deserialization
/// forward-compatible (Type-Safety Rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RealmScope {
    Tenant,
    Silo,
    System,
    #[serde(other)]
    Unknown,
}

/// RFC 7800 confirmation claim. For workload tokens, `cn` binds the
/// token to a specific compute node (`OAuthClient.bound_to_cn`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Confirmation {
    #[serde(rename = "cn", default, skip_serializing_if = "Option::is_none")]
    pub cn: Option<Uuid>,
}

/// Verified claims from an identityd access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    /// Subject: the user (or workload client) id.
    pub sub: Uuid,
    /// Issuer URL: the realm's `issuer_url`.
    pub iss: String,
    /// Audience, when scoped to a specific resource server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    /// Expiry (unix seconds).
    pub exp: i64,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Not-before (unix seconds), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<i64>,

    /// Realm id that minted the token.
    pub realm: Uuid,
    /// Realm scope (tenant / silo / system).
    pub realm_scope: RealmScope,
    /// The one tenant this token is valid for (RFD 00004 decision 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    /// The owning silo, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silo_id: Option<Uuid>,
    /// Root operator.
    #[serde(default)]
    pub is_root: bool,
    /// Fleet administrator.
    #[serde(default)]
    pub fleet_admin: bool,
    /// Group memberships (names).
    #[serde(default)]
    pub groups: Vec<String>,
    /// OAuth scope string (space-delimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Confirmation claim (workload CN binding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnf: Option<Confirmation>,
}

impl AccessClaims {
    /// Iterate the space-delimited OAuth scopes.
    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        self.scope.as_deref().unwrap_or("").split_whitespace()
    }

    /// True if `scope` grants `wanted`.
    pub fn has_scope(&self, wanted: &str) -> bool {
        self.scopes().any(|s| s == wanted)
    }
}
